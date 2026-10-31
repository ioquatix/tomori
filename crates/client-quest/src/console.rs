// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The panel in a terminal, for using a server without a headset or a GPU.
use crate::{caption::COLUMNS, connection::Connection, conversation::Conversation};
use anyhow::Result;
use std::{
    io::BufRead,
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

const HELP: &str = "Commands, one or more per line: u/d/l/r (left stick), < > (right stick), a, b, y, x (start talking to the agent, x again to send), q (quit).";

pub fn run(mut conversation: Conversation, mut connection: Connection) -> Result<()> {
    let (sender, input) = mpsc::channel();
    std::thread::Builder::new()
        .name("console-input".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        })?;
    println!("{HELP}");
    let mut shown = String::new();
    loop {
        connection.poll(true, &mut conversation);
        match input.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => {
                for command in line.split_whitespace() {
                    match command {
                        "u" => conversation.navigate(0, -1),
                        "d" => conversation.navigate(0, 1),
                        "l" => conversation.navigate(-1, 0),
                        "r" => conversation.navigate(1, 0),
                        "<" => conversation.choose(-1),
                        ">" => conversation.choose(1),
                        "a" => {
                            if conversation.accept() {
                                println!(
                                    "(A has nothing to choose; on the headset it resets the swarm.)"
                                );
                            }
                        }
                        "b" => conversation.back(),
                        "y" => conversation.refresh(),
                        // A terminal reports no key releases, so x starts recording and x again sends it.
                        "x" => {
                            let recording = conversation.recording().is_some();
                            conversation.talk(!recording);
                        }
                        "q" => return Ok(()),
                        _ => println!("{HELP}"),
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Standard input ended.
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        connection.poll(true, &mut conversation);
        let text = conversation.text();
        if text != shown {
            let border = format!("+{}+", "-".repeat(COLUMNS));
            println!("{border}");
            for line in text.lines() {
                println!("|{line:<COLUMNS$}|");
            }
            println!("{border} {:?}", conversation.activity());
            shown = text;
        }
    }
}
