// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! `tomori-server [--bind ADDRESS] [--backend NAME]... [--speech-root PATH] [--speech-provider NAME] [--name NAME] [--no-advertise] [--keepalive-interval SECONDS] [--keepalive-timeout SECONDS] [--request-timeout SECONDS] [--workspace-root PATH]... [--max-spaces COUNT]`
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
use std::time::Duration;

use base_scheduler::{Cancellation, SocketLocalAddress};
use base_scheduler_direct::{DirectOptions, DirectScheduler};
use futures_lite::future::block_on;
use protocol::Keepalive;
use server::{Backends, NullTranscriber, Server, Transcriber, WorkspaceRoots};

const USAGE: &str = "Usage: tomori-server [--bind ADDRESS] [--backend NAME]... [--speech-root PATH]
                     [--speech-provider NAME] [--name NAME] [--no-advertise] [--keepalive-interval SECONDS]
                     [--keepalive-timeout SECONDS] [--request-timeout SECONDS]
                     [--workspace-root PATH]... [--max-spaces COUNT]

Options:
  --bind ADDRESS       Listen address (default 0.0.0.0:8765).
  --backend NAME       Offer a backend; repeat for several. Available: fake, claude-code.
  --speech-root PATH   The directory holding .tools/ with the speech models; see
                       crates/speech/readme.md. Without it, there is no speech recognition or
                       synthesis.
  --speech-provider NAME
                       The ONNX Runtime execution provider for speech recognition: cpu (default), or
                       cuda in a server built with the speech-shared feature and the CUDA libraries.
  --name NAME          The name clients list for this server (default: the host name).
  --no-advertise       Do not advertise the server with Bonjour (_tomori._tcp); clients must enter
                       its address.
  --keepalive-interval SECONDS
                       Ping a connection that has been quiet this long (default 5).
  --keepalive-timeout SECONDS
                       Release a connection that has received nothing, or accepted no output,
                       for this long (default 15). Must be longer than the interval.
  --request-timeout SECONDS
                       Close a connection that has not sent a request, or said Hello, within this
                       long (default 10).
  --workspace-root PATH
                       Let spaces run in this directory or beneath it; repeat for several. Without
                       it, the only root is the directory the server starts in. Each root must exist.
  --max-spaces COUNT   The most spaces whose agent sessions run at once (default 8).
  --help               Show this help.";

struct Arguments {
    bind: SocketAddr,
    backends: Vec<String>,
    speech_root: Option<PathBuf>,
    speech_provider: String,
    name: Option<String>,
    advertise: bool,
    keepalive: Keepalive,
    request_timeout: Option<Duration>,
    /// Empty for the default, the current directory.
    workspace_roots: Vec<PathBuf>,
    max_spaces: usize,
}

/// A positive number of seconds, such as `5` or `0.5`.
fn seconds(flag: &str, value: Option<String>) -> Result<Duration, String> {
    let value = value.ok_or_else(|| format!("{flag} needs a number of seconds"))?;
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .ok_or_else(|| format!("{flag} needs a positive number of seconds, not {value:?}"))
}

fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Arguments, String> {
    let mut parsed = Arguments {
        bind: SocketAddr::from(([0, 0, 0, 0], protocol::DEFAULT_PORT)),
        backends: Vec::new(),
        speech_root: None,
        speech_provider: "cpu".to_owned(),
        name: None,
        advertise: true,
        keepalive: Keepalive::DEFAULT,
        request_timeout: None,
        workspace_roots: Vec::new(),
        max_spaces: server::DEFAULT_MAX_SPACES,
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
            "--speech-provider" => {
                let value = arguments.next().ok_or("--speech-provider needs a name")?;
                if !matches!(value.as_str(), "cpu" | "cuda") {
                    return Err(format!(
                        "--speech-provider needs cpu or cuda, not {value:?}"
                    ));
                }
                parsed.speech_provider = value;
            }
            "--name" => parsed.name = Some(arguments.next().ok_or("--name needs a name")?),
            "--no-advertise" => parsed.advertise = false,
            "--keepalive-interval" => {
                parsed.keepalive.interval = seconds(&argument, arguments.next())?
            }
            "--keepalive-timeout" => {
                parsed.keepalive.timeout = seconds(&argument, arguments.next())?
            }
            "--request-timeout" => {
                parsed.request_timeout = Some(seconds(&argument, arguments.next())?)
            }
            "--workspace-root" => parsed.workspace_roots.push(
                arguments
                    .next()
                    .ok_or("--workspace-root needs a directory")?
                    .into(),
            ),
            "--max-spaces" => {
                let value = arguments.next().ok_or("--max-spaces needs a count")?;
                parsed.max_spaces = value
                    .parse::<usize>()
                    .ok()
                    .filter(|count| *count > 0)
                    .ok_or_else(|| format!("--max-spaces needs a positive count, not {value:?}"))?;
            }
            "--help" | "-h" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    if parsed.keepalive.timeout <= parsed.keepalive.interval {
        return Err(format!(
            "--keepalive-timeout ({:?}) must be longer than --keepalive-interval ({:?}), so that a ping can be answered first",
            parsed.keepalive.timeout, parsed.keepalive.interval
        ));
    }
    if parsed.backends.is_empty() {
        parsed.backends.push("fake".to_owned());
    }
    Ok(parsed)
}

/// The canonical workspace roots: those given, or the current directory. A root that does not exist is an error. They are printed, so that starting the server somewhere broad, such as the home directory, is visible.
fn workspace_roots(roots: &[PathBuf]) -> Result<WorkspaceRoots, String> {
    let roots = if roots.is_empty() {
        WorkspaceRoots::current()
    } else {
        WorkspaceRoots::new(roots)
    }
    .map_err(|error| format!("Invalid --workspace-root: {error}."))?;
    let listed: Vec<String> = roots
        .roots()
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    eprintln!("Workspace roots: {}.", listed.join(", "));
    Ok(roots)
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

/// Speech recognition: the streaming transducer from `root`, run with `provider`, or none. Without recognition, every utterance fails visibly.
#[cfg(feature = "speech")]
fn transcriber(root: Option<&Path>, provider: &str) -> Result<Arc<dyn Transcriber>, String> {
    use speech::stt::{TransducerConfig, TransducerRecognizer};

    let Some(root) = root else {
        eprintln!("Speech recognition: off (no --speech-root); utterances fail with an error.");
        return Ok(Arc::new(NullTranscriber::new()));
    };
    let mut config = TransducerConfig::from_root(root);
    config.provider = provider.to_owned();
    Ok(match TransducerRecognizer::new(&config) {
        Ok(recognizer) => {
            eprintln!(
                "Speech recognition: streaming transducer from {} ({provider}).",
                config.model.display()
            );
            Arc::new(server::RecognitionTranscriber::streaming(recognizer))
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
fn transcriber(root: Option<&Path>, _provider: &str) -> Result<Arc<dyn Transcriber>, String> {
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
    use speech::tts::{KokoroConfig, KokoroSynthesizer};

    let Some(root) = root else {
        eprintln!("Speech synthesis: off (no --speech-root); replies are not spoken.");
        return server;
    };
    match KokoroSynthesizer::new(&KokoroConfig::from_root(root)) {
        Ok(synthesizer) => {
            eprintln!("Speech synthesis: Kokoro from {}.", root.display());
            server.with_synthesizer(synthesizer)
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
    // Log to standard error, at info and above unless `RUST_LOG` says otherwise.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
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
    let roots = match workspace_roots(&arguments.workspace_roots) {
        Ok(roots) => roots,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let root = arguments.speech_root.as_deref();
    let transcriber = match transcriber(root, &arguments.speech_provider) {
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
    let mut server = Server::new(backends, transcriber)
        .with_keepalive(arguments.keepalive)
        .with_workspace_roots(roots)
        .with_max_spaces(arguments.max_spaces);
    if let Some(timeout) = arguments.request_timeout {
        server = server.with_request_timeout(timeout);
    }
    let server = synthesis(server, root);
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
            .map_err(|error| {
                let mut message = format!("The server failed: {error}");
                let mut source = std::error::Error::source(&error);
                while let Some(cause) = source {
                    message.push_str(&format!(": {cause}"));
                    source = cause.source();
                }
                message
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(list: &[&str]) -> Result<Arguments, String> {
        parse(list.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn keepalive_and_request_timeouts_are_configurable() {
        let parsed = arguments(&[]).unwrap();
        assert_eq!(parsed.keepalive, Keepalive::DEFAULT);
        assert_eq!(parsed.request_timeout, None);

        let parsed = arguments(&[
            "--keepalive-interval",
            "0.5",
            "--keepalive-timeout",
            "2",
            "--request-timeout",
            "3",
        ])
        .unwrap();
        assert_eq!(parsed.keepalive.interval, Duration::from_millis(500));
        assert_eq!(parsed.keepalive.timeout, Duration::from_secs(2));
        assert_eq!(parsed.request_timeout, Some(Duration::from_secs(3)));
    }

    #[test]
    fn invalid_timeouts_are_refused() {
        for list in [
            &["--keepalive-interval"][..],
            &["--keepalive-interval", "0"],
            &["--keepalive-interval", "-1"],
            &["--keepalive-timeout", "soon"],
            &["--request-timeout", "inf"],
            // The timeout must exceed the interval.
            &["--keepalive-interval", "20"],
            &["--keepalive-interval", "4", "--keepalive-timeout", "4"],
        ] {
            assert!(arguments(list).is_err(), "{list:?} was accepted");
        }
    }

    #[test]
    fn workspace_roots_and_the_space_limit_are_configurable() {
        let parsed = arguments(&[]).unwrap();
        assert!(parsed.workspace_roots.is_empty());
        assert_eq!(parsed.max_spaces, server::DEFAULT_MAX_SPACES);
        assert_eq!(server::DEFAULT_MAX_SPACES, 8);

        let parsed = arguments(&[
            "--workspace-root",
            "/srv/a",
            "--max-spaces",
            "3",
            "--workspace-root",
            "/srv/b",
        ])
        .unwrap();
        assert_eq!(
            parsed.workspace_roots,
            [PathBuf::from("/srv/a"), PathBuf::from("/srv/b")]
        );
        assert_eq!(parsed.max_spaces, 3);
    }

    #[test]
    fn invalid_workspace_options_are_refused() {
        for list in [
            &["--workspace-root"][..],
            &["--max-spaces"],
            &["--max-spaces", "0"],
            &["--max-spaces", "-1"],
            &["--max-spaces", "many"],
        ] {
            assert!(arguments(list).is_err(), "{list:?} was accepted");
        }
    }

    #[test]
    fn workspace_roots_must_exist() {
        let missing =
            std::env::temp_dir().join(format!("tomori-missing-root-{}", std::process::id()));
        assert!(workspace_roots(&[missing]).is_err());
        // Without any, the current directory is the only root.
        let current = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(workspace_roots(&[]).unwrap().roots(), [current]);
        let roots = workspace_roots(&[std::env::temp_dir()]).unwrap();
        assert_eq!(
            roots.roots(),
            [std::env::temp_dir().canonicalize().unwrap()]
        );
    }
}
