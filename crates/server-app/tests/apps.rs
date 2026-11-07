// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Tomori's own programs agree on every environment they register: the `tomori-server` executable, offering the GitHub prototype too, offers Tomori's own app, `tomori::app::builder()`, and Tomori's own spectator, `tomori_client_spectator::app::sceneries()`, exactly what it offers a client that lists nothing, which is exactly what each presents, at the same identifiers and versions. Each environment's identity is in its own crate, so a server half and a client half whose identities drift apart fail here. See `design/environments.md`.
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use tomori_client::connection::Session;
use tomori_protocol::EnvironmentVersion;
use tomori_render::scenery::Sceneries;
use tomori_scene::conversation::Conversation;
use tomori_server::test_support::TestClient;

const SERVER: &str = env!("CARGO_BIN_EXE_tomori-server");

/// What a server says on its standard error, line by line.
type Said = std::io::Lines<BufReader<std::process::ChildStderr>>;

/// The `tomori-server` executable, offering the GitHub prototype with a token it never uses, since nobody watches its cards, its address, and what it says after it listens, which is read until it stops, so that it can say it.
fn serve(state: &std::path::Path) -> (Child, std::net::SocketAddr, Said) {
    let state = state.to_str().unwrap();
    let mut server = Command::new(SERVER)
        .args([
            "--bind",
            "127.0.0.1:0",
            "--no-advertise",
            "--auth",
            "open",
            "--github-repository",
            "ioquatix/tomori",
            "--state-directory",
            state,
            "--workspace-root",
            state,
        ])
        .env("GITHUB_TOKEN", "not-a-real-token")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut said = BufReader::new(server.stderr.take().unwrap()).lines();
    let listening = said
        .by_ref()
        .map(Result::unwrap)
        .find(|line| line.starts_with("Tomori server listening on http://"))
        .expect("the server says where it listens");
    let address = listening
        .trim_start_matches("Tomori server listening on http://")
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    (server, address, said)
}

/// Stop `server` as an operator does, so that it exits in order, writing what it measured of itself.
#[cfg(unix)]
fn stop(server: &mut Child, said: Said) {
    // SAFETY: `kill` only sends a signal to the child.
    assert_eq!(
        unsafe { libc::kill(server.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let rest: Vec<String> = said.map(Result::unwrap).collect();
    assert!(server.wait().unwrap().success(), "{rest:?}");
}

/// Stop `server`.
#[cfg(not(unix))]
fn stop(server: &mut Child, said: Said) {
    server.kill().unwrap();
    server.wait().unwrap();
    drop(said);
}

/// What a client drawing `sceneries` lists in its `Hello`, as its conversation does.
fn presented(sceneries: &Sceneries) -> Vec<EnvironmentVersion> {
    let mut conversation = Conversation::new("Ada");
    conversation.set_drawable(sceneries.environments());
    conversation.set_over_grid(sceneries.over_grid());
    conversation
        .environments()
        .expect("a conversation lists what it presents")
}

#[test]
fn tomoris_own_server_offers_tomoris_own_app_and_spectator_exactly_what_they_present() {
    let state = std::env::temp_dir().join(format!("tomori-server-app-apps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let (mut server, address, said) = serve(&state);
    let offered = |presents: Option<Vec<EnvironmentVersion>>| {
        let mut client = TestClient::open(address);
        let mut offered: Vec<(String, u32)> = client
            .hello_presenting("ada", "Ada", presents)
            .into_iter()
            .map(|info| (info.id, info.version))
            .collect();
        offered.sort();
        offered
    };
    // A client that lists nothing is offered every one, at the server's versions: Sky, the grid, the farmyard and GitHub.
    let everything = offered(None);
    assert_eq!(
        everything,
        [
            ("farmyard".to_owned(), 1),
            ("github".to_owned(), 1),
            ("minimal".to_owned(), 1),
            ("sky".to_owned(), 1)
        ]
    );
    let mut app = tomori::app::builder();
    for (name, presents) in [
        ("tomori", presented(app.sceneries())),
        (
            "tomori-spectator",
            presented(&tomori_client_spectator::app::sceneries()),
        ),
    ] {
        // Each presents every one at the server's version, so it is offered every one, and the server offers every
        // one it presents.
        assert_eq!(offered(Some(presents.clone())), everything, "{name}");
        let mut presents: Vec<(String, u32)> = presents
            .into_iter()
            .map(|presented| (presented.id, presented.version))
            .collect();
        presents.sort();
        assert_eq!(presents, everything, "{name}");
    }
    stop(&mut server, said);
    std::fs::remove_dir_all(state).unwrap();
}
