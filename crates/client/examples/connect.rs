// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Connect to a Tomori server and print what it sends.
//!
//! ```text
//! cargo run -p client --example connect -- 192.168.1.10:8765 [name]
//! cargo run -p client --example connect -- - [name]
//! cargo run -p client --example connect
//! ```
//!
//! Without an address, or with `-`, it browses for servers advertised with Bonjour, lists them and asks which one to join.
//!
//! Type commands on standard input: `list`, `join <space>`, `leave`, `gate off|live|agent`, `quit`.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use client::discovery::{self, Servers};
use client::{Client, Event, Options, Sender};
use protocol::{ClientMessage, Gate, ServerMessage};

fn command(sender: &Sender, line: &str) -> bool {
    let mut words = line.split_whitespace();
    let message = match (words.next(), words.next()) {
        (Some("quit"), _) => return false,
        (Some("list"), _) => ClientMessage::ListSpaces,
        (Some("join"), Some(space)) => ClientMessage::Join {
            space: space.into(),
        },
        (Some("leave"), _) => ClientMessage::Leave,
        (Some("gate"), Some(gate)) => ClientMessage::Gate {
            gate: match gate {
                "live" => Gate::Live,
                "agent" => Gate::Agent,
                _ => Gate::Off,
            },
        },
        (None, _) => return true,
        _ => {
            eprintln!("commands: list, join <space>, leave, gate off|live|agent, quit");
            return true;
        }
    };
    if let Err(error) = sender.send(message) {
        eprintln!("cannot send: {error}");
    }
    true
}

fn describe(message: &ServerMessage) -> String {
    match message {
        ServerMessage::Space { state } => format!(
            "space {} ({}) activity={:?} messages={} queue={} participants={}",
            state.id,
            state.title,
            state.activity,
            state.messages.len(),
            state.queue.len(),
            state.participants.len()
        ),
        other => format!("{other:?}"),
    }
}

/// How long to browse before listing servers.
const BROWSE_TIME: Duration = Duration::from_secs(3);

/// Browse for servers, list them and read the user's choice; `None` at the end of input.
fn choose() -> std::io::Result<Option<String>> {
    let browser = discovery::browse().map_err(std::io::Error::other)?;
    let mut servers = Servers::new();
    loop {
        eprintln!("Looking for Tomori servers...");
        let deadline = Instant::now() + BROWSE_TIME;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match browser.recv_timeout(remaining) {
                Ok(event) => {
                    servers.apply(event);
                }
                Err(_) => break,
            }
        }
        let compatible = servers.compatible();
        for (index, server) in compatible.iter().enumerate() {
            let address = server.address().unwrap_or_default();
            eprintln!("  {}. {} ({address})", index + 1, server.name);
        }
        for server in servers.incompatible() {
            let protocol = server
                .protocol
                .map_or_else(|| "unknown".to_owned(), |protocol| protocol.to_string());
            eprintln!(
                "  -  {} (protocol {protocol}; this client speaks {}, so it is unavailable)",
                server.name,
                protocol::PROTOCOL_VERSION
            );
        }
        if compatible.is_empty() {
            eprintln!(
                "No servers found. Press Enter to look again, or give an address: connect <host[:port]>."
            );
        } else {
            eprint!(
                "Choose a server [1-{}], or press Enter to look again: ",
                compatible.len()
            );
        }
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line.parse::<usize>() {
            Ok(choice) if (1..=compatible.len()).contains(&choice) => {
                return Ok(compatible[choice - 1].address());
            }
            _ => eprintln!("{line:?} is not in the list."),
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let address = match arguments.next() {
        Some(address) if address != "-" => address,
        _ => match choose()? {
            Some(address) => address,
            None => std::process::exit(1),
        },
    };
    eprintln!("Connecting to {address}.");
    let name = arguments.next().unwrap_or_else(|| "Console".into());
    let participant = format!("console-{}", std::process::id());
    let client = Client::connect(Options::new(address, participant, name))?;

    let sender = client.sender();
    let (quit, quitting) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if !command(&sender, &line) {
                break;
            }
        }
        let _ = quit.send(());
    });

    let mut audio: BTreeMap<u32, usize> = BTreeMap::new();
    let mut reported = Instant::now();
    loop {
        match client.events().recv_timeout(Duration::from_millis(100)) {
            Ok(Event::State(state)) => println!("state: {state:?}"),
            Ok(Event::Message(message)) => println!("message: {}", describe(&message)),
            Ok(Event::UtteranceFailed(failed)) => {
                println!("utterance {} failed: {}", failed.utterance, failed.failure)
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Some(frame) = client.try_audio() {
            *audio.entry(frame.source).or_default() += 1;
        }
        if reported.elapsed() >= Duration::from_secs(1) {
            if !audio.is_empty() {
                println!("audio frames per source: {audio:?}");
                audio.clear();
            }
            reported = Instant::now();
        }
        if quitting.try_recv().is_ok() {
            break;
        }
    }
    client.close();
    Ok(())
}
