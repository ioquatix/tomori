// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Connect to a Tomori server and print what it sends.
//!
//! ```text
//! cargo run -p client --example connect -- 192.168.1.10:8765 [name]
//! ```
//!
//! Type commands on standard input: `list`, `join <space>`, `leave`, `gate off|live|agent`, `quit`.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

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

fn main() -> std::io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let Some(address) = arguments.next() else {
        eprintln!("usage: connect <host[:port]> [name]");
        std::process::exit(2);
    };
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
