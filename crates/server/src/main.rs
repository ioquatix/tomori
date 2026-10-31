// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! `tomori-server [--bind ADDRESS] [--backend NAME]... [--speech-root PATH] [--name NAME] [--no-advertise]`
//!
//! Serves Tomori clients on the local network, advertised with Bonjour as `_tomori._tcp`. There is no authentication yet: anyone who can reach the port can join a space. See `design/server.md`.

// Built without any backend, the backend enums are empty and every match on them is vacuous.
#![cfg_attr(
    not(any(feature = "fake", feature = "claude-code")),
    allow(unused_variables, unused_mut, unreachable_code)
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use base_scheduler::{Cancellation, SocketLocalAddress};
use base_scheduler_direct::{DirectOptions, DirectScheduler};
use futures_lite::future::block_on;
use server::{Backends, NullTranscriber, Server, Transcriber};

const USAGE: &str = "Usage: tomori-server [--bind ADDRESS] [--backend NAME]... [--speech-root PATH]
                     [--name NAME] [--no-advertise]

Options:
  --bind ADDRESS       Listen address (default 0.0.0.0:8765).
  --backend NAME       Offer a backend; repeat for several. Available: fake, claude-code.
  --speech-root PATH   The directory holding .tools/ with the speech environments and models; see
                       crates/speech/worker/readme.md. Without it, there is no speech recognition or
                       synthesis.
  --name NAME          The name clients list for this server (default: the host name).
  --no-advertise       Do not advertise the server with Bonjour (_tomori._tcp); clients must enter
                       its address.
  --help               Show this help.";

struct Arguments {
    bind: SocketAddr,
    backends: Vec<String>,
    speech_root: Option<PathBuf>,
    name: Option<String>,
    advertise: bool,
}

fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut parsed = Arguments {
        bind: SocketAddr::from(([0, 0, 0, 0], protocol::DEFAULT_PORT)),
        backends: Vec::new(),
        speech_root: None,
        name: None,
        advertise: true,
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
            "--speech-root" => {
                parsed.speech_root = Some(
                    arguments
                        .next()
                        .ok_or("--speech-root needs a directory")?
                        .into(),
                )
            }
            "--name" => parsed.name = Some(arguments.next().ok_or("--name needs a name")?),
            "--no-advertise" => parsed.advertise = false,
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    if parsed.backends.is_empty() {
        parsed.backends.push("fake".to_owned());
    }
    Ok(parsed)
}

/// The default display name: the host name without its domain, such as `studio` for `studio.local`.
fn host_name() -> String {
    let host = gethostname::gethostname();
    let host = host.to_string_lossy();
    match host.split('.').next() {
        Some(label) if !label.is_empty() => label.to_owned(),
        _ => "Tomori".to_owned(),
    }
}

/// Advertise the bound `address` unless turned off. A failure is reported but does not stop the server, which remains reachable by address.
fn advertise(arguments: &Arguments, address: SocketAddr) -> Option<discovery::Advertisement> {
    if !arguments.advertise {
        eprintln!("Bonjour: not advertising (--no-advertise).");
        return None;
    }
    let name = arguments.name.clone().unwrap_or_else(host_name);
    match discovery::advertise(&name, address.port(), address.ip()) {
        Ok(advertisement) => {
            eprintln!(
                "Bonjour: advertising {:?} as _tomori._tcp on port {}.",
                advertisement.instance(),
                address.port()
            );
            Some(advertisement)
        }
        Err(error) => {
            eprintln!("Bonjour: could not advertise ({error}); clients must enter the address.");
            None
        }
    }
}

fn backend(name: &str) -> Result<server::AnyBackend, String> {
    match name {
        #[cfg(feature = "fake")]
        "fake" => Ok(server::fake::FakeBackend::new().into()),
        #[cfg(feature = "claude-code")]
        "claude-code" => Ok(backend_claude_code::ClaudeCode::new(Default::default()).into()),
        other => Err(format!("unknown backend {other:?}")),
    }
}

fn backends(names: &[String]) -> Result<Backends, String> {
    let mut backends = Backends::new();
    for name in names {
        backends = backends.with(backend(name)?);
    }
    Ok(backends)
}

/// Speech recognition: Whisper from `root`, or none. Without recognition, every utterance fails visibly.
#[cfg(feature = "speech")]
fn transcriber(root: Option<&Path>) -> Result<Arc<dyn Transcriber>, String> {
    use speech::stt::{WhisperConfig, WhisperWorker};

    let Some(root) = root else {
        eprintln!("Speech recognition: off (no --speech-root); utterances fail with an error.");
        return Ok(Arc::new(NullTranscriber::new()));
    };
    let started = WhisperWorker::new(&WhisperConfig::from_root(root)).and_then(|mut worker| {
        worker.start()?;
        Ok(worker)
    });
    Ok(match started {
        Ok(worker) => {
            eprintln!("Speech recognition: Whisper from {}.", root.display());
            Arc::new(server::RecognitionTranscriber::new(
                worker,
                Default::default(),
            ))
        }
        Err(error) => {
            eprintln!(
                "Speech recognition: unavailable ({error}); utterances fail with this error."
            );
            Arc::new(NullTranscriber::with_reason(format!(
                "Speech recognition is not available on this server: {error}."
            )))
        }
    })
}

#[cfg(not(feature = "speech"))]
fn transcriber(root: Option<&Path>) -> Result<Arc<dyn Transcriber>, String> {
    if root.is_some() {
        return Err(
            "This server was built without the speech feature, so --speech-root is not supported."
                .to_owned(),
        );
    }
    eprintln!("Speech recognition: off (built without speech); utterances fail with an error.");
    Ok(Arc::new(NullTranscriber::new()))
}

/// Speech synthesis: Kokoro from `root`, or none, in which case replies are not spoken.
#[cfg(feature = "speech")]
fn synthesis(server: Server, root: Option<&Path>) -> Server {
    use speech::tts::{KokoroConfig, KokoroWorker};

    let Some(root) = root else {
        eprintln!("Speech synthesis: off (no --speech-root); replies are not spoken.");
        return server;
    };
    let started = KokoroWorker::new(&KokoroConfig::from_root(root)).and_then(|mut worker| {
        worker.start()?;
        Ok(worker)
    });
    match started {
        Ok(worker) => {
            eprintln!("Speech synthesis: Kokoro from {}.", root.display());
            server.with_synthesizer(worker)
        }
        Err(error) => {
            eprintln!("Speech synthesis: unavailable ({error}); replies are not spoken.");
            server
        }
    }
}

#[cfg(not(feature = "speech"))]
fn synthesis(server: Server, _root: Option<&Path>) -> Server {
    eprintln!("Speech synthesis: off (built without speech); replies are not spoken.");
    server
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
    let root = arguments.speech_root.as_deref();
    let transcriber = match transcriber(root) {
        Ok(transcriber) => transcriber,
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
    let server = synthesis(Server::new(backends, transcriber), root);
    let cancellation = Cancellation::new();
    // Stop on Ctrl-C or SIGTERM by shutting down in order, so that the Bonjour advertisement is withdrawn rather than left to expire.
    #[cfg(unix)]
    let _signals = {
        use base_scheduler::Signals;
        scheduler
            .signal_subscribe(
                &[libc::SIGINT, libc::SIGTERM],
                Shutdown(cancellation.clone()),
            )
            .inspect_err(|error| eprintln!("Could not handle signals: {error}"))
            .ok()
    };
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
        let _advertisement = advertise(&arguments, address);
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

/// Shut down in order on the first signal, and exit immediately on a repeated one in case shutdown hangs.
#[cfg(unix)]
struct Shutdown(Cancellation);

#[cfg(unix)]
impl base_scheduler::Notify<libc::c_int> for Shutdown {
    fn notify(&self, signal: libc::c_int) {
        if self.0.cancel() {
            eprintln!("Shutting down; signal again to exit immediately.");
        } else {
            std::process::exit(128 + signal);
        }
    }
}
