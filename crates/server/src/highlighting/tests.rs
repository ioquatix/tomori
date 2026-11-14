// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use futures_lite::future::block_on;
use tomori_protocol::{
    CODE_CHARS, CODE_LINES, ClientMessage, PROTOCOL_VERSION, Projection, ServerMessage, TokenKind,
};

use super::*;
use crate::Server;
use crate::fake::FakeBackend;
use crate::stage::Found;
use crate::test_support::{TIMEOUT, TestClient, TestServer, TextTranscriber};

/// A reply of a code panel of two lines, the first `marker`.
fn code_reply(marker: &str) -> String {
    format!("Here:\n\n```rust\n{marker}\nfn f() {{}}\n```\n")
}

/// A highlighter that says what it is given and then waits to be let go, once for each text, or for at most
/// [`TIMEOUT`], so that a test that fails does not wait for it for ever, before it finds a keyword at the start of it.
struct Gated {
    /// Receives the text each highlighting is given, as it starts.
    started: Receiver<String>,
    /// Lets one highlighting go.
    release: Sender<()>,
}

impl Gated {
    fn new() -> (Self, Highlighter) {
        let (started, starting) = channel();
        let (release, released) = channel();
        let released = Mutex::new(released);
        let highlighter: Highlighter = Arc::new(move |_: &str, text: &str| {
            started.send(text.to_owned()).unwrap();
            let _ = released.lock().unwrap().recv_timeout(TIMEOUT);
            vec![Span::new(0, 2, TokenKind::Keyword)]
        });
        let gated = Self {
            started: starting,
            release,
        };
        (gated, highlighter)
    }

    /// The text the next highlighting is given, once it starts.
    fn next(&self) -> String {
        self.started
            .recv_timeout(TIMEOUT)
            .expect("highlighting starts")
    }
}

/// A server that highlights with `highlighter`.
fn server(backend: &FakeBackend, highlighter: Highlighter) -> TestServer {
    TestServer::with_server(backend.clone(), move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_highlighter(highlighter)
    })
}

/// A client of `server`, as `participant`, that reads what is projected.
fn projecting(server: &TestServer, participant: &str) -> TestClient {
    let mut client = TestClient::open(server.address);
    client.send(&ClientMessage::Hello {
        protocol: PROTOCOL_VERSION,
        participant: participant.to_owned(),
        name: participant.to_owned(),
        names: false,
        environments: None,
        environment_state: false,
        projection: true,
        pairing_window: false,
        speech_held: false,
        images: false,
        diagrams: false,
        world_edges: false,
        projection_patches: false,
        live_panels: false,
        spawns: false,
        limits: None,
    });
    client.wait_for(|message| matches!(message, ServerMessage::Welcome { .. }).then_some(()));
    client
}

/// The next projection `client` is sent that `matches`.
fn projection(client: &TestClient, matches: impl Fn(&Projection) -> bool) -> Projection {
    client.wait_for(|message| match message {
        ServerMessage::Projection { projection } if matches(projection) => {
            Some((**projection).clone())
        }
        _ => None,
    })
}

/// Whether `projection` shows a code panel whose first line is `marker`.
fn shows(projection: &Projection, marker: &str) -> bool {
    projection
        .panels
        .iter()
        .any(|panel| panel.code.is_some() && panel.body.starts_with(marker))
}

#[test]
fn highlighting_holds_no_lock_so_other_spaces_carry_on_meanwhile() {
    let backend = FakeBackend::new();
    let (gated, highlighter) = Gated::new();
    let server = server(&backend, highlighter);
    let mut ada = projecting(&server, "ada");
    ada.create_space("Code");
    let session = backend.started().pop().unwrap();
    session.reply(&code_reply("fn slow() {}"));
    // While a reply's code is being highlighted, another participant makes a space of their own and is told of it,
    // which needs the hub's lock.
    assert_eq!(gated.next(), "fn slow() {}\nfn f() {}");
    let mut bob = server.connect("bob", "Bob");
    assert_eq!(bob.create_space("Other").title, "Other");
    // Nothing of the reply is projected until its code is highlighted, and then its code panel is, highlighted.
    gated.release.send(()).unwrap();
    let projected = projection(&ada, |projection| shows(projection, "fn slow() {}"));
    let code = projected.panels[0].code.as_ref().unwrap();
    assert_eq!(code.spans, [Span::new(0, 2, TokenKind::Keyword)]);
}

#[test]
fn replies_are_projected_in_order_however_long_each_takes_to_highlight() {
    let backend = FakeBackend::new();
    let (gated, highlighter) = Gated::new();
    let server = server(&backend, highlighter);
    let mut ada = projecting(&server, "ada");
    ada.create_space("Code");
    let session = backend.started().pop().unwrap();
    // Two replies in quick succession, the first slow to highlight.
    session.reply(&code_reply("fn first() {}"));
    session.reply(&code_reply("fn second() {}"));
    assert!(gated.next().starts_with("fn first() {}"));
    // The second is not highlighted while the first is.
    assert_eq!(gated.started.try_recv(), Err(TryRecvError::Empty));
    gated.release.send(()).unwrap();
    // The first is projected, highlighted, before the second is highlighted.
    assert!(gated.next().starts_with("fn second() {}"));
    let first = projection(&ada, |projection| shows(projection, "fn first() {}"));
    assert!(!shows(&first, "fn second() {}"));
    assert!(!first.panels[0].code.as_ref().unwrap().spans.is_empty());
    // And then the second, whose code panel replaces it.
    gated.release.send(()).unwrap();
    let second = projection(&ada, |projection| shows(projection, "fn second() {}"));
    assert_eq!(second.panels.len(), 1);
}

/// The panels of a reply of `text`, with nobody asking.
fn found(text: &str) -> Vec<(Vec<Found>, Option<tomori_protocol::Pose>)> {
    vec![(crate::stage::panels(text), None)]
}

/// Each language and text a highlighter was given.
type Given = Arc<Mutex<Vec<(String, String)>>>;

/// The server's highlighters, recording each language and text they are given.
fn recording() -> (Highlighter, Given) {
    let given = Arc::new(Mutex::new(Vec::new()));
    let highlighter: Highlighter = {
        let given = given.clone();
        Arc::new(move |language: &str, text: &str| {
            given
                .lock()
                .unwrap()
                .push((language.to_owned(), text.to_owned()));
            highlighter()(language, text)
        })
    };
    (highlighter, given)
}

#[test]
fn panels_without_code_are_projected_without_highlighting() {
    let (recording, given) = recording();
    let projecting = Projecting::new(found("```panel\nNotes.\n```\n"));
    let replies = block_on(highlight(projecting, recording));
    assert_eq!(replies[0].0[0].0.body, "Notes.");
    assert!(given.lock().unwrap().is_empty());
}

#[test]
fn code_is_plain_when_it_cannot_be_highlighted() {
    crate::test_log::record();
    // When no thread can be started for it.
    crate::system::refuse("tomori-highlight");
    let (recording, given) = recording();
    let projecting = Projecting::new(found(&code_reply("fn f() {}")));
    let replies = block_on(highlight(projecting, recording));
    assert!(given.lock().unwrap().is_empty());
    let code = replies[0].0[0].0.code.as_ref().unwrap();
    assert!(code.spans.is_empty());
    assert_eq!(code.lines, 2);
    // When the highlighter fails.
    let failing: Highlighter = Arc::new(|_: &str, _: &str| panic!("the highlighter failed"));
    let projecting = Projecting::new(found(&code_reply("fn f() {}")));
    let replies = block_on(highlight(projecting, failing));
    assert!(replies[0].0[0].0.code.as_ref().unwrap().spans.is_empty());
    assert!(!crate::test_log::logged("Showing code panels plain").is_empty());
}

#[test]
#[cfg_attr(not(feature = "highlight"), ignore = "needs the server's highlighters")]
fn only_the_code_of_the_last_reply_with_any_is_highlighted_and_within_bounds() {
    // An oversized reply: more code blocks than are projected, each far longer and wider than is shown.
    let line = format!("let s = \"{}\";", "é".repeat(1_000));
    let block = format!("```rust\n{}```\n", format!("{line}\n").repeat(300));
    let huge = block.repeat(6);
    // Then a reply with a text panel alone, which replaces no code panels.
    let asker = tomori_protocol::Pose {
        position: [1.0, 0.0, 0.0],
        orientation: [0.0, 0.0, 0.0, 1.0],
    };
    let replies = vec![
        (crate::stage::panels(&code_reply("fn earlier() {}")), None),
        (crate::stage::panels(&huge), Some(asker)),
        (crate::stage::panels("```panel\nNotes.\n```\n"), None),
    ];
    let (recording, given) = recording();
    let replies = block_on(highlight(Projecting::new(replies), recording));
    // Only the oversized reply's code is highlighted, since its code panels replace the earlier reply's as they are
    // projected together, and only as much of each block as is kept: its first lines, cut.
    let given = given.lock().unwrap();
    assert_eq!(given.len(), 4);
    for (language, text) in given.iter() {
        assert_eq!(language, "rust");
        // More than is shown, so that what is shown is highlighted as it continues, and no more than the highlighter
        // takes.
        let characters = text.chars().filter(|c| *c != '\n').count();
        assert!(characters > CODE_CHARS && characters <= 2 * CODE_CHARS);
        assert!(text.lines().count() <= CODE_LINES);
        // The highlighters' own bound, which a server built without them has none of.
        #[cfg(feature = "highlight")]
        assert!(text.len() <= tomori_highlight::INPUT_BYTES);
    }
    assert!(replies[0].0[0].0.code.as_ref().unwrap().spans.is_empty());
    assert_eq!(replies[1].1, Some(asker));
    for (content, _) in &replies[1].0 {
        let code = content.code.as_ref().unwrap();
        assert_eq!(code.lines, 300);
        assert!(
            code.spans
                .iter()
                .any(|span| span.kind == TokenKind::Keyword)
        );
        assert!(tomori_protocol::valid_spans(&content.body, &code.spans));
    }
    assert_eq!(replies[2].0[0].0.body, "Notes.");
}
