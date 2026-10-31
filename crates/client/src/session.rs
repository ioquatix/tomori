// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The connection thread: one duplex exchange at a time, reconnecting with backoff.

use std::cell::Cell;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use base_http::body::{BodyFrame, ReadableBody};
use base_http::{Method, RequestMetadata};
use base_http_client::{Client as HttpClient, Endpoint, StreamingResponseBody};
use base_scheduler::{Cancellation, Clock, Timers};
use base_scheduler_direct::DirectScheduler;
use futures_lite::future::{or, zip};
use protocol::{
    AudioFrame, CONNECT_PATH, CONTENT_TYPE, ClientMessage, Frame, FrameDecoder, FrameKind,
    PROTOCOL_VERSION, ServerMessage,
};

use crate::client::Shared;
use crate::outbox::Batch;
use crate::resolve::resolve_in_background;
use crate::{ConnectionState, Disconnect, Event, Options};

/// The most encoded bytes written to the request body at once.
const BATCH_LIMIT: usize = 32 * 1024;

pub(crate) struct Context<'a> {
    pub scheduler: &'a DirectScheduler,
    pub shared: &'a Shared,
    pub options: &'a Options,
    pub audio: &'a SyncSender<AudioFrame>,
}

impl Context<'_> {
    fn set_state(&self, state: ConnectionState) {
        self.shared.set_state(state);
    }

    /// Fail with [`Disconnect::Timeout`] if `future` does not finish within `duration`.
    async fn within<T>(
        &self,
        duration: Duration,
        future: impl Future<Output = Result<T, Disconnect>>,
    ) -> Result<T, Disconnect> {
        or(future, async {
            let _ = self.scheduler.sleep(duration, &Cancellation::never()).await;
            Err(Disconnect::Timeout)
        })
        .await
    }
}

pub(crate) async fn run(context: &Context<'_>) {
    let mut failures = 0;
    while !context.shared.outbox.is_closed() {
        context.set_state(ConnectionState::Connecting {
            attempt: failures + 1,
        });
        let welcomed = Cell::new(None);
        let reason = session(context, &welcomed).await;
        let failed = context.shared.outbox.disconnect();
        context.shared.report(failed);
        if context.shared.outbox.is_closed() {
            break;
        }
        let welcomed_for = welcomed
            .get()
            .map(|since| context.scheduler.now().saturating_duration_since(since));
        failures = context
            .options
            .backoff
            .failures_after(failures, welcomed_for);
        let retry_in = context.options.backoff.delay(failures);
        context.set_state(ConnectionState::Disconnected { reason, retry_in });
        let _ = context
            .scheduler
            .sleep(retry_in, &context.shared.closing)
            .await;
    }
    context.set_state(ConnectionState::Closed);
}

fn transport(error: impl std::fmt::Display) -> Disconnect {
    Disconnect::Transport(error.to_string())
}

/// Run one connection until it ends, returning why. Resolution and connecting share the connect timeout; each resolved address is tried in turn until one accepts the connection.
async fn session(context: &Context<'_>, welcomed: &Cell<Option<Instant>>) -> Disconnect {
    let deadline = context.scheduler.now() + context.options.connect_timeout;
    // Closing does not wait for a slow lookup.
    let resolution = or(
        context.within(
            context.options.connect_timeout,
            resolve_in_background(&context.options.address),
        ),
        async {
            context.shared.closing.cancelled().await;
            Err(Disconnect::Closed)
        },
    );
    let addresses = match resolution.await {
        Ok(addresses) => addresses,
        Err(reason) => return reason,
    };
    let mut reason = Disconnect::Timeout;
    for address in addresses {
        let opened = Cell::new(false);
        reason = connection(context, address, deadline, &opened, welcomed).await;
        // Only an address that refused the connection is skipped: once the request is open, queued messages may have been taken for it.
        let refused = !opened.get() && matches!(reason, Disconnect::Transport(_));
        if !refused || context.shared.outbox.is_closed() || context.scheduler.now() >= deadline {
            break;
        }
    }
    reason
}

/// Run one connection to `address` until it ends, returning why. `opened` is set once the request is open.
async fn connection(
    context: &Context<'_>,
    address: SocketAddr,
    deadline: Instant,
    opened: &Cell<bool>,
    welcomed: &Cell<Option<Instant>>,
) -> Disconnect {
    let (client, driver) = HttpClient::new(
        context.scheduler,
        Endpoint::new(address),
        Default::default(),
    )
    .expect("the default HTTP client options are valid");
    let lifetime = Cancellation::new();
    let exchange = async {
        let exchange = exchange(context, &client, &lifetime, deadline, opened, welcomed);
        let reason = or(exchange, async {
            // Closing lets the exchange deliver queued control messages, but only for so long.
            context.shared.closing.cancelled().await;
            let _ = context
                .scheduler
                .sleep(context.options.close_timeout, &Cancellation::never())
                .await;
            Disconnect::Closed
        })
        .await;
        client.begin_close();
        lifetime.cancel();
        reason
    };
    let (reason, _) = zip(exchange, driver.run(&lifetime)).await;
    reason
}

async fn exchange(
    context: &Context<'_>,
    client: &HttpClient<'_>,
    lifetime: &Cancellation,
    deadline: Instant,
    opened: &Cell<bool>,
    welcomed: &Cell<Option<Instant>>,
) -> Disconnect {
    let mut metadata = RequestMetadata::new(Method::POST);
    metadata.path = Some(CONNECT_PATH.into());
    metadata
        .fields
        .set("content-type", CONTENT_TYPE)
        .expect("the content type is a valid field value");
    metadata
        .fields
        .set("accept", CONTENT_TYPE)
        .expect("the content type is a valid field value");
    let remaining = deadline.saturating_duration_since(context.scheduler.now());
    let request = context
        .within(remaining, async {
            client
                .open_request(metadata, lifetime)
                .await
                .map_err(transport)
        })
        .await;
    let (mut output, response) = match request {
        Ok(request) => request.split(),
        Err(reason) => return reason,
    };
    opened.set(true);

    let upload = async {
        let options = context.options;
        let hello = ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            participant: options.participant.clone(),
            name: options.name.clone(),
            credentials: options.credentials.clone(),
        };
        let mut bytes = Frame::client(&hello).encode();
        for message in context.shared.outbox.connect() {
            Frame::client(&message).encode_into(&mut bytes);
        }
        let result: Result<(), Disconnect> = async {
            output.write(bytes, lifetime).await.map_err(transport)?;
            while let Batch::Frames(bytes) = context.shared.outbox.next_batch(BATCH_LIMIT).await {
                output.write(bytes, lifetime).await.map_err(transport)?;
            }
            output.finish(lifetime).await.map_err(transport)
        }
        .await;
        match result {
            Err(reason) => reason,
            // Closing, with queued control sent: wait for the server to end its response, which `session` bounds by the close timeout.
            Ok(()) => std::future::pending().await,
        }
    };

    let download = async {
        let result: Result<Infallible, Disconnect> = async {
            let mut response = context
                .within(context.options.connect_timeout, async {
                    response.await.map_err(transport)
                })
                .await?;
            if !response.status.is_success() {
                return Err(Disconnect::Status(response.status.as_u16()));
            }
            let received = Cell::new(context.scheduler.now());
            let mut reader = FrameReader {
                body: &mut response.body,
                decoder: FrameDecoder::new(),
                ended: false,
                lifetime,
                scheduler: context.scheduler,
                received: &received,
            };
            let first = context
                .within(context.options.connect_timeout, reader.next())
                .await?;
            let source = welcome(context, first)?;
            welcomed.set(Some(context.scheduler.now()));
            let frames = async {
                loop {
                    let frame = reader.next().await?;
                    deliver(context, frame, source)?;
                }
            };
            or(frames, async { Err(keepalive(context, &received).await) }).await
        }
        .await;
        match result {
            Err(reason) => reason,
        }
    };

    or(upload, download).await
}

/// Ping the server whenever nothing has been received for the keepalive interval, and end the connection with [`Disconnect::Timeout`] once nothing has been received for the keepalive timeout. `received` is when data last arrived.
async fn keepalive(context: &Context<'_>, received: &Cell<Instant>) -> Disconnect {
    let scheduler = context.scheduler;
    let keepalive = context.options.keepalive;
    let mut timer = scheduler.timer();
    let mut nonce = 0;
    let mut pinged: Option<Instant> = None;
    loop {
        let last = received.get();
        // Ping once the link has been quiet for an interval, and again each interval after that.
        let ping_at = match pinged {
            Some(pinged) if pinged >= last => pinged + keepalive.interval,
            _ => last + keepalive.interval,
        };
        let deadline = ping_at.min(last + keepalive.timeout);
        // Data arriving meanwhile does not wake this; the next wake simply finds a later deadline.
        if let Err(error) = scheduler
            .timer_wait_until(&mut timer, deadline, &Cancellation::never())
            .await
        {
            return transport(error);
        }
        let now = scheduler.now();
        let last = received.get();
        if now >= last + keepalive.timeout {
            return Disconnect::Timeout;
        }
        if now >= ping_at && now >= last + keepalive.interval {
            context
                .shared
                .outbox
                .keepalive(ClientMessage::Ping { nonce });
            nonce = nonce.wrapping_add(1);
            pinged = Some(now);
        }
    }
}

/// Check the server's Welcome, returning this client's participant number.
fn welcome(context: &Context<'_>, frame: Frame) -> Result<u32, Disconnect> {
    let message = frame
        .to_server_message()
        .map_err(|error| Disconnect::Protocol(error.to_string()))?;
    match message {
        ServerMessage::Welcome {
            protocol,
            server,
            source,
        } => {
            if protocol != PROTOCOL_VERSION {
                return Err(Disconnect::Protocol(format!(
                    "server speaks protocol {protocol}, client speaks {PROTOCOL_VERSION}"
                )));
            }
            context.set_state(ConnectionState::Connected { source, server });
            Ok(source)
        }
        ServerMessage::Error { error, .. } => Err(Disconnect::Protocol(format!(
            "server refused the connection: {error}"
        ))),
        other => Err(Disconnect::Protocol(format!(
            "expected welcome, received {other:?}"
        ))),
    }
}

/// Hand one frame to the application. `source` is this client's participant number on this connection.
fn deliver(context: &Context<'_>, frame: Frame, source: u32) -> Result<(), Disconnect> {
    let protocol = |error: protocol::FrameError| Disconnect::Protocol(error.to_string());
    match frame.kind {
        FrameKind::Control => match frame.to_server_message().map_err(protocol)? {
            ServerMessage::Welcome { .. } => {
                return Err(Disconnect::Protocol("unexpected second welcome".into()));
            }
            // Keepalive is handled here and not reported: receiving it already counted as liveness.
            ServerMessage::Ping { nonce } => context
                .shared
                .outbox
                .keepalive(ClientMessage::Pong { nonce }),
            ServerMessage::Pong { .. } => {}
            message => {
                match &message {
                    ServerMessage::Space { state } => {
                        context.shared.outbox.joined(Some(state.id.clone()))
                    }
                    ServerMessage::Left => context.shared.outbox.joined(None),
                    _ => {}
                }
                context.shared.events.push(Event::Message(message));
            }
        },
        FrameKind::Audio => {
            let audio = frame.to_audio().map_err(protocol)?;
            // What this participant records never plays back to them. The server does not relay it back; this guards against a server that does.
            if audio.source == source {
                return Ok(());
            }
            // Playback is real time: if the application falls behind, newer audio is dropped rather than queued without bound.
            let _ = context.audio.try_send(audio);
        }
    }
    Ok(())
}

struct FrameReader<'a, 'scope> {
    body: &'a mut StreamingResponseBody<'scope>,
    decoder: FrameDecoder,
    ended: bool,
    lifetime: &'a Cancellation,
    scheduler: &'a DirectScheduler,
    /// When data last arrived: any data counts as liveness, even part of a frame.
    received: &'a Cell<Instant>,
}

impl FrameReader<'_, '_> {
    /// The next complete frame, or why the response ended.
    async fn next(&mut self) -> Result<Frame, Disconnect> {
        let protocol = |error: protocol::FrameError| Disconnect::Protocol(error.to_string());
        loop {
            if let Some(frame) = self.decoder.next_frame().map_err(protocol)? {
                return Ok(frame);
            }
            if self.ended {
                self.decoder.finish().map_err(protocol)?;
                return Err(Disconnect::Closed);
            }
            self.ended = match self.body.read(self.lifetime).await.map_err(transport)? {
                BodyFrame::Data { data, end } => {
                    self.received.set(self.scheduler.now());
                    self.decoder.push(data.as_ref());
                    end
                }
                BodyFrame::Headers { end, .. } => end,
                BodyFrame::End => true,
            };
        }
    }
}
