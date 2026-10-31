// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A stand-in for `claude -p --input-format stream-json --output-format stream-json` that replays a scenario.
//!
//! Usage: `fake-claude --scenario <path> --log <path> [claude arguments...]`. Every line read from standard input is
//! appended to the log. Scenario lines are commands:
//!
//! - `send <json>` writes a line to standard output, replacing `{{session}}`, `{{uuid}}` (of the last user message
//!   received) and `{{request}}` (the id of the last control request received).
//! - `expect <type>` or `expect control_request:<subtype>` reads standard input until a matching line arrives.
//! - `stderr <text>` writes a line to standard error.
//! - `drain` reads standard input until it closes.
//! - `hang` ignores standard input and never exits.
//! - `exit <code>` exits immediately.
//!
//! When standard input closes during `expect`, the fake exits successfully, as `claude` does.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::process::exit;
use std::thread;
use std::time::Duration;

use serde_json::Value;

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let option = |name: &str| {
        arguments
            .iter()
            .position(|argument| argument == name)
            .and_then(|index| arguments.get(index + 1))
            .cloned()
    };
    let scenario = fs::read_to_string(option("--scenario").expect("--scenario is required"))
        .expect("the scenario is readable");
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(option("--log").expect("--log is required"))
        .expect("the log is writable");
    let session = option("--session-id")
        .or_else(|| option("--resume"))
        .unwrap_or_default();

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = io::stdout();
    let mut uuid = String::new();
    let mut request = String::new();

    for command in scenario.lines() {
        let command = command.trim();
        if command.is_empty() || command.starts_with('#') {
            continue;
        }
        let (verb, rest) = command.split_once(' ').unwrap_or((command, ""));
        match verb {
            "send" => {
                let line = rest
                    .replace("{{session}}", &session)
                    .replace("{{uuid}}", &uuid)
                    .replace("{{request}}", &request);
                writeln!(stdout, "{line}").unwrap();
                stdout.flush().unwrap();
            }
            "stderr" => eprintln!("{rest}"),
            "expect" => {
                let (kind, subtype) = rest.split_once(':').unwrap_or((rest, ""));
                loop {
                    let Some(Ok(line)) = lines.next() else {
                        exit(0);
                    };
                    writeln!(log, "{line}").unwrap();
                    log.flush().unwrap();
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if value["type"] != kind {
                        continue;
                    }
                    if !subtype.is_empty() && value["request"]["subtype"] != subtype {
                        continue;
                    }
                    if let Some(id) = value["uuid"].as_str() {
                        uuid = id.into();
                    }
                    if let Some(id) = value["request_id"].as_str() {
                        request = id.into();
                    }
                    break;
                }
            }
            "drain" => {
                for line in lines.by_ref().map_while(Result::ok) {
                    writeln!(log, "{line}").unwrap();
                    log.flush().unwrap();
                }
                exit(0);
            }
            "hang" => loop {
                thread::sleep(Duration::from_secs(60));
            },
            "exit" => exit(rest.parse().expect("an exit code")),
            _ => panic!("unknown scenario command {verb:?}"),
        }
    }
}
