// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Helpers for end-to-end tests: a server on an ephemeral port with the fake backend, and a blocking client speaking raw HTTP/1.1 chunked framing over TCP.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base_scheduler::{Cancellation, SocketLocalAddress};
use base_scheduler_direct::{DirectOptions, DirectScheduler};
use futures_lite::future::block_on;
use protocol::{
    AudioFrame, ClientMessage, Frame, FrameDecoder, FrameKind, Gate, PROTOCOL_VERSION,
    ServerMessage, SpaceState,
};

use crate::fake::{FAKE_BACKEND_NAME, FakeBackend};
use crate::{Backends, Server, Transcriber, TranscriptSink, UtteranceId};

/// How long test helpers wait for an expected message.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// A transcriber that reads each sample as a Unicode scalar value, so tests can "speak" exact text. See [`speech`].
#[derive(Default)]
pub struct TextTranscriber {
    utterances: Mutex<HashMap<UtteranceId, (TranscriptSink, String)>>,
}

/// Samples that [`TextTranscriber`] transcribes as `text`.
pub fn speech(text: &str) -> Vec<i16> {
    text.chars()
        .map(|character| i16::try_from(u32::from(character)).unwrap_or(0x3f))
        .collect()
}

impl Transcriber for TextTranscriber {
    fn begin(&self, utterance: UtteranceId, sink: TranscriptSink) {
        self.utterances
            .lock()
            .unwrap()
            .insert(utterance, (sink, String::new()));
    }

    fn push(&self, utterance: UtteranceId, frame: &AudioFrame) {
        let partial = {
            let mut utterances = self.utterances.lock().unwrap();
            let Some((sink, text)) = utterances.get_mut(&utterance) else {
                return;
            };
            text.extend(
                frame
                    .samples
                    .iter()
                    .filter_map(|&sample| char::from_u32(u32::from(sample as u16))),
            );
            (sink.clone(), text.clone())
        };
        partial.0.partial(&partial.1);
    }

    fn end(&self, utterance: UtteranceId) {
        let finished = self.utterances.lock().unwrap().remove(&utterance);
        if let Some((sink, text)) = finished {
            sink.finish(&text);
        }
    }

    fn cancel(&self, utterance: UtteranceId) {
        self.utterances.lock().unwrap().remove(&utterance);
    }
}

/// A server running on its own thread, listening on an ephemeral loopback port. Dropping it shuts the server down.
pub struct TestServer {
    pub address: SocketAddr,
    pub backend: FakeBackend,
    cancellation: Cancellation,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    /// A server whose fake sessions answer every prompt immediately.
    pub fn start() -> Self {
        Self::with_backend(FakeBackend::new())
    }

    pub fn with_backend(backend: FakeBackend) -> Self {
        let cancellation = Cancellation::new();
        let (sender, receiver) = channel();
        let thread = {
            let backend = backend.clone();
            let cancellation = cancellation.clone();
            std::thread::spawn(move || {
                let scheduler = DirectScheduler::new(DirectOptions {
                    threads: 2,
                    capacity: 256,
                })
                .expect("the scheduler starts");
                let server = Server::new(
                    Backends::new().with(backend),
                    Arc::new(TextTranscriber::default()),
                );
                block_on(async {
                    let address = "127.0.0.1:0".parse().unwrap();
                    let listener = crate::bind(&scheduler, address, &cancellation)
                        .await
                        .expect("the listener binds");
                    sender
                        .send(scheduler.socket_local_address(&listener).unwrap())
                        .unwrap();
                    if let Err(error) = server.run(&scheduler, listener, &cancellation).await {
                        panic!("the server failed: {error}");
                    }
                });
                block_on(scheduler.shutdown());
            })
        };
        let address = receiver.recv().expect("the server starts");
        Self {
            address,
            backend,
            cancellation,
            thread: Some(thread),
        }
    }

    /// Open a connection, say hello and wait for the welcome.
    pub fn connect(&self, participant: &str, name: &str) -> TestClient {
        let mut client = TestClient::open(self.address);
        client.hello(participant, name);
        client
    }

    /// Make one plain request and return the status code and body.
    pub fn get(&self, path: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(self.address).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response[9..12].parse().unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default();
        (status, body)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.expect("the server thread exits cleanly");
            }
        }
    }
}

/// A blocking client connection. Received control messages and audio frames are queued separately.
pub struct TestClient {
    stream: TcpStream,
    messages: Receiver<ServerMessage>,
    audio: Receiver<AudioFrame>,
    /// The audio source number assigned in the welcome.
    pub source: u32,
    sequence: u32,
}

impl TestClient {
    /// Open `POST /connect` without saying hello.
    pub fn open(address: SocketAddr) -> Self {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        write!(
            stream,
            "POST {} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
            protocol::CONNECT_PATH,
            protocol::CONTENT_TYPE
        )
        .unwrap();
        let (message_sender, messages) = channel();
        let (audio_sender, audio) = channel();
        let reader = stream.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(
                line.starts_with("HTTP/1.1 200"),
                "unexpected status {line:?}"
            );
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let mut decoder = FrameDecoder::new();
            while let Some(chunk) = read_chunk(&mut reader) {
                decoder.push(&chunk);
                while let Some(frame) = decoder.next_frame().unwrap() {
                    let delivered = match frame.kind {
                        FrameKind::Control => message_sender
                            .send(frame.to_server_message().unwrap())
                            .is_ok(),
                        FrameKind::Audio => audio_sender.send(frame.to_audio().unwrap()).is_ok(),
                    };
                    if !delivered {
                        return;
                    }
                }
            }
        });
        Self {
            stream,
            messages,
            audio,
            source: 0,
            sequence: 0,
        }
    }

    pub fn hello(&mut self, participant: &str, name: &str) {
        self.send(&ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            participant: participant.to_owned(),
            name: name.to_owned(),
            credentials: None,
        });
        self.source = self.wait_for(|message| match message {
            ServerMessage::Welcome { source, .. } => Some(*source),
            _ => None,
        });
    }

    fn write_chunk(&mut self, data: &[u8]) {
        let mut chunk = format!("{:x}\r\n", data.len()).into_bytes();
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(b"\r\n");
        self.stream.write_all(&chunk).unwrap();
    }

    pub fn send(&mut self, message: &ClientMessage) {
        self.write_chunk(&Frame::client(message).encode());
    }

    /// Send one audio frame with the next sequence number.
    pub fn send_audio(&mut self, gate: Gate, samples: Vec<i16>) {
        let frame = AudioFrame {
            gate,
            source: 0,
            sequence: self.sequence,
            timestamp: u64::from(self.sequence) * 20_000,
            sample_rate: 16_000,
            samples,
        };
        self.sequence += 1;
        self.write_chunk(&Frame::audio(&frame).encode());
    }

    /// Skip sequence numbers, as if frames were lost.
    pub fn skip_sequence(&mut self, count: u32) {
        self.sequence += count;
    }

    /// Speak `text` to the agent as one push-to-talk utterance.
    pub fn say(&mut self, utterance: &str, text: &str) {
        self.send(&ClientMessage::UtteranceBegin {
            utterance: utterance.to_owned(),
        });
        self.send_audio(Gate::Agent, speech(text));
        self.send(&ClientMessage::UtteranceEnd {
            utterance: utterance.to_owned(),
        });
    }

    /// End the upload; the server ends the response after sending what is queued.
    pub fn finish(&mut self) {
        self.stream.write_all(b"0\r\n\r\n").unwrap();
    }

    /// The next control message, or `None` after `timeout`.
    pub fn next_message(&self, timeout: Duration) -> Option<ServerMessage> {
        self.messages.recv_timeout(timeout).ok()
    }

    /// Skip messages until `select` returns a value. Panics after [`TIMEOUT`].
    pub fn wait_for<T>(&self, mut select: impl FnMut(&ServerMessage) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .next_message(remaining)
                .expect("timed out waiting for a matching message");
            if let Some(value) = select(&message) {
                return value;
            }
        }
    }

    /// Wait for a space snapshot satisfying `predicate`.
    pub fn space(&self, mut predicate: impl FnMut(&SpaceState) -> bool) -> SpaceState {
        self.wait_for(|message| match message {
            ServerMessage::Space { state } if predicate(state) => Some((**state).clone()),
            _ => None,
        })
    }

    /// Wait for an error message and return its text.
    pub fn error(&self) -> String {
        self.wait_for(|message| match message {
            ServerMessage::Error { error } => Some(error.clone()),
            _ => None,
        })
    }

    /// Create a fake space; the creator joins it. Returns its first snapshot.
    pub fn create_space(&mut self, title: &str) -> SpaceState {
        self.send(&ClientMessage::CreateSpace {
            title: title.to_owned(),
            backend: FAKE_BACKEND_NAME.to_owned(),
            directory: "/tmp".to_owned(),
            session: None,
        });
        let title = title.to_owned();
        self.space(|state| state.title == title)
    }

    pub fn next_audio(&self, timeout: Duration) -> Option<AudioFrame> {
        self.audio.recv_timeout(timeout).ok()
    }

    /// Whether the server has ended the response.
    pub fn is_closed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self
                .messages
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(_) => continue,
                Err(RecvTimeoutError::Disconnected) => return true,
                Err(RecvTimeoutError::Timeout) => return false,
            }
        }
    }
}

/// Read one chunk of a chunked body, or `None` at the end of the body or connection.
fn read_chunk(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let size = line.trim_end().split(';').next()?;
    let size = usize::from_str_radix(size, 16).ok()?;
    if size == 0 {
        return None;
    }
    let mut chunk = vec![0; size + 2];
    reader.read_exact(&mut chunk).ok()?;
    chunk.truncate(size);
    Some(chunk)
}
