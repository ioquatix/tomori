// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Spaces run only beneath the workspace roots, and only a limited number at once. See `design/workspaces.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use backend::Event;
use protocol::{ClientMessage, ServerMessage};
use server::fake::FakeBackend;
use server::test_support::{TIMEOUT, TestClient, TestServer, TextTranscriber};
use server::{DIRECTORY_UNAVAILABLE, Server, WorkspaceRoots};

/// A directory removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("tomori-workspaces-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn directory(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start(backend: FakeBackend, roots: &[&Path], limit: usize) -> TestServer {
    let roots = WorkspaceRoots::new(roots).unwrap();
    TestServer::with_server(backend, move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default()))
            .with_workspace_roots(roots)
            .with_max_spaces(limit)
    })
}

fn create(client: &mut TestClient, title: &str, directory: &str, session: Option<&str>) {
    client.send(&ClientMessage::CreateSpace {
        title: title.to_owned(),
        backend: "fake".to_owned(),
        directory: directory.to_owned(),
        session: session.map(str::to_owned),
    });
}

/// Wait until `count` sessions are waiting to be created.
fn wait_creating(backend: &FakeBackend, count: u64) {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while backend.creating() != count {
        assert!(
            std::time::Instant::now() < deadline,
            "creation never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn refused_directories_all_get_the_same_reply() {
    let scratch = Scratch::new("refused");
    let root = scratch.directory("root");
    let other = scratch.directory("other");
    scratch.directory("root2");
    scratch.directory("root/project");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&other, root.join("escape")).unwrap();
    let backend = FakeBackend::new();
    let server = start(backend.clone(), &[&root], 8);
    let mut ada = server.connect("ada", "Ada");

    let root_text = root.to_str().unwrap();
    let mut refused = vec![
        // Outside every root.
        other.to_str().unwrap().to_owned(),
        "/".to_owned(),
        // A sibling that shares the root's prefix.
        format!("{root_text}2"),
        // Traversal out of the root.
        format!("{root_text}/project/../../other"),
        // Missing paths, inside and outside the root.
        format!("{root_text}/missing"),
        "/does/not/exist".to_owned(),
        "relative".to_owned(),
    ];
    if cfg!(unix) {
        // A link inside the root to a directory outside it.
        refused.push(format!("{root_text}/escape"));
    }
    for directory in &refused {
        create(&mut ada, "Refused", directory, None);
        assert_eq!(ada.error(), DIRECTORY_UNAVAILABLE, "{directory}");
        // Binding an existing session is checked the same way.
        create(&mut ada, "Refused", directory, Some("fake-1"));
        assert_eq!(ada.error(), DIRECTORY_UNAVAILABLE, "{directory}");
    }
    assert!(
        backend.started().is_empty(),
        "a refused directory started a session"
    );

    // A directory beneath the root is accepted, and the space shows its canonical form.
    create(
        &mut ada,
        "Project",
        &format!("{root_text}/project/./"),
        None,
    );
    let state = ada.space(|state| state.title == "Project");
    assert_eq!(state.directory, root.join("project").to_str().unwrap());
}

#[test]
fn concurrent_creates_cannot_pass_the_space_limit() {
    let scratch = Scratch::new("limit");
    let root = scratch.directory("root");
    let directory = root.to_str().unwrap().to_owned();
    let backend = FakeBackend::new();
    backend.hold_creation();
    let server = start(backend.clone(), &[&root], 2);
    let mut clients: Vec<TestClient> = ["ada", "bob", "carol"]
        .iter()
        .map(|name| server.connect(name, name))
        .collect();

    // Three creates while none has finished starting: only two may start.
    for (index, client) in clients.iter_mut().enumerate() {
        create(client, &format!("Space {index}"), &directory, None);
    }
    wait_creating(&backend, 2);
    let refusals: Vec<String> = clients
        .iter()
        .filter_map(|client| {
            let mut found = None;
            while let Some(message) = client.next_message(Duration::from_millis(300)) {
                if let ServerMessage::Error { error } = message {
                    found = Some(error);
                }
            }
            found
        })
        .collect();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains("limit of 2 spaces"), "{refusals:?}");
    assert_eq!(backend.creating(), 2);
    backend.release_creation();
    let deadline = std::time::Instant::now() + TIMEOUT;
    while backend.started().len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "the spaces never started"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Running spaces count too.
    let mut dave = server.connect("dave", "Dave");
    create(&mut dave, "Over", &directory, None);
    assert!(dave.error().contains("limit of 2 spaces"));

    // Once a session closes, its place is free again.
    backend.started()[0].emit(Event::Closed { reason: None });
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        create(&mut dave, "Again", &directory, None);
        let created = dave.wait_for(|message| match message {
            ServerMessage::Space { state } if state.title == "Again" => Some(true),
            ServerMessage::Error { error } if error.contains("limit") => Some(false),
            _ => None,
        });
        if created {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the closed space kept its place"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(backend.started().len(), 3);
}

#[test]
fn a_refused_directory_does_not_keep_a_place() {
    let scratch = Scratch::new("refused-place");
    let root = scratch.directory("root");
    let server = start(FakeBackend::new(), &[&root], 1);
    let mut ada = server.connect("ada", "Ada");
    for _ in 0..3 {
        create(&mut ada, "Refused", "/", None);
        assert_eq!(ada.error(), DIRECTORY_UNAVAILABLE);
    }
    create(&mut ada, "Allowed", root.to_str().unwrap(), None);
    ada.space(|state| state.title == "Allowed");
}

#[test]
fn a_refused_directory_holds_no_place_while_it_is_checked() {
    let scratch = Scratch::new("pipelined");
    let root = scratch.directory("root");
    let server = start(FakeBackend::new(), &[&root], 1);
    let mut ada = server.connect("ada", "Ada");
    let missing = format!("{}/missing", root.to_str().unwrap());
    // Sent together, so the refused requests are still being checked when the allowed one arrives. Fewer missing paths than the limit on running checks, which would refuse the allowed one too.
    for _ in 0..server::MAX_RESOLVING / 2 {
        create(&mut ada, "Refused", "/", None);
        create(&mut ada, "Refused", &missing, None);
    }
    create(&mut ada, "Allowed", root.to_str().unwrap(), None);
    ada.wait_for(|message| match message {
        ServerMessage::Space { state } if state.title == "Allowed" => Some(()),
        ServerMessage::Error { error } if error == DIRECTORY_UNAVAILABLE => None,
        ServerMessage::Error { error } => panic!("the allowed space was refused: {error}"),
        _ => None,
    });
}
