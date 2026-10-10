//! macron schedules, runs and records local jobs on macOS through launchd.
//!
//! The binary is a thin shell over this library. The CLI and the web UI call the same functions, so
//! what one can do the other can, and they cannot disagree about what happened.

pub mod cli;
pub mod config;
pub mod duration;
pub mod error;
pub mod exec;
pub mod history;
pub mod human;
pub mod job;
pub mod launchd;
pub mod notify;
pub mod ops;
pub mod paths;
pub mod runlog;
pub mod schedule;
pub mod time;
