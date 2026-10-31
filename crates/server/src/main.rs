// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! `tomori-server [--bind ADDRESS] [--backend NAME]...`
//!
//! Serves Tomori clients on the local network. There is no authentication yet: anyone who can reach the port can join a space. See `design/server.md`.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use base_scheduler::{Cancellation, SocketLocalAddress};
use base_scheduler_direct::{DirectOptions, DirectScheduler};
use futures_lite::future::block_on;
use server::{Backends, NullTranscriber, Server};

const USAGE: &str = "Usage: tomori-server [--bind ADDRESS] [--backend NAME]...

Options:
  --bind ADDRESS   Listen address (default 0.0.0.0:8765).
  --backend NAME   Offer a backend; repeat for several. Available: fake.
  --help           Show this help.";

struct Arguments {
    bind: SocketAddr,
    backends: Vec<String>,
}

fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut parsed = Arguments {
        bind: SocketAddr::from(([0, 0, 0, 0], protocol::DEFAULT_PORT)),
        backends: Vec::new(),
    };
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--bind" => {
                let value = arguments.next().ok_or("--bind needs an address")?;
                parsed.bind = value
                    .parse()
                    .map_err(|error| format!("invalid address {value:?}: {error}"))?;
            }
            "--backend" => parsed
                .backends
                .push(arguments.next().ok_or("--backend needs a name")?),
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    if parsed.backends.is_empty() {
        parsed.backends.push("fake".to_owned());
    }
    Ok(parsed)
}

fn backends(names: &[String]) -> Result<Backends, String> {
    let mut backends = Backends::new();
    for name in names {
        backends = match name.as_str() {
            #[cfg(feature = "fake")]
            "fake" => backends.with(server::fake::FakeBackend::new()),
            other => return Err(format!("unknown backend {other:?}")),
        };
    }
    Ok(backends)
}

fn main() -> ExitCode {
    let arguments = match parse(std::env::args().skip(1)) {
        Ok(arguments) => arguments,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let backends = match backends(&arguments.backends) {
        Ok(backends) => backends,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let scheduler = match DirectScheduler::new(DirectOptions {
        threads: 4,
        capacity: 1024,
    }) {
        Ok(scheduler) => scheduler,
        Err(error) => {
            eprintln!("Could not start the scheduler: {error}");
            return ExitCode::FAILURE;
        }
    };
    let server = Server::new(backends, Arc::new(NullTranscriber::new()));
    let cancellation = Cancellation::new();
    let result = block_on(async {
        let listener = server::bind(&scheduler, arguments.bind, &cancellation)
            .await
            .map_err(|error| format!("Could not listen on {}: {error}", arguments.bind))?;
        let address = scheduler
            .socket_local_address(&listener)
            .map_err(|error| error.to_string())?;
        eprintln!(
            "Tomori server listening on http://{address} (backends: {}). There is no authentication: anyone on this network can join.",
            arguments.backends.join(", ")
        );
        server
            .run(&scheduler, listener, &cancellation)
            .await
            .map(|_| ())
            .map_err(|error| format!("The server failed: {error}"))
    });
    block_on(scheduler.shutdown());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
