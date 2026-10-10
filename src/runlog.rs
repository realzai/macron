//! Run output: one JSON Lines file per run, at `logs/<job>/<run id>.jsonl`.
//!
//! Each line of output is one record, `{"t":<milliseconds since the epoch>,"s":"<stream>","l":"<text>"}`,
//! in the order macron read it, so the CLI, the web UI and anyone reading the file see the same
//! thing. The stream is `out` or `err` for the command's stdout and stderr, and `macron` for
//! macron's own notes on the run: that it could not start, reached its timeout, or stopped
//! recording output.
//!
//! stdout and stderr arrive through separate pipes, so the order between them is the order macron
//! read them in. That matches the order they were written, except for lines written to both within
//! the same instant.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// How much of a run's output is recorded. Past this, lines are dropped and the log says so.
pub const MAX_OUTPUT_BYTES: u64 = 10 * 1024 * 1024;

/// The longest line recorded as one. A longer one, or output with no newlines at all, is split.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Out,
    Err,
    Macron,
}

/// One line of a run's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    #[serde(rename = "t")]
    pub time: i64,
    #[serde(rename = "s")]
    pub stream: Stream,
    #[serde(rename = "l")]
    pub text: String,
}

/// Where the log of run `run` of `job` is.
pub fn path(paths: &Paths, job: &str, run: i64) -> PathBuf {
    paths.logs_dir.join(job).join(format!("{run}.jsonl"))
}

/// Writes a run's log, a whole line per write, so a reader never sees half a line unless it reads
/// while that one write is under way.
pub struct Writer {
    file: File,
    limit: u64,
    bytes: u64,
    truncated: bool,
}

impl Writer {
    pub fn create(path: &Path) -> Result<Self> {
        Self::with_limit(path, MAX_OUTPUT_BYTES)
    }

    fn with_limit(path: &Path, limit: u64) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
        }
        let file = File::create(path).map_err(Error::io("create", path))?;
        Ok(Self {
            file,
            limit,
            bytes: 0,
            truncated: false,
        })
    }

    /// Records a line of the command's output. Once the output passes the limit, the rest is
    /// dropped, with a note saying so.
    pub fn output(&mut self, stream: Stream, text: &str, time: Timestamp) -> io::Result<()> {
        if self.truncated {
            return Ok(());
        }
        let size = text.len() as u64;
        if self.bytes + size > self.limit {
            self.truncated = true;
            let limit = self.limit / (1024 * 1024);
            return self.note(
                &format!("stopped recording output after {limit} MiB; the rest was discarded"),
                time,
            );
        }
        self.bytes += size;
        self.write(stream, text, time)
    }

    /// Records a note from macron about the run.
    pub fn note(&mut self, text: &str, time: Timestamp) -> io::Result<()> {
        self.write(Stream::Macron, text, time)
    }

    /// How many bytes of output were recorded.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    fn write(&mut self, stream: Stream, text: &str, time: Timestamp) -> io::Result<()> {
        let line = Line {
            time: time.as_millisecond(),
            stream,
            text: text.to_owned(),
        };
        let mut record = serde_json::to_vec(&line).map_err(io::Error::other)?;
        record.push(b'\n');
        self.file.write_all(&record)
    }
}

/// Reads the complete lines of the log at `path` from byte `offset` on, and returns them with the
/// offset after the last one. A line still being written is left for the next read. A log that
/// does not exist yet has no lines.
pub fn read(path: &Path, offset: u64) -> Result<(Vec<Line>, u64)> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), offset)),
        Err(err) => return Err(Error::io("read", path)(err)),
    };
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_to_end(&mut bytes))
        .map_err(Error::io("read", path))?;
    let Some(end) = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map(|last| last + 1)
    else {
        return Ok((Vec::new(), offset));
    };
    let lines = bytes[..end]
        .split(|&byte| byte == b'\n')
        .filter(|record| !record.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<Vec<Line>, _>>()
        .map_err(|err| Error::InvalidLog {
            path: path.to_owned(),
            reason: err.to_string(),
        })?;
    Ok((lines, offset + end as u64))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).unwrap()
    }

    #[test]
    fn writes_one_record_per_line() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("logs/backup/1.jsonl");
        let mut writer = Writer::create(&path).unwrap();
        writer
            .output(Stream::Out, "hello \"world\"", at(1))
            .unwrap();
        writer.output(Stream::Err, "oops", at(2)).unwrap();
        writer.note("stopped", at(3)).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\"t\":1000,\"s\":\"out\",\"l\":\"hello \\\"world\\\"\"}\n\
             {\"t\":2000,\"s\":\"err\",\"l\":\"oops\"}\n\
             {\"t\":3000,\"s\":\"macron\",\"l\":\"stopped\"}\n"
        );
        assert_eq!(writer.bytes(), 17);
    }

    #[test]
    fn stops_recording_output_past_the_limit_and_says_so() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("1.jsonl");
        let mut writer = Writer::with_limit(&path, 3 * 1024 * 1024).unwrap();
        let mebibyte = "x".repeat(1024 * 1024);
        for _ in 0..5 {
            writer.output(Stream::Out, &mebibyte, at(1)).unwrap();
        }
        writer.note("finished", at(2)).unwrap();
        assert!(writer.truncated());
        assert_eq!(writer.bytes(), 3 * 1024 * 1024);
        let (lines, _) = read(&path, 0).unwrap();
        let streams: Vec<_> = lines.iter().map(|line| line.stream).collect();
        assert_eq!(
            streams,
            [
                Stream::Out,
                Stream::Out,
                Stream::Out,
                Stream::Macron,
                Stream::Macron
            ]
        );
        assert_eq!(
            lines[3].text,
            "stopped recording output after 3 MiB; the rest was discarded"
        );
    }

    #[test]
    fn reads_complete_lines_from_an_offset() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("1.jsonl");
        let mut writer = Writer::create(&path).unwrap();
        writer.output(Stream::Out, "one", at(1)).unwrap();
        let (lines, offset) = read(&path, 0).unwrap();
        assert_eq!(lines.len(), 1);

        writer.output(Stream::Out, "two", at(2)).unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"t\":3000,\"s\":\"out\",\"l\":\"thr")
            .unwrap();
        let (lines, offset) = read(&path, offset).unwrap();
        let texts: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["two"]);

        file.write_all(b"ee\"}\n").unwrap();
        let (lines, _) = read(&path, offset).unwrap();
        assert_eq!(lines[0].text, "three");
    }

    #[test]
    fn a_missing_log_has_no_lines_and_a_broken_one_is_an_error() {
        let dir = TempDir::new().unwrap();
        assert_eq!(read(&dir.path().join("none.jsonl"), 0).unwrap().0, []);
        let broken = dir.path().join("broken.jsonl");
        fs::write(&broken, "not json\n").unwrap();
        assert_eq!(read(&broken, 0).unwrap_err().code(), "invalid_log");
    }
}
