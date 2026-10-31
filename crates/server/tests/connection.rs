// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use protocol::{Activity, ClientMessage, Frame, Gate, Keepalive, QueueState, Role, ServerMessage};
use server::Server;
use server::fake::FakeBackend;
use server::test_support::{TIMEOUT, TestServer, TextTranscriber};

#[test]
fn health_reports_the_protocol_version() {
    let server = TestServer::start();
    let (status, body) = server.get("/health");
    assert_eq!(status, 200);
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["protocol"], protocol::PROTOCOL_VERSION);
    assert_eq!(server.get("/missing").0, 404);
}

#[test]
fn hello_is_answered_with_a_welcome_and_the_lobby() {
    let server = TestServer::start();
    let ada = server.connect("ada", "Ada");
    let bob = server.connect("bob", "Bob");
    assert_ne!(ada.source, bob.source);
    let spaces = ada.wait_for(|message| match message {
        ServerMessage::Spaces { spaces } => Some(spaces.clone()),
        _ => None,
    });
    assert!(spaces.is_empty());
}

#[test]
fn messages_before_hello_are_rejected() {
    let server = TestServer::start();
    let mut client = server_client(&server);
    client.send(&ClientMessage::ListSpaces);
    assert_eq!(client.error(), "Send hello first.");
}

fn server_client(server: &TestServer) -> server::test_support::TestClient {
    server::test_support::TestClient::open(server.address)
}

#[test]
fn spoken_input_is_delivered_and_the_reply_streams_back() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    let space = ada.create_space("Demo");
    assert_eq!(space.activity, Activity::Idle);
    assert_eq!(space.participants.len(), 1);

    ada.say("u1", "hello there");
    let transcript = ada.wait_for(|message| match message {
        ServerMessage::Transcript {
            utterance,
            text,
            finished: true,
        } if utterance == "u1" => Some(text.clone()),
        _ => None,
    });
    assert_eq!(transcript, "hello there");

    let state = ada.space(|state| {
        state.activity == Activity::Idle
            && state
                .messages
                .iter()
                .any(|message| message.completed && message.role == Role::Assistant)
    });
    assert_eq!(state.queue[0].state, QueueState::Delivered);
    assert_eq!(state.messages[0].text, "Ada: hello there");
    assert_eq!(state.messages[1].text, "You said: Ada: hello there");

    let prompts = server.backend.started()[0].log().prompts;
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].text, "Ada: hello there");
}

#[test]
fn ending_the_upload_ends_the_connection() {
    let server = TestServer::start();
    let mut ada = server.connect("ada", "Ada");
    ada.finish();
    assert!(ada.is_closed(TIMEOUT));
}

#[test]
fn a_silent_client_is_released_and_keeps_its_identity() {
    let keepalive = Keepalive {
        interval: Duration::from_millis(50),
        timeout: Duration::from_millis(500),
    };
    let server = TestServer::with_server(FakeBackend::new(), move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_keepalive(keepalive)
    });
    // Ada says hello, creates a space and then sends nothing at all, like a headset that left the network.
    let mut ada = server.connect("ada", "Ada");
    let space = ada.create_space("Demo").id;
    let mut bob = server.connect("bob", "Bob");
    bob.send(&ClientMessage::Join {
        space: space.clone(),
    });
    bob.space(|state| state.participants.len() == 2);

    // The server answers a ping at once, before anything else it has queued.
    bob.send(&ClientMessage::Ping { nonce: 42 });
    let pong = bob.wait_for(|message| match message {
        ServerMessage::Pong { nonce } => Some(*nonce),
        _ => None,
    });
    assert_eq!(pong, 42);

    // Bob stays live by answering the server's pings; Ada is released from the space.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "Ada was never released");
        match bob.next_message(Duration::from_millis(10)) {
            Some(ServerMessage::Ping { nonce }) => bob.send(&ClientMessage::Pong { nonce }),
            Some(ServerMessage::Space { state })
                if state
                    .participants
                    .iter()
                    .all(|participant| participant.id != "ada") =>
            {
                break;
            }
            _ => {}
        }
    }
    // Ada was pinged before the timeout, and her connection has ended.
    ada.wait_for(|message| matches!(message, ServerMessage::Ping { .. }).then_some(()));
    assert!(ada.is_closed(TIMEOUT));

    // Reconnecting keeps her identity and rejoins the space.
    let again = server.connect("ada", "Ada");
    assert_eq!(again.source, ada.source);
    let state = again.space(|state| state.id == space);
    assert_eq!(state.participants.len(), 2);
}

/// One HTTP/1.1 chunk carrying `message`.
fn chunk(message: &ClientMessage) -> Vec<u8> {
    let frame = Frame::client(message).encode();
    let mut chunk = format!("{:x}\r\n", frame.len()).into_bytes();
    chunk.extend(frame);
    chunk.extend(b"\r\n");
    chunk
}

/// Connect Ada over a raw socket, say hello and join `space`, without reading anything.
fn raw_join(server: &TestServer, space: &str) -> TcpStream {
    let mut stream = TcpStream::connect(server.address).unwrap();
    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
        protocol::CONNECT_PATH,
        protocol::CONTENT_TYPE
    )
    .into_bytes();
    request.extend(chunk(&ClientMessage::Hello {
        protocol: protocol::PROTOCOL_VERSION,
        participant: "ada".into(),
        name: "Ada".into(),
        credentials: None,
    }));
    request.extend(chunk(&ClientMessage::Join {
        space: space.to_owned(),
    }));
    stream.write_all(&request).unwrap();
    stream
}

#[test]
fn a_client_that_stops_reading_is_released_even_while_writes_are_blocked() {
    let keepalive = Keepalive {
        interval: Duration::from_millis(50),
        timeout: Duration::from_millis(500),
    };
    let server = TestServer::with_server(FakeBackend::new(), move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_keepalive(keepalive)
    });
    let mut bob = server.connect("bob", "Bob");
    let space = bob.create_space("Demo").id;

    // Ada then neither reads nor writes, so the server's writes to her back up.
    let mut ada = raw_join(&server, &space);
    bob.space(|state| state.participants.len() == 2);

    // Bob talks without pause, far beyond what the socket buffers hold, until Ada is released.
    let deadline = Instant::now() + TIMEOUT;
    'talking: loop {
        assert!(Instant::now() < deadline, "Ada was never released");
        for _ in 0..100 {
            bob.send_audio(Gate::Live, vec![1; 320]);
        }
        while let Some(message) = bob.next_message(Duration::ZERO) {
            if let ServerMessage::Space { state } = message
                && state.participants.len() == 1
            {
                break 'talking;
            }
        }
    }

    // The server abandoned the blocked write and closed the socket: draining it reaches the end.
    ada.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match ada.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => break,
            Err(error) => panic!("the server kept the connection open: {error}"),
        }
    }
}

#[test]
fn a_client_that_sends_but_never_reads_is_released() {
    let keepalive = Keepalive {
        interval: Duration::from_millis(100),
        timeout: Duration::from_millis(500),
    };
    let server = TestServer::with_server(FakeBackend::new(), move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_keepalive(keepalive)
    });
    let mut bob = server.connect("bob", "Bob");
    let space = bob.create_space("Demo").id;
    let ada = raw_join(&server, &space);
    bob.space(|state| state.participants.len() == 2);

    // Ada keeps sending, but never reads what the server sends her.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sender = {
        let mut ada = ada.try_clone().unwrap();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut nonce = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                nonce += 1;
                if ada
                    .write_all(&chunk(&ClientMessage::Ping { nonce }))
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };

    // Bob talks without pause, so the server's writes to Ada back up, until she is released.
    let deadline = Instant::now() + TIMEOUT;
    'talking: loop {
        assert!(Instant::now() < deadline, "Ada was never released");
        for _ in 0..100 {
            bob.send_audio(Gate::Live, vec![1; 320]);
        }
        while let Some(message) = bob.next_message(Duration::ZERO) {
            if let ServerMessage::Space { state } = message
                && state.participants.len() == 1
            {
                break 'talking;
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sender.join().unwrap();
}

/// A server whose connections must complete a request head, or say Hello, within `timeout`.
fn with_request_timeout(timeout: Duration) -> TestServer {
    TestServer::with_server(FakeBackend::new(), move |backends| {
        Server::new(backends, Arc::new(TextTranscriber::default())).with_request_timeout(timeout)
    })
}

/// Make one plain `GET /health` on `stream` and return the status line.
fn health(mut stream: TcpStream) -> String {
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response.lines().next().unwrap_or_default().to_owned()
}

/// Whether the server has closed `stream` within `timeout`.
fn closed_within(stream: &mut TcpStream, timeout: Duration) -> bool {
    stream.set_read_timeout(Some(timeout)).unwrap();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return true,
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => return true,
            Err(_) => return false,
        }
    }
}

#[test]
fn silent_sockets_do_not_hold_connection_slots() {
    let server = with_request_timeout(Duration::from_millis(300));
    // More silent sockets than the server serves at once; the rest wait in the listen backlog.
    // Connecting faster than the server accepts can briefly overflow the listen backlog, which resets a connection attempt; try again.
    let connect = || {
        for _ in 0..50 {
            match TcpStream::connect(server.address) {
                Ok(stream) => return stream,
                Err(error) if error.kind() == ErrorKind::ConnectionReset => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("connecting failed: {error}"),
            }
        }
        panic!("connecting kept being reset");
    };
    let silent: Vec<TcpStream> = (0..300).map(|_| connect()).collect();
    std::thread::sleep(Duration::from_millis(100));
    let fresh = TcpStream::connect(server.address).unwrap();
    assert_eq!(health(fresh), "HTTP/1.1 200 OK");
    drop(silent);
}

#[test]
fn idle_and_unintroduced_connections_are_closed() {
    let server = with_request_timeout(Duration::from_millis(300));

    // A keep-alive connection that goes idle after its first response.
    let mut keep_alive = TcpStream::connect(server.address).unwrap();
    write!(
        keep_alive,
        "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    assert!(closed_within(&mut keep_alive, TIMEOUT));

    // A request head that never completes.
    let mut trickle = TcpStream::connect(server.address).unwrap();
    write!(trickle, "GET /health HTTP/1.1\r\n").unwrap();
    assert!(closed_within(&mut trickle, TIMEOUT));

    // A connection that only answers keepalive and never says Hello.
    let mut client = server::test_support::TestClient::open(server.address);
    client.send(&ClientMessage::Ping { nonce: 1 });
    assert!(client.is_closed(TIMEOUT));

    // A participant who said Hello stays connected.
    let ada = server.connect("ada", "Ada");
    assert!(!ada.is_closed(Duration::from_millis(600)));
}
