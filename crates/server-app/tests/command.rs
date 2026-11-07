// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The `tomori-server` executable, Tomori's own server program, run as a separate process. Every server here listens on an ephemeral loopback port and does not advertise.
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SERVER: &str = env!("CARGO_BIN_EXE_tomori-server");

/// An empty directory of its own for the test named `name`.
fn directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "tomori-server-command-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn help_and_unknown_arguments_fail_with_the_usage() {
    for argument in ["--help", "--verbose"] {
        let output = Command::new(SERVER).arg(argument).output().unwrap();
        assert!(!output.status.success(), "{argument}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("Usage: tomori-server"), "{error}");
    }
}

#[test]
fn the_auth_command_manages_tokens_in_the_state_directory() {
    let directory = directory("auth");
    let state = directory.to_str().unwrap();
    let output = Command::new(SERVER)
        .args([
            "auth",
            "mint",
            "--owner",
            "--name",
            "Ada",
            "--state-directory",
            state,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let listed = Command::new(SERVER)
        .args(["auth", "list", "--state-directory", state])
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8(listed.stdout).unwrap().contains("Ada"));
    let refused = Command::new(SERVER)
        .args(["auth", "revoke", "--state-directory", state])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn sigterm_shuts_the_server_down_in_order() {
    let directory = directory("sigterm");
    let state = directory.to_str().unwrap();
    let mut server = Command::new(SERVER)
        .args([
            "--bind",
            "127.0.0.1:0",
            "--no-advertise",
            "--auth",
            "open",
            "--state-directory",
            state,
            "--workspace-root",
            state,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(server.stderr.take().unwrap()).lines();
    let listening = lines
        .by_ref()
        .map(Result::unwrap)
        .find(|line| line.starts_with("Tomori server listening on http://127.0.0.1:"))
        .expect("the server says where it listens");
    let address = listening
        .trim_start_matches("Tomori server listening on http://")
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    // It serves: a client is welcomed, and offered Tomori's own environments, Sky first, then the grid and the farmyard.
    let mut client = tomori_server::test_support::TestClient::open(address);
    let offered: Vec<(String, String, u32)> = client
        .hello_presenting("quest", "Ada", None)
        .into_iter()
        .map(|environment| (environment.id, environment.label, environment.version))
        .collect();
    assert_eq!(
        offered,
        [
            ("sky".to_owned(), "Sky".to_owned(), 1),
            ("minimal".to_owned(), "Grid".to_owned(), 1),
            ("farmyard".to_owned(), "Farmyard".to_owned(), 1)
        ]
    );
    drop(client);

    // SAFETY: `kill` only sends a signal to the child.
    assert_eq!(
        unsafe { libc::kill(server.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let rest: Vec<String> = lines.map(Result::unwrap).collect();
    assert!(
        rest.iter()
            .any(|line| line == "Shutting down; signal again to exit immediately."),
        "{rest:?}"
    );
    let status = server.wait().unwrap();
    assert!(status.success(), "{status}");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_server_that_cannot_start_fails() {
    let directory = directory("unstartable");
    let state = directory.to_str().unwrap();
    let output = Command::new(SERVER)
        .args([
            "--bind",
            "192.0.2.1:0",
            "--no-advertise",
            "--auth",
            "open",
            "--state-directory",
            state,
            "--workspace-root",
            state,
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("Could not listen on 192.0.2.1:0"), "{error}");
    std::fs::remove_dir_all(directory).unwrap();
}

/// The operator answers pairings on the console of a server that admits by tokens.
#[cfg(unix)]
#[test]
fn the_console_answers_the_operator() {
    let directory = directory("console");
    let state = directory.to_str().unwrap();
    let mut server = Command::new(SERVER)
        .args([
            "--bind",
            "127.0.0.1:0",
            "--no-advertise",
            "--state-directory",
            state,
            "--workspace-root",
            state,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut console = server.stdin.take().unwrap();
    let mut lines = BufReader::new(server.stderr.take().unwrap())
        .lines()
        .map(Result::unwrap);
    // Wait for each line that begins with `start`, passing over others.
    let mut said = |start: &str| {
        lines
            .by_ref()
            .find(|said| said.starts_with(start))
            .unwrap_or_else(|| panic!("the server never said {start:?}"))
    };
    said("No owner token is issued yet: run `tomori-server auth mint --owner --name NAME`.");
    // Once it listens, it also handles signals.
    said("Tomori server listening on https://127.0.0.1:");
    writeln!(console, "decline").unwrap();
    said("No pairing is waiting.");
    writeln!(console, "allow").unwrap();
    said("Pairing is allowed again.");

    // SAFETY: `kill` only sends a signal to the child.
    assert_eq!(
        unsafe { libc::kill(server.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    said("Shutting down; signal again to exit immediately.");
    assert!(server.wait().unwrap().success());
    std::fs::remove_dir_all(directory).unwrap();
}
