//! The web UI end to end: `macron up` as a process, driven over HTTP the way its pages drive it,
//! against the sandbox and the stand-in `launchctl`.

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use support::{Response, Sandbox, http, label};

/// `macron up` running in a sandbox, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
    cookie: String,
    up_file: PathBuf,
}

impl Server {
    fn start(sandbox: &Sandbox) -> Self {
        let child = sandbox
            .command(&["up", "--no-open", "--port", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let state = sandbox.home().parent().unwrap().join("state");
        let up_file = state.join("up.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        let port = loop {
            if let Ok(text) = fs::read_to_string(&up_file)
                && let Ok(up) = serde_json::from_str::<serde_json::Value>(&text)
            {
                break up["port"].as_u64().unwrap() as u16;
            }
            assert!(Instant::now() < deadline, "macron up did not start");
            thread::sleep(Duration::from_millis(50));
        };
        let token = fs::read_to_string(state.join("web-token")).unwrap();
        Self {
            child,
            port,
            cookie: format!("macron-session={}", token.trim()),
            up_file,
        }
    }

    fn get(&self, path: &str) -> Response {
        http(self.port, "GET", path, Some(&self.cookie), None)
    }

    fn post(&self, path: &str, form: &str) -> Response {
        http(self.port, "POST", path, Some(&self.cookie), Some(form))
    }

    /// Stops the server as Ctrl-C would, and returns whether it exited cleanly.
    fn stop(mut self) -> bool {
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGINT).unwrap();
        self.child.wait().unwrap().success()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn redirect(response: &Response) -> &str {
    assert_eq!(response.status, 303, "{}", response.body);
    response.header("location").unwrap()
}

#[test]
fn up_signs_a_browser_in_and_stops_cleanly() {
    let sandbox = Sandbox::new();
    let server = Server::start(&sandbox);

    assert_eq!(http(server.port, "GET", "/", None, None).status, 401);
    let token = server.cookie.trim_start_matches("macron-session=");
    let signed_in = http(
        server.port,
        "GET",
        &format!("/auth?token={token}"),
        None,
        None,
    );
    assert_eq!(redirect(&signed_in), "/");
    assert!(
        signed_in
            .header("set-cookie")
            .unwrap()
            .starts_with(&server.cookie)
    );
    assert!(server.get("/").body.contains("No jobs yet"));

    let again = sandbox.run(&["up", "--no-open"]).ok();
    assert_eq!(
        again.stdout,
        format!("macron is already up at http://127.0.0.1:{}\n", server.port)
    );

    let up_file = server.up_file.clone();
    assert!(server.stop(), "macron up should exit cleanly on Ctrl-C");
    assert!(!up_file.exists(), "up.json should be removed");
}

#[test]
fn a_job_is_managed_from_its_pages() {
    let sandbox = Sandbox::new();
    let server = Server::start(&sandbox);

    let created = server.post(
        "/jobs",
        "name=greet&schedule=0+9+*+*+*&command=echo+hello&shell=%2Fbin%2Fsh&description=Says+hello&enabled=on",
    );
    assert_eq!(redirect(&created), "/jobs/greet");
    assert!(sandbox.jobs_toml().contains("[jobs.greet]"));
    assert_eq!(sandbox.loaded(), [label("greet")]);

    let page = server.get("/jobs/greet");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Says hello") && page.body.contains("at 09:00 every day"));
    assert!(server.get("/").body.contains("/jobs/greet"));

    let edited = server.post(
        "/jobs/greet",
        "schedule=%40every+15m&command=echo+hello&shell=%2Fbin%2Fsh&description=Says+hello&enabled=on",
    );
    assert_eq!(redirect(&edited), "/jobs/greet");
    assert!(sandbox.jobs_toml().contains("schedule = \"@every 15m\""));
    assert!(
        sandbox
            .plist("greet")
            .unwrap()
            .contains("<key>StartInterval</key>")
    );

    let ran = server.post("/jobs/greet/run", "");
    let run_page = redirect(&ran).to_owned();
    assert_eq!(run_page, "/jobs/greet/runs/1");
    let events = server.get("/jobs/greet/runs/1/events");
    assert_eq!(events.header("content-type"), Some("text/event-stream"));
    assert!(events.body.contains("event: line"), "{}", events.body);
    assert!(
        events.body.contains("\"text\":\"hello\""),
        "{}",
        events.body
    );
    assert!(
        events.body.contains("event: end\ndata: Succeeded in"),
        "{}",
        events.body
    );
    let run = server.get(&run_page);
    assert!(run.body.contains("hello") && run.body.contains("succeeded"));

    assert_eq!(
        redirect(&server.post("/jobs/greet/disable", "back=%2F")),
        "/"
    );
    assert!(sandbox.loaded().is_empty());
    assert_eq!(
        redirect(&server.post("/jobs/greet/enable", "")),
        "/jobs/greet"
    );
    assert_eq!(sandbox.loaded(), [label("greet")]);

    let confirm = server.get("/jobs/greet/delete");
    assert!(confirm.body.contains("Also delete its 1 recorded run"));
    assert_eq!(
        redirect(&server.post("/jobs/greet/delete", "purge=on")),
        "/"
    );
    assert!(!sandbox.jobs_toml().contains("greet"));
    assert!(sandbox.loaded().is_empty());
    assert_eq!(server.get("/jobs/greet").status, 404);
}

#[test]
fn an_invalid_form_comes_back_with_its_errors() {
    let sandbox = Sandbox::new();
    let server = Server::start(&sandbox);

    let refused = server.post("/jobs", "name=Bad+Name&schedule=0+25+*+*+*&command=true");
    assert_eq!(refused.status, 400);
    assert!(
        refused.body.contains("value=\"Bad Name\""),
        "keeps what was typed"
    );
    assert!(refused.body.contains("use lowercase letters"));

    let refused = server.post("/jobs", "name=good&schedule=0+25+*+*+*&command=true");
    assert_eq!(refused.status, 400);
    assert!(refused.body.contains("is out of range; use 0–23"));
    assert_eq!(sandbox.jobs_toml(), "");
}

#[test]
fn runs_are_listed_across_jobs() {
    let sandbox = Sandbox::new();
    for (name, command) in [("good", "true"), ("bad", "false")] {
        sandbox
            .run(&[
                "add", name, "-s", "@daily", "--shell", "/bin/sh", "--", command,
            ])
            .ok();
        sandbox.run(&["exec", name]);
    }
    let server = Server::start(&sandbox);
    let all = server.get("/runs");
    assert!(all.body.contains("/jobs/good/runs/1") && all.body.contains("/jobs/bad/runs/2"));
    let failed = server.get("/runs?status=failed");
    assert!(failed.body.contains("/jobs/bad/runs/2") && !failed.body.contains("/jobs/good/runs/1"));
}
