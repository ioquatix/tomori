// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A minimal Tomori server on `base-http-server`: it answers each `/connect` request body with a response body that streams concurrently, frame by frame.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use base_endpoint::{AddressEndpoint, BindEndpoint, BindOptions};
use base_http::body::{BodyError, BodyFrame, ReadableBody};
use base_http::{InterimHandler, InterimResponses, Request, Response, Status};
use base_http_server::Handler;
use base_scheduler::{Cancellation, SocketLocalAddress};
use base_scheduler_direct::DirectScheduler;
use futures_lite::future::block_on;
use protocol::{
    AudioFrame, CONNECT_PATH, ClientMessage, Frame, FrameDecoder, FrameKind, PROTOCOL_VERSION,
    ServerMessage, SpaceState,
};

/// The source the mini server assigns to echoed audio.
pub const ECHO_SOURCE: u32 = 99;

/// What the client sent, by connection number (counting from one).
pub type Log = Arc<Mutex<Vec<(u32, Received)>>>;

#[derive(Clone, Debug, PartialEq)]
pub enum Received {
    Message(ClientMessage),
    Audio(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Welcome with an incompatible protocol version.
    WrongProtocol,
    /// Respond 503 to every connection.
    Unavailable,
    /// Stop reading the request body after Hello, so the client's writes back up.
    Stall,
    /// Echo each audio frame back attributed to the client itself, as well as to [`ECHO_SOURCE`].
    EchoToSender,
}

struct Mini {
    mode: Mode,
    connections: AtomicU32,
    log: Log,
}

impl Handler for Mini {
    type Error = Infallible;
    type Body<B: ReadableBody> = Session<B>;

    async fn call<B: ReadableBody, I: InterimHandler>(
        &self,
        request: Request<B>,
        _: &mut InterimResponses<I>,
        _: &Cancellation,
    ) -> Result<Response<Session<B>>, Infallible> {
        let path_matches = request.path.as_deref() == Some(CONNECT_PATH);
        let status = if self.mode == Mode::Unavailable || !path_matches {
            Status::SERVICE_UNAVAILABLE
        } else {
            Status::OK
        };
        let connection = self.connections.fetch_add(1, Ordering::SeqCst) + 1;
        let ended = status != Status::OK;
        Ok(Response::new(
            status,
            Session {
                body: request.body,
                decoder: FrameDecoder::new(),
                output: VecDeque::new(),
                ended,
                greeted: false,
                connection,
                mode: self.mode,
                log: self.log.clone(),
            },
        ))
    }
}

/// Transforms the request body into the response body: each client frame produces zero or more server frames.
pub struct Session<B> {
    body: B,
    decoder: FrameDecoder,
    output: VecDeque<Vec<u8>>,
    ended: bool,
    greeted: bool,
    connection: u32,
    mode: Mode,
    log: Log,
}

impl<B: ReadableBody> Session<B> {
    fn record(&self, received: Received) {
        self.log.lock().unwrap().push((self.connection, received));
    }

    fn reply(&mut self, message: ServerMessage) {
        self.output.push_back(Frame::server(&message).encode());
    }

    fn handle(&mut self, frame: Frame) {
        match frame.kind {
            FrameKind::Audio => {
                let mut audio: AudioFrame = frame.to_audio().unwrap();
                self.record(Received::Audio(audio.sequence));
                if self.mode == Mode::EchoToSender {
                    // The client's source is its connection number; see the Welcome below.
                    audio.source = self.connection;
                    self.output.push_back(Frame::audio(&audio).encode());
                }
                audio.source = ECHO_SOURCE;
                self.output.push_back(Frame::audio(&audio).encode());
            }
            FrameKind::Control => {
                let message = frame.to_client_message().unwrap();
                self.record(Received::Message(message.clone()));
                match message {
                    ClientMessage::Hello { .. } => {
                        self.greeted = true;
                        let protocol = if self.mode == Mode::WrongProtocol {
                            PROTOCOL_VERSION + 1
                        } else {
                            PROTOCOL_VERSION
                        };
                        self.reply(ServerMessage::Welcome {
                            protocol,
                            server: "mini".into(),
                            source: self.connection,
                        });
                    }
                    ClientMessage::Join { space } => self.reply(ServerMessage::Space {
                        state: Box::new(SpaceState {
                            id: space,
                            ..Default::default()
                        }),
                    }),
                    ClientMessage::Leave => self.reply(ServerMessage::Left),
                    ClientMessage::ListSpaces => {
                        self.reply(ServerMessage::Spaces { spaces: Vec::new() })
                    }
                    ClientMessage::Ping { nonce } => self.reply(ServerMessage::Pong { nonce }),
                    // A test hook: end the response, which disconnects the client.
                    ClientMessage::Answer { text, .. } if text == "kick" => self.ended = true,
                    // A test hook: ping the client, which answers with a Pong.
                    ClientMessage::Answer { text, .. } if text == "ping" => {
                        self.reply(ServerMessage::Ping { nonce: 7 })
                    }
                    _ => {}
                }
            }
        }
    }
}

impl<B: ReadableBody> ReadableBody for Session<B> {
    type Data = Vec<u8>;
    type Error = BodyError;

    fn close(&mut self, _: Option<BodyError>) {
        self.body.close(None);
    }

    async fn read(&mut self, cancellation: &Cancellation) -> Result<BodyFrame<Vec<u8>>, BodyError> {
        loop {
            if self.output.is_empty() && self.ended {
                return Ok(BodyFrame::End);
            }
            if !self.output.is_empty() {
                let mut data = Vec::new();
                while let Some(bytes) = self.output.pop_front() {
                    data.extend(bytes);
                }
                return Ok(BodyFrame::Data { data, end: false });
            }
            if self.mode == Mode::Stall && self.greeted {
                cancellation.cancelled().await;
                return Err(BodyError::new(std::io::Error::other("stalled")));
            }
            match self.body.read(cancellation).await.map_err(BodyError::new)? {
                BodyFrame::Data { data, end } => {
                    self.decoder.push(data.as_ref());
                    while let Some(frame) = self.decoder.next_frame().unwrap() {
                        self.handle(frame);
                    }
                    self.ended |= end;
                }
                BodyFrame::Headers { end, .. } => self.ended |= end,
                BodyFrame::End => self.ended = true,
            }
        }
    }
}

pub struct MiniServer {
    pub address: SocketAddr,
    pub log: Log,
    stop: Cancellation,
    thread: Option<JoinHandle<()>>,
}

impl MiniServer {
    pub fn start(address: SocketAddr, mode: Mode) -> Self {
        let log = Log::default();
        let stop = Cancellation::new();
        let (bound, receive) = mpsc::channel();
        let handler = Mini {
            mode,
            connections: AtomicU32::new(0),
            log: log.clone(),
        };
        let lifetime = stop.clone();
        let thread = std::thread::spawn(move || {
            let scheduler = DirectScheduler::new(Default::default()).unwrap();
            block_on(async {
                let listener = AddressEndpoint::new(address)
                    .bind(&scheduler, BindOptions::default(), &lifetime)
                    .await
                    .unwrap()
                    .remove(0);
                bound
                    .send(scheduler.socket_local_address(&listener).unwrap())
                    .unwrap();
                let _ = base_http_server::serve(
                    &scheduler,
                    listener,
                    &handler,
                    &Default::default(),
                    &lifetime,
                )
                .await;
            });
            block_on(scheduler.shutdown());
        });
        Self {
            address: receive.recv().unwrap(),
            log,
            stop,
            thread: Some(thread),
        }
    }

    /// What connection `connection` received, in order.
    pub fn received(&self, connection: u32) -> Vec<Received> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(number, _)| *number == connection)
            .map(|(_, received)| received.clone())
            .collect()
    }
}

impl Drop for MiniServer {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
