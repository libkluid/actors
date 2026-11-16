//! End-to-end tests for the demo binary: exit codes and stderr chains.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_actors-demo"))
        .args(args)
        .env("RUST_BACKTRACE", "0")
        .output()
        .expect("could not execute actors-demo")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn concurrent_producers_are_serialized() {
    let output = run(&[
        "run",
        "--producers",
        "4",
        "--per-producer",
        "1000",
        "--capacity",
        "64",
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    assert!(stdout.contains("producers: 4 x 1000"), "{stdout}");
    assert!(stdout.contains("counter:   4000"), "{stdout}");
}

#[test]
fn domain_error_uses_ex_dataerr() {
    let output = run(&["fail"]);

    assert_eq!(output.status.code(), Some(65));
    let stderr = stderr(&output);
    assert!(stderr.contains("counter overflow: 1 +"), "{stderr}");
}

#[test]
fn timeout_uses_ex_tempfail() {
    let output = run(&["timeout"]);

    assert_eq!(output.status.code(), Some(75));
    let stderr = stderr(&output);
    assert!(stderr.contains("did not send a reply within"), "{stderr}");
}

#[test]
fn overload_uses_ex_tempfail() {
    let output = run(&["saturate"]);

    assert_eq!(output.status.code(), Some(75));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("mailbox for actor `chaos` is full"),
        "{stderr}"
    );
}

#[test]
fn stopped_actor_uses_ex_unavailable() {
    let output = run(&["stopped"]);

    assert_eq!(output.status.code(), Some(69));
    let stderr = stderr(&output);
    assert!(stderr.contains("actor `chaos` is not running"), "{stderr}");
}

#[test]
fn panic_uses_ex_software_and_is_reported_once() {
    let output = run(&["panic"]);

    assert_eq!(output.status.code(), Some(70));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("actor `chaos` panicked: chaos actor was asked to panic"),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("panicked").count(),
        1,
        "the panic must be reported once: {stderr}"
    );
}

#[test]
fn missing_command_is_usage_error() {
    let output = run(&[]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.contains("no command given"), "{stderr}");
    assert!(stderr.contains("usage: actors-demo"), "{stderr}");
}

#[test]
fn zero_capacity_is_usage_error() {
    let output = run(&["run", "--capacity", "0"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.contains("must be greater than zero"), "{stderr}");
}

#[test]
fn unknown_command_is_usage_error() {
    let output = run(&["explode"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    assert!(stderr.contains("unknown command `explode`"), "{stderr}");
}
