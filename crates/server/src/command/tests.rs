// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;
use futures_lite::future::block_on;

fn arguments(list: &[&str]) -> Result<Arguments, String> {
    parse(
        list.iter().map(|argument| argument.to_string()),
        &usage(),
        &[],
    )
}

/// The usage of `tomori-server`, with Tomori's own backends.
fn usage() -> String {
    Server::builder().usage()
}

#[test]
fn the_usage_lists_the_speech_options_only_in_a_server_built_with_speech() {
    let with = super::usage("tomori-server", "fake", &[], &[], true);
    assert!(with.starts_with(
        "Usage: tomori-server [--bind ADDRESS] [--backend NAME]... [--speech-root PATH]\n                     [--speech-provider NAME] [--name NAME] [--no-advertise]"
    ));
    assert!(with.contains(&format!(
        "--service` registration file, then http://127.0.0.1:4096.\n{SPEECH}  --name NAME "
    )));
    let without = super::usage("tomori-server", "fake", &[], &[], false);
    assert!(without.starts_with(
        "Usage: tomori-server [--bind ADDRESS] [--backend NAME]...\n                     [--name NAME] [--no-advertise]"
    ));
    assert!(
        without
            .contains("--service` registration file, then http://127.0.0.1:4096.\n  --name NAME ")
    );
    assert!(!without.contains("speech"), "{without}");
    // Nothing else changes.
    assert_eq!(
        without,
        with.replace(" [--speech-root PATH]", "")
            .replace("[--speech-provider NAME] ", "")
            .replace(SPEECH, "")
    );
}

/// A server built without speech refuses its speech options, as it refuses `--speech-root`, rather than ignoring them.
#[cfg(not(feature = "speech"))]
#[test]
fn a_server_without_speech_refuses_its_options() {
    for flag in [
        "--speech-provider",
        "--speech-backend",
        "--speech-offline-model",
        "--speech-segmentation",
    ] {
        assert_eq!(
            arguments(&[flag, "cpu"]).err(),
            Some(format!(
                "This server was built without the speech feature, so {flag} is not supported."
            ))
        );
    }
}

/// An empty directory of its own for the test named `name`.
fn directory(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("tomori-server-main-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

/// Arguments for a server on an ephemeral loopback port, with its state and workspace in `directory`, and `extra`.
fn local(directory: &Path, extra: &[&str]) -> Arguments {
    let directory = directory.to_str().unwrap();
    let mut list = vec![
        "--bind",
        "127.0.0.1:0",
        "--no-advertise",
        "--state-directory",
        directory,
        "--workspace-root",
        directory,
    ];
    list.extend_from_slice(extra);
    arguments(&list).unwrap()
}

#[test]
fn every_option_is_read_and_every_missing_value_is_refused() {
    let parsed = arguments(&[
        "--bind",
        "127.0.0.1:9999",
        "--backend",
        "fake",
        "--backend",
        "opencode",
        "--opencode-url",
        "http://127.0.0.1:4096",
        "--speech-root",
        "/srv/speech",
        "--name",
        "Studio",
        "--no-advertise",
    ])
    .unwrap();
    assert_eq!(parsed.bind, "127.0.0.1:9999".parse().unwrap());
    assert_eq!(parsed.backends, ["fake", "opencode"]);
    assert_eq!(
        parsed.opencode_url.as_deref(),
        Some("http://127.0.0.1:4096")
    );
    assert_eq!(parsed.speech_root, Some(PathBuf::from("/srv/speech")));
    assert_eq!(parsed.name.as_deref(), Some("Studio"));
    // The speech options, which only a server built with speech takes.
    #[cfg(feature = "speech")]
    {
        let parsed = arguments(&[
            "--speech-backend",
            "offline",
            "--speech-segmentation",
            "sentence",
        ])
        .unwrap();
        assert_eq!(parsed.speech_backend, "offline");
        assert_eq!(parsed.speech_segmentation, "sentence");
        for (list, error) in [
            (&["--speech-backend"][..], "--speech-backend needs a name"),
            (
                &["--speech-backend", "batch"],
                "--speech-backend needs streaming or offline, not \"batch\"",
            ),
            (
                &["--speech-segmentation"],
                "--speech-segmentation needs a name",
            ),
            (
                &["--speech-segmentation", "word"],
                "--speech-segmentation needs block or sentence, not \"word\"",
            ),
        ] {
            assert_eq!(arguments(list).err().as_deref(), Some(error), "{list:?}");
        }
    }
    assert!(!parsed.advertise);

    // Without any, the fake backend is offered, on the default port on every interface, streaming and speaking whole blocks.
    let parsed = arguments(&[]).unwrap();
    assert_eq!(parsed.backends, ["fake"]);
    assert_eq!(
        parsed.bind,
        SocketAddr::from(([0, 0, 0, 0], tomori_protocol::DEFAULT_PORT))
    );
    assert_eq!(parsed.speech_backend, "streaming");
    assert_eq!(parsed.speech_segmentation, "block");

    for (list, error) in [
        (&["--bind"][..], "--bind needs an address"),
        (&["--backend"], "--backend needs a name"),
        (&["--opencode-url"], "--opencode-url needs a URL"),
        (&["--speech-root"], "--speech-root needs a directory"),
        (&["--name"], "--name needs a name"),
        (
            &["--max-visitors", "many"],
            "--max-visitors needs a count, not \"many\"",
        ),
    ] {
        assert_eq!(arguments(list).err().as_deref(), Some(error), "{list:?}");
    }
    assert_eq!(
        arguments(&["--mcp-bind"]).err().as_deref(),
        Some("--mcp-bind needs an address")
    );
    let error = arguments(&["--mcp-bind", "nowhere"]).err().unwrap();
    assert!(
        error.starts_with("invalid address \"nowhere\": "),
        "{error}"
    );
    // Tomori's tools are served on a free loopback port, unless asked elsewhere or not at all.
    assert_eq!(
        arguments(&[]).unwrap().mcp,
        Some("127.0.0.1:0".parse().unwrap())
    );
    assert_eq!(
        arguments(&["--mcp-bind", "127.0.0.1:8766"]).unwrap().mcp,
        Some("127.0.0.1:8766".parse().unwrap())
    );
    assert_eq!(arguments(&["--no-mcp"]).unwrap().mcp, None);
    let error = arguments(&["--bind", "nowhere"]).err().unwrap();
    assert!(
        error.starts_with("invalid address \"nowhere\": "),
        "{error}"
    );
    assert_eq!(
        arguments(&["--help"]).err().as_deref(),
        Some(usage().as_str())
    );
    assert_eq!(arguments(&["-h"]).err().as_deref(), Some(usage().as_str()));
    assert_eq!(
        arguments(&["--verbose"]).err(),
        Some(format!("unknown argument \"--verbose\"\n\n{}", usage()))
    );
}

#[test]
fn the_state_directory_is_the_one_given_else_the_environments() {
    let given = arguments(&["--state-directory", "/var/lib/tomori"]).unwrap();
    let none = arguments(&[]).unwrap();
    let environment = Path::new("/home/ada/.local/state/tomori");
    assert_eq!(
        state_directory(&given, Some(environment)),
        Ok(PathBuf::from("/var/lib/tomori"))
    );
    assert_eq!(
        state_directory(&none, Some(environment)),
        Ok(environment.to_owned())
    );
    assert_eq!(
        state_directory(&none, None),
        Err(
            "No state directory: give --state-directory, or set XDG_STATE_HOME or HOME.".to_owned()
        )
    );
}

/// The server `arguments` configure with no speech, or why it cannot be configured.
fn configured(arguments: &Arguments) -> Result<(Server, Option<String>), String> {
    let mut engines = Engines::default();
    // Offering environments, as an application registers them.
    let builder = crate::test_support::environments()
        .into_iter()
        .fold(Server::builder(), Builder::environment);
    let configured = configure(builder, arguments, None, &mut engines);
    assert!(engines.stop(Instant::now()));
    configured
}

#[test]
fn an_open_server_serves_plain_http_and_keeps_its_spaces() {
    let directory = directory("open");
    let (server, fingerprint) = configured(&local(
        &directory,
        &["--auth", "open", "--request-timeout", "3"],
    ))
    .unwrap();
    assert_eq!(fingerprint, None);
    drop(server);
    // Its spaces are kept in the state directory, though it issues no tokens.
    assert!(!directory.join("tokens.json").exists());
    assert!(!directory.join("certificate.der").exists());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_server_admitting_by_tokens_serves_tls_with_the_certificate_it_keeps() {
    let directory = directory("tokens");
    let (_, fingerprint) = configured(&local(&directory, &["--no-visitors"])).unwrap();
    let certificate = tomori_security::ServerCertificate::load_or_create(&directory).unwrap();
    assert_eq!(fingerprint, Some(certificate.fingerprint().to_string()));
    // Started again, it presents the same certificate, now with an owner token issued.
    tomori_security::TokenStore::open(&directory)
        .unwrap()
        .issue("Ada", tomori_security::Grant::Owner)
        .unwrap();
    let (_, again) = configured(&local(&directory, &[])).unwrap();
    assert_eq!(again, fingerprint);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_dropped_server_frees_its_state_directory_while_its_console_is_still_read() {
    let directory = directory("console");
    let (server, _) = configured(&local(&directory, &[])).unwrap();
    // As the console thread does, until the console ends.
    let console = server.pairing();
    drop(server);
    assert!(crate::kept::SpaceStore::open(&directory).is_ok());
    assert!(console.pending().is_none());
    assert_eq!(console.confirm("123456"), Err("The server has stopped."));
    assert!(!console.decline());
    assert!(!console.paused());
    console.allow();
    assert_eq!(
        console_reply(&console, "decline").as_deref(),
        Some("No pairing is waiting.")
    );
    assert_eq!(console.open(), Err("The server has stopped."));
    assert_eq!(
        console_reply(&console, "pair").as_deref(),
        Some("The server has stopped.")
    );
    assert!(!console.close());
    assert_eq!(console.state(), None);
    assert_eq!(
        console_reply(&console, "close").as_deref(),
        Some("Pairing is not open.")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_state_directory_that_cannot_be_used_stops_the_server_from_starting() {
    let directory = directory("unusable");
    let state = directory.join("state");
    let failure = |extra: &[&str]| {
        let mut arguments = local(&directory, extra);
        arguments.state_directory = Some(state.clone());
        configured(&arguments).err().unwrap()
    };

    // A file where the directory should be: neither tokens nor spaces can be kept.
    std::fs::write(&state, "").unwrap();
    let error = failure(&[]);
    assert!(
        error.starts_with("Could not open the token store in "),
        "{error}"
    );
    let error = failure(&["--auth", "open"]);
    assert!(
        error.starts_with("Could not open the spaces kept in "),
        "{error}"
    );
    std::fs::remove_file(&state).unwrap();

    // A certificate that cannot be read, or a key that cannot be used.
    std::fs::create_dir_all(state.join("certificate.der")).unwrap();
    let error = failure(&[]);
    assert!(
        error.starts_with("Could not load or create the server's certificate in "),
        "{error}"
    );
    std::fs::remove_dir(state.join("certificate.der")).unwrap();
    tomori_security::ServerCertificate::load_or_create(&state).unwrap();
    std::fs::write(state.join("key.der"), "not a key").unwrap();
    let error = failure(&[]);
    assert!(
        error.starts_with("The server's certificate cannot be used: "),
        "{error}"
    );

    // Without a state directory at all.
    let mut arguments = local(&directory, &["--auth", "open"]);
    arguments.state_directory = None;
    assert_eq!(
        configured(&arguments).err().as_deref(),
        Some("No state directory: give --state-directory, or set XDG_STATE_HOME or HOME.")
    );
    let mut arguments = local(&directory, &[]);
    arguments.state_directory = None;
    assert_eq!(
        configured(&arguments).err().as_deref(),
        Some("No state directory: give --state-directory, or set XDG_STATE_HOME or HOME.")
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unknown_backends_and_missing_roots_stop_the_server_from_starting() {
    let directory = directory("refused");
    assert_eq!(
        configured(&local(&directory, &["--backend", "claude"]))
            .err()
            .as_deref(),
        Some("unknown backend \"claude\"")
    );
    let mut arguments = local(&directory, &[]);
    arguments.workspace_roots = vec![directory.join("missing")];
    let error = configured(&arguments).err().unwrap();
    assert!(error.starts_with("Invalid --workspace-root: "), "{error}");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn every_backend_this_build_has_can_be_offered() {
    let parsed = arguments(&["--backend", "fake", "--backend", "opencode"]).unwrap();
    let builder = Server::builder();
    assert!(backends(&builder, &parsed).is_ok());
    assert!(builder.backend_named("fake", &parsed).is_ok());
    assert!(builder.backend_named("opencode", &parsed).is_ok());
    assert_eq!(
        builder.backend_named("claude", &parsed).err().as_deref(),
        Some("unknown backend \"claude\"")
    );
}

#[cfg(feature = "speech")]
#[test]
fn speech_without_models_starts_no_engine_and_the_server_still_starts() {
    let directory = directory("speech");
    let root = directory.to_str().unwrap();
    for extra in [
        &["--speech-root", root][..],
        &["--speech-root", root, "--speech-backend", "offline"],
        &[
            "--speech-root",
            root,
            "--speech-backend",
            "offline",
            "--speech-offline-model",
            "tiny.en",
        ],
    ] {
        let mut engines = Engines::default();
        let arguments = local(&directory, &[&["--auth", "open"][..], extra].concat());
        assert!(configure(Server::builder(), &arguments, None, &mut engines).is_ok());
        // Recognition and synthesis are unavailable, and so is every engine's thread.
        #[cfg(feature = "speech")]
        assert!(engines.stoppers.is_empty());
        assert!(engines.stop(Instant::now()));
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn the_display_name_is_the_hosts_first_label() {
    assert_eq!(display_name("studio.local"), "studio");
    assert_eq!(display_name("studio"), "studio");
    assert_eq!(display_name(""), "Tomori");
    assert_eq!(display_name(".local"), "Tomori");
    assert!(!host_name().is_empty());
}

/// What an advertiser that fails was asked to advertise, as name, port, address and fingerprint.
type Asked = (String, u16, IpAddr, Option<String>);

/// An advertiser that records what it was asked to advertise on `asked`, and fails as one without multicast would.
fn failing_advertiser(
    asked: std::sync::mpsc::Sender<Asked>,
) -> impl Advertiser<Error = &'static str> {
    move |name: &str, port, address, _counts, fingerprint| {
        asked
            .send((name.to_owned(), port, address, fingerprint))
            .unwrap();
        Err("no multicast here")
    }
}

#[test]
fn advertising_is_skipped_when_turned_off_and_its_failure_is_not_fatal() {
    let address = SocketAddr::from(([127, 0, 0, 1], 9999));
    let (asked, received) = std::sync::mpsc::channel();
    let off = arguments(&["--no-advertise"]).unwrap();
    assert!(
        advertise(
            &off,
            address,
            Default::default(),
            None,
            failing_advertiser(asked.clone())
        )
        .is_none()
    );
    assert!(received.try_recv().is_err());

    let named = arguments(&["--name", "Studio"]).unwrap();
    let fingerprint = Some("ab".repeat(32));
    assert!(
        advertise(
            &named,
            address,
            Default::default(),
            fingerprint.clone(),
            failing_advertiser(asked.clone())
        )
        .is_none()
    );
    assert_eq!(
        received.try_recv().unwrap(),
        ("Studio".to_owned(), 9999, address.ip(), fingerprint)
    );
    // Without a name, the host's.
    advertise(
        &arguments(&[]).unwrap(),
        address,
        Default::default(),
        None,
        failing_advertiser(asked),
    );
    assert_eq!(received.try_recv().unwrap().0, host_name());
}

/// Serve `server` as `arguments` say until the returned cancellation, with an advertiser that fails, returning what it was asked to advertise, once listening.
fn serving(
    server: Server,
    arguments: Arguments,
    fingerprint: Option<String>,
) -> (
    std::sync::mpsc::Receiver<Asked>,
    Cancellation,
    std::thread::JoinHandle<Result<(), String>>,
) {
    let (asked, received) = std::sync::mpsc::channel();
    let (cancellation, serving) =
        serving_with(server, arguments, fingerprint, failing_advertiser(asked));
    (received, cancellation, serving)
}

/// Serve `server` as `arguments` say, advertised with `start`, until the returned cancellation.
fn serving_with(
    server: Server,
    arguments: Arguments,
    fingerprint: Option<String>,
    start: impl Advertiser + Send + 'static,
) -> (Cancellation, std::thread::JoinHandle<Result<(), String>>) {
    let cancellation = Cancellation::new();
    let serving = std::thread::spawn({
        let cancellation = cancellation.clone();
        move || {
            let scheduler = NativeScheduler::new().unwrap();
            let result = block_on(serve(
                &scheduler,
                server,
                &arguments,
                fingerprint,
                &cancellation,
                start,
            ));
            block_on(scheduler.shutdown());
            result
        }
    });
    (cancellation, serving)
}

#[test]
fn a_server_is_advertised_over_multicast_while_it_serves() {
    let directory = directory("advertised");
    let mut arguments = local(&directory, &["--auth", "open", "--name", "Announced"]);
    arguments.advertise = true;
    let (server, fingerprint) = configured(&arguments).unwrap();
    let (listening, port) = std::sync::mpsc::channel();
    let start = move |name: &str, port, address, counts, fingerprint| {
        listening.send(port).unwrap();
        tomori_discovery::advertise(name, port, address, counts, fingerprint)
    };
    let (cancellation, serving) = serving_with(server, arguments, fingerprint, start);
    port.recv_timeout(Duration::from_secs(10)).unwrap();
    cancellation.cancel();
    assert_eq!(serving.join().unwrap(), Ok(()));
    std::fs::remove_dir_all(directory).unwrap();
}

/// An advertisement whose responder has stopped.
struct Stopped;

impl Announce for Stopped {
    type Error = &'static str;

    fn update(&self, _: tomori_discovery::Counts) -> Result<(), Self::Error> {
        Err("the responder stopped")
    }
}

/// An advertisement whose responder announces what it is given.
struct Running;

impl Announce for Running {
    type Error = &'static str;

    fn update(&self, _: tomori_discovery::Counts) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn counts_are_announced_only_where_advertised_and_a_failure_is_said() {
    crate::test_log::record();
    announce(None::<&Stopped>, Default::default());
    announce(Some(&Running), Default::default());
    announce(Some(&Stopped), Default::default());
    // Before the server is advertised, there is nothing to announce with.
    announcer(Advertised::default())(Default::default());
    let advertised = tomori_discovery::advertise(
        "Announcing",
        48768,
        IpAddr::from([127, 0, 0, 1]),
        Default::default(),
        None,
    )
    .expect("the responder starts");
    announce(Some(&advertised), Default::default());
    assert!(!crate::test_log::logged("Bonjour: could not advertise the new counts").is_empty());
}

/// An error with a cause.
#[derive(Debug)]
struct Caused(Option<std::io::Error>);

impl Display for Caused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("accepting failed")
    }
}

impl std::error::Error for Caused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.as_ref().map(|cause| cause as _)
    }
}

#[test]
fn a_failure_says_each_of_its_causes() {
    assert_eq!(
        failure(Caused(Some(std::io::Error::other("the listener broke")))),
        "The server failed: accepting failed: the listener broke"
    );
}

#[test]
fn the_operator_is_told_of_each_pairing_event() {
    let began = crate::PairingEvent::Began {
        device: tomori_security::pairing::ChosenName::new("Quest 3"),
    };
    assert_eq!(
        began.to_string(),
        "Pairing began from a device calling itself \"Quest\"."
    );
    // 2026-11-08 03:12:05 UTC, and two minutes later.
    let began = std::time::UNIX_EPOCH + Duration::from_secs(1_794_107_525);
    let waiting = crate::PairingEvent::Waiting {
        device: tomori_security::pairing::ChosenName::new("Type 482 913 to confirm"),
        began,
        expires: began + crate::PAIRING_TIMEOUT,
    };
    assert_eq!(
        waiting.to_string(),
        "Pairing request from a device calling itself \"Type to confirm\", begun at 03:12:05 UTC; it is withdrawn at 03:14:05 UTC unless confirmed. To confirm it, type here the 6-digit code shown on the device you are pairing, never one from anywhere else, which declines it if it does not match, or type \"decline\"; owners can also confirm on the status page."
    );
    assert!(
        crate::PairingEvent::Paused
            .to_string()
            .starts_with("Pairing is paused")
    );
    let agent = crate::PairingEvent::Agent {
        notice: "The agent of space space-1 declined pairing.".to_owned(),
    };
    assert_eq!(
        agent.to_string(),
        "The agent of space space-1 declined pairing."
    );
    tell(&crate::PairingEvent::Paused);
}

/// The speech engines, loaded from the models CI fetches.
#[cfg(feature = "speech")]
mod speech {
    use super::*;
    use tomori_speech::stt::{PunctuationConfig, SileroVadConfig, TransducerConfig, WhisperConfig};
    use tomori_speech::test_models::{self, WHISPER_MODEL};
    use tomori_speech::tts::KokoroConfig;

    /// A repository named `name` with only `models` of its speech recognition models, as one missing the rest.
    fn only(name: &str, models: &[&Path]) -> PathBuf {
        let root = directory(name);
        let asr = root.join(".tools/asr");
        std::fs::create_dir_all(&asr).unwrap();
        for model in models {
            std::os::unix::fs::symlink(model, asr.join(model.file_name().unwrap())).unwrap();
        }
        root
    }

    #[test]
    fn the_streaming_recognizer_punctuates_its_transcripts_where_it_can() {
        let root = test_models::root();
        let model = TransducerConfig::from_root(&root).model;
        let punctuation = PunctuationConfig::from_root(&root).model;
        test_models::with(
            "the streaming recognizer and its punctuation",
            "scripts/fetch-asr-model.sh",
            &[&model, &punctuation],
            || {
                let mut engines = Engines::default();
                transcriber(Some(&root), "cpu", "streaming", None, &mut engines);
                let unpunctuated = only("unpunctuated", &[&model]);
                transcriber(Some(&unpunctuated), "cpu", "streaming", None, &mut engines);
                assert_eq!(engines.stoppers.len(), 2);
                assert!(engines.stop(Instant::now() + ENGINE_STOP));
                std::fs::remove_dir_all(unpunctuated).unwrap();
            },
        );
    }

    #[test]
    fn the_offline_recognizer_hears_voices_where_it_can() {
        let root = test_models::root();
        let whisper = WhisperConfig::named(&root, WHISPER_MODEL).model;
        let vad = SileroVadConfig::from_root(&root).model;
        test_models::with(
            "Whisper and the voice activity detector",
            "scripts/fetch-asr-model.sh whisper tiny.en",
            &[&whisper, &vad],
            || {
                let mut engines = Engines::default();
                transcriber(
                    Some(&root),
                    "cpu",
                    "offline",
                    Some(WHISPER_MODEL),
                    &mut engines,
                );
                let unheard = only("unheard", &[&whisper]);
                transcriber(
                    Some(&unheard),
                    "cpu",
                    "offline",
                    Some(WHISPER_MODEL),
                    &mut engines,
                );
                assert_eq!(engines.stoppers.len(), 2);
                assert!(engines.stop(Instant::now() + ENGINE_STOP));
                std::fs::remove_dir_all(unheard).unwrap();
            },
        );
    }

    #[test]
    fn replies_are_spoken_by_sentence_or_by_block() {
        let root = test_models::root();
        let kokoro = KokoroConfig::from_root(&root).model;
        test_models::with("Kokoro", "scripts/fetch-tts-model.sh", &[&kokoro], || {
            let mut engines = Engines::default();
            // With a state directory, whose pronunciations are added if it has them, and without.
            let state = directory("pronunciations");
            for (segmentation, given) in [("sentence", Some(state.as_path())), ("block", None)] {
                synthesis(
                    Server::new(Backends::new(), Arc::new(NullTranscriber::new())),
                    Some(&root),
                    segmentation,
                    given,
                    &mut engines,
                );
            }
            assert_eq!(engines.stoppers.len(), 2);
            assert!(engines.stop(Instant::now() + ENGINE_STOP));
            std::fs::remove_dir_all(state).unwrap();
        });
    }
}

#[test]
fn a_configured_server_serves_until_cancelled() {
    let directory = directory("serve");
    let mut arguments = local(&directory, &["--auth", "open", "--name", "Served"]);
    // Advertised, but by an advertiser that fails, which only tells the operator.
    arguments.advertise = true;
    let (server, fingerprint) = configured(&arguments).unwrap();
    // Counts are published as soon as they change, though not advertised.
    let server = server.with_counts_interval(Duration::from_millis(1));
    let (asked, cancellation, serving) = serving(server, arguments.clone(), fingerprint);
    let (name, port, address, fingerprint) = asked.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        (name.as_str(), address, fingerprint),
        ("Served", IpAddr::from([127, 0, 0, 1]), None)
    );

    // It is serving: a client is welcomed, and creates a space in the workspace root.
    let mut client =
        crate::test_support::TestClient::open(SocketAddr::from(([127, 0, 0, 1], port)));
    client.hello("quest", "Ada");
    client.send(&tomori_protocol::ClientMessage::CreateSpace {
        title: "Kept".into(),
        backend: "fake".into(),
        directory: directory.to_str().unwrap().into(),
        session: None,
        environment: None,
        instructions: None,
    });
    client.space(|state| state.title == "Kept");
    drop(client);

    cancellation.cancel();
    assert_eq!(serving.join().unwrap(), Ok(()));
    // The space is kept in the state directory, to be bound again when the server starts again.
    assert!(configured(&arguments).is_ok());
    let kept = crate::kept::SpaceStore::open(&directory).unwrap().spaces();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].closed, None);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_server_admitting_by_tokens_advertises_its_fingerprint() {
    let directory = directory("serve-tls");
    let mut arguments = local(&directory, &[]);
    arguments.advertise = true;
    let (server, fingerprint) = configured(&arguments).unwrap();
    let (asked, cancellation, serving) = serving(server, arguments, fingerprint.clone());
    assert_eq!(
        asked.recv_timeout(Duration::from_secs(10)).unwrap().3,
        fingerprint
    );
    cancellation.cancel();
    assert_eq!(serving.join().unwrap(), Ok(()));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_server_started_with_open_pairing_opens_it_once_it_listens() {
    let directory = directory("open-pairing");
    // A state directory only its user can read is not warned about.
    #[cfg(unix)]
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    assert!(!tomori_security::shared_directory(&directory).unwrap());
    let arguments = local(&directory, &["--open-pairing"]);
    let (server, fingerprint) = configured(&arguments).unwrap();
    let pairing = server.pairing();
    // Closed, though the server has no owner, until the operator opens it.
    assert_eq!(
        pairing.state(),
        Some(crate::PairingState::Closed {
            opens_for: crate::PAIRING_OPEN_FOR
        })
    );
    let (_, cancellation, serving) = serving(server, arguments, fingerprint);
    crate::test_support::eventually(|| {
        matches!(
            pairing.state(),
            Some(crate::PairingState::Open { by, .. }) if by == crate::Opener::startup()
        )
        .then_some(())
    });
    cancellation.cancel();
    assert_eq!(serving.join().unwrap(), Ok(()));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_address_the_server_cannot_listen_for_tools_on_is_reported() {
    let directory = directory("tools-unbound");
    // An address of the documentation range, which is on no interface here.
    let arguments = local(&directory, &["--auth", "open", "--mcp-bind", "192.0.2.1:0"]);
    let (server, fingerprint) = configured(&arguments).unwrap();
    let (_, _cancellation, serving) = serving(server, arguments, fingerprint);
    let error = serving.join().unwrap().err().unwrap();
    assert!(
        error.starts_with("Could not listen for Tomori's tools on 192.0.2.1:0: "),
        "{error}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_server_without_tools_serves_until_cancelled() {
    let directory = directory("no-tools");
    let mut arguments = local(&directory, &["--auth", "open", "--no-mcp"]);
    // Asked to advertise once it listens.
    arguments.advertise = true;
    let (server, fingerprint) = configured(&arguments).unwrap();
    let (asked, cancellation, serving) = serving(server, arguments, fingerprint);
    asked.recv_timeout(Duration::from_secs(10)).unwrap();
    cancellation.cancel();
    assert_eq!(serving.join().unwrap(), Ok(()));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_address_the_server_cannot_listen_on_is_reported() {
    let directory = directory("unbound");
    // An address of the documentation range, which is on no interface here.
    let mut arguments = local(&directory, &["--auth", "open"]);
    arguments.bind = "192.0.2.1:0".parse().unwrap();
    let (server, fingerprint) = configured(&arguments).unwrap();
    let (_, _cancellation, serving) = serving(server, arguments, fingerprint);
    let error = serving.join().unwrap().err().unwrap();
    assert!(
        error.starts_with("Could not listen on 192.0.2.1:0: "),
        "{error}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn the_auth_command_and_unusable_arguments_end_with_their_exit_code() {
    let directory = directory("run");
    let state = Some(directory.clone());
    let mut engines = Engines::default();
    let run = |list: &[&str], engines: &mut Engines| {
        super::run(
            Server::builder(),
            list.iter().map(|argument| argument.to_string()),
            state.clone(),
            engines,
        )
    };
    assert_eq!(run(&["auth", "list"], &mut engines), ExitCode::SUCCESS);
    assert_eq!(run(&["auth", "unknown"], &mut engines), ExitCode::FAILURE);
    assert_eq!(run(&["--verbose"], &mut engines), ExitCode::FAILURE);
    assert_eq!(
        run(&["--backend", "claude", "--no-advertise"], &mut engines),
        ExitCode::FAILURE
    );
    assert!(engines.stop(Instant::now()));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_server_whose_scheduler_cannot_start_says_so_and_fails() {
    let directory = directory("no-scheduler");
    let arguments = local(&directory, &["--auth", "open"]);
    let (server, fingerprint) = configured(&arguments).unwrap();
    crate::system::refuse("scheduler");
    assert_eq!(
        crate::process::serve_until_signalled(server, &arguments, fingerprint),
        Err("Could not start the scheduler: the system refused scheduler".to_owned())
    );
    std::fs::remove_dir_all(directory).unwrap();
}

/// Begin pairing as a device does, and reveal, returning the code it derives.
fn pair(
    client: &mut crate::test_support::TestClient,
    fingerprint: tomori_security::Fingerprint,
) -> String {
    use tomori_protocol::{ClientMessage, ServerMessage};
    use tomori_security::pairing::{Nonce, code};
    let device = Nonce::generate();
    client.send(&ClientMessage::PairBegin {
        commitment: device.commitment(),
    });
    let server = client.wait_for(|message| match message {
        ServerMessage::PairNonce { nonce } => Nonce::parse(nonce),
        _ => None,
    });
    client.send(&ClientMessage::PairReveal {
        nonce: device.to_hex(),
    });
    client.wait_for(|message| matches!(message, ServerMessage::PairWaiting).then_some(()));
    code(fingerprint, device, server)
}

#[test]
fn the_operator_confirms_or_declines_pairings_on_the_console() {
    let fingerprint = tomori_security::Fingerprint::of(b"console certificate");
    let (sender, receiver) = std::sync::mpsc::channel();
    let server = crate::test_support::TestServer::with_tokens(true, move |server| {
        let server = server
            .with_pairing(fingerprint)
            .with_tokens_over_plain_http_for_tests();
        sender.send(server.pairing()).unwrap();
        server
    });
    let pairing = receiver.recv().unwrap();
    let reply = |line: &str| console_reply(&pairing, line);

    assert_eq!(reply("  "), None);
    assert_eq!(reply("Allow").as_deref(), Some("Pairing is allowed again."));
    assert_eq!(reply("decline").as_deref(), Some("No pairing is waiting."));
    assert_eq!(reply("123456").as_deref(), Some("No pairing is waiting."));
    assert_eq!(reply("close").as_deref(), Some("Pairing is not open."));

    // Closed by default, though the server has no owner yet, until the operator opens it for its first device.
    let mut quest = server.connect("quest", "Quest 3");
    assert_eq!(refused(&mut quest), tomori_protocol::PAIRING_CLOSED);
    assert_eq!(
        reply("pair").as_deref(),
        Some(
            "Pairing is open for 2 minutes: choose the pairing object on the device you are pairing."
        )
    );
    let code = pair(&mut quest, fingerprint);
    assert_eq!(
        reply("the code").as_deref(),
        Some("Type the 6-digit code shown on the device you are pairing, or \"decline\".")
    );
    // A wrong code declines the pairing, since the console never shows the code that whatever types on it could search for.
    let wrong = format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000);
    assert_eq!(
        reply(&wrong).as_deref(),
        Some("That code does not match, so the pairing was declined.")
    );
    assert_eq!(pairing.pending(), None);
    assert_eq!(reply(&code).as_deref(), Some("No pairing is waiting."));
    // Typed as the device shows it, in two groups.
    let code = pair(&mut quest, fingerprint);
    assert_eq!(
        reply(&tomori_security::pairing::display(&code)).as_deref(),
        Some("Paired a device calling itself \"Quest\" as an owner.")
    );

    // The pairing closed the window, so pairing is closed until the operator opens it again: the way back in for an owner who has lost every paired device.
    let mut tablet = server.connect("tablet", "Tablet");
    assert_eq!(
        pairing.state(),
        Some(crate::PairingState::Closed {
            opens_for: crate::PAIRING_OPEN_FOR
        })
    );
    assert_eq!(refused(&mut tablet), tomori_protocol::PAIRING_CLOSED);
    assert_eq!(reply("close").as_deref(), Some("Pairing is not open."));
    assert_eq!(
        reply("PAIR").as_deref(),
        Some(
            "Pairing is open for 2 minutes: choose the pairing object on the device you are pairing."
        )
    );
    assert!(matches!(
        pairing.state(),
        Some(crate::PairingState::Open { by, .. }) if by == crate::Opener::console()
    ));
    pair(&mut tablet, fingerprint);
    assert_eq!(reply("DECLINE").as_deref(), Some("Declined the pairing."));
    assert_eq!(pairing.pending(), None);
    // Only a pairing that succeeds uses the window, so the device may try again within it, within the attempts allowed; closing it withdraws the pairing waiting.
    assert_eq!(reply("allow").as_deref(), Some("Pairing is allowed again."));
    pair(&mut tablet, fingerprint);
    assert_eq!(reply("close"), None);
    assert_eq!(
        failed(&tablet),
        "Pairing was closed, so the pairing was withdrawn."
    );
    assert_eq!(pairing.pending(), None);
    assert_eq!(refused(&mut tablet), tomori_protocol::PAIRING_CLOSED);
    assert_eq!(reply("close").as_deref(), Some("Pairing is not open."));
}

/// Ask to pair as a device does, expecting a refusal or a pairing that ends, and return why.
fn refused(client: &mut crate::test_support::TestClient) -> String {
    let device = tomori_security::pairing::Nonce::generate();
    client.send(&tomori_protocol::ClientMessage::PairBegin {
        commitment: device.commitment(),
    });
    failed(client)
}

/// Wait for a pairing to fail, returning why.
fn failed(client: &crate::test_support::TestClient) -> String {
    client.wait_for(|message| match message {
        tomori_protocol::ServerMessage::PairFailed { error } => Some(error.clone()),
        _ => None,
    })
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
fn attention_is_advertised_unless_turned_off_on_its_own() {
    let parsed = arguments(&[]).unwrap();
    assert!(parsed.advertise && parsed.advertise_attention);
    let parsed = arguments(&["--no-advertise-attention"]).unwrap();
    assert!(parsed.advertise && !parsed.advertise_attention);
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

#[cfg(feature = "speech")]
#[test]
fn the_speech_provider_must_be_one_this_build_supports() {
    assert_eq!(arguments(&[]).unwrap().speech_provider, "cpu");
    assert_eq!(
        arguments(&["--speech-provider", "cpu"])
            .unwrap()
            .speech_provider,
        "cpu"
    );
    let cuda = arguments(&["--speech-provider", "cuda"]);
    #[cfg(feature = "speech-shared")]
    assert_eq!(cuda.unwrap().speech_provider, "cuda");
    #[cfg(not(feature = "speech-shared"))]
    {
        let error = cuda.err().expect("cuda is refused without speech-shared");
        assert!(error.contains("speech-shared"), "{error}");
    }
    assert!(arguments(&["--speech-provider", "gpu"]).is_err());
    assert!(arguments(&["--speech-provider"]).is_err());
}

#[cfg(feature = "speech")]
#[test]
fn the_offline_model_is_configurable() {
    assert_eq!(arguments(&[]).unwrap().speech_offline_model, None);
    assert_eq!(
        arguments(&["--speech-offline-model", "tiny.en"])
            .unwrap()
            .speech_offline_model
            .as_deref(),
        Some("tiny.en")
    );
    assert!(arguments(&["--speech-offline-model"]).is_err());
    #[cfg(feature = "speech")]
    for name in ["", "../small.en", ".hidden", "Small.en"] {
        assert!(
            arguments(&["--speech-offline-model", name]).is_err(),
            "{name:?} was accepted"
        );
    }
}

#[test]
fn workspace_roots_and_the_space_limit_are_configurable() {
    let parsed = arguments(&[]).unwrap();
    assert!(parsed.workspace_roots.is_empty());
    assert_eq!(parsed.max_spaces, crate::DEFAULT_MAX_SPACES);
    assert_eq!(crate::DEFAULT_MAX_SPACES, 8);

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
fn admission_is_configurable() {
    let parsed = arguments(&[]).unwrap();
    assert!(!parsed.open);
    assert!(parsed.visitors);
    assert_eq!(parsed.max_visitors, crate::DEFAULT_MAX_VISITORS);
    assert_eq!(parsed.state_directory, None);
    let parsed = arguments(&[
        "--auth",
        "tokens",
        "--no-visitors",
        "--max-visitors",
        "3",
        "--state-directory",
        "/var/lib/tomori",
    ])
    .unwrap();
    assert!(!parsed.open);
    assert!(!parsed.visitors);
    assert_eq!(parsed.max_visitors, 3);
    assert_eq!(
        parsed.state_directory,
        Some(PathBuf::from("/var/lib/tomori"))
    );
    assert!(arguments(&["--auth", "open"]).unwrap().open);
    assert!(!parsed.open_pairing);
    assert!(arguments(&["--open-pairing"]).unwrap().open_pairing);
    for list in [
        &["--auth"][..],
        &["--auth", "none"],
        &["--auth", "open", "--no-visitors"],
        &["--auth", "open", "--open-pairing"],
        &["--max-visitors"],
        &["--max-visitors", "-1"],
        &["--state-directory"],
    ] {
        assert!(arguments(list).is_err(), "{list:?}");
    }
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
    let missing = std::env::temp_dir().join(format!("tomori-missing-root-{}", std::process::id()));
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
