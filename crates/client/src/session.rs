// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The connection thread: one duplex exchange at a time, reconnecting with backoff.

use std::cell::Cell;
use std::convert::Infallible;
use std::future::Future;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

use base_http::body::{BodyFrame, ReadableBody};
use base_http::{Method, RequestMetadata};
use base_http_client::{Client as HttpClient, Endpoint, StreamingResponseBody};
use base_scheduler::{Cancellation, Clock};
use base_scheduler_direct::DirectScheduler;
use futures_lite::future::{or, zip};
use protocol::{
    AudioFrame, CONNECT_PATH, CONTENT_TYPE, ClientMessage, DEFAULT_PORT, Frame, FrameDecoder,
    FrameKind, PROTOCOL_VERSION, ServerMessage,
};

use crate::client::Shared;
use crate::outbox::Batch;
use crate::{ConnectionState, Disconnect, Event, Options};

/// The most encoded bytes written to the request body at once.
const BATCH_LIMIT: usize = 32 * 1024;

pub(crate) struct Context<'a> {
    pub scheduler: &'a DirectScheduler,
    pub shared: &'a Shared,
    pub options: &'a Options,
    pub events: &'a mpsc::Sender<Event>,
    pub audio: &'a SyncSender<AudioFrame>,
}

impl Context<'_> {
    fn set_state(&self, state: ConnectionState) {
        self.shared.set_state(state, self.events);
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
        let welcomed = Cell::new(false);
        let reason = session(context, &welcomed).await;
        context.shared.outbox.disconnect();
        if context.shared.outbox.is_closed() {
            break;
        }
        failures = if welcomed.get() { 1 } else { failures + 1 };
        let retry_in = context.options.backoff.delay(failures);
        context.set_state(ConnectionState::Disconnected { reason, retry_in });
        let _ = context
            .scheduler
            .sleep(retry_in, &context.shared.closing)
            .await;
    }
    context.set_state(ConnectionState::Closed);
}

fn resolve(address: &str) -> Result<SocketAddr, Disconnect> {
    let addresses = if address.contains(':') {
        address.to_socket_addrs()
    } else {
        (address, DEFAULT_PORT).to_socket_addrs()
    };
    addresses
        .map_err(|error| Disconnect::Address(error.to_string()))?
        .next()
        .ok_or_else(|| Disconnect::Address(format!("{address} has no addresses")))
}

fn transport(error: impl std::fmt::Display) -> Disconnect {
    Disconnect::Transport(error.to_string())
}

/// Run one connection until it ends, returning why.
async fn session(context: &Context<'_>, welcomed: &Cell<bool>) -> Disconnect {
    let address = match resolve(&context.options.address) {
        Ok(address) => address,
        Err(reason) => return reason,
    };
    let (client, driver) = HttpClient::new(
        context.scheduler,
        Endpoint::new(address),
        Default::default(),
    )
    .expect("the default HTTP client options are valid");
    let lifetime = Cancellation::new();
    let exchange = async {
        let reason = or(exchange(context, &client, &lifetime, welcomed), async {
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
    welcomed: &Cell<bool>,
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
    let request = context
        .within(context.options.connect_timeout, async {
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
            let mut reader = FrameReader {
                body: &mut response.body,
                decoder: FrameDecoder::new(),
                ended: false,
                lifetime,
            };
            let first = context
                .within(context.options.connect_timeout, reader.next())
                .await?;
            welcome(context, first)?;
            welcomed.set(true);
            loop {
                let frame = reader.next().await?;
                deliver(context, frame)?;
            }
        }
        .await;
        match result {
            Err(reason) => reason,
        }
    };

    or(upload, download).await
}

fn welcome(context: &Context<'_>, frame: Frame) -> Result<(), Disconnect> {
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
            Ok(())
        }
        ServerMessage::Error { error } => Err(Disconnect::Protocol(format!(
            "server refused the connection: {error}"
        ))),
        other => Err(Disconnect::Protocol(format!(
            "expected welcome, received {other:?}"
        ))),
    }
}

fn deliver(context: &Context<'_>, frame: Frame) -> Result<(), Disconnect> {
    let protocol = |error: protocol::FrameError| Disconnect::Protocol(error.to_string());
    match frame.kind {
        FrameKind::Control => match frame.to_server_message().map_err(protocol)? {
            ServerMessage::Welcome { .. } => {
                return Err(Disconnect::Protocol("unexpected second welcome".into()));
            }
            message => {
                let _ = context.events.send(Event::Message(message));
            }
        },
        FrameKind::Audio => {
            // Playback is real time: if the application falls behind, newer audio is dropped rather than queued without bound.
            let _ = context.audio.try_send(frame.to_audio().map_err(protocol)?);
        }
    }
    Ok(())
}

struct FrameReader<'a, 'scope> {
    body: &'a mut StreamingResponseBody<'scope>,
    decoder: FrameDecoder,
    ended: bool,
    lifetime: &'a Cancellation,
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
                    self.decoder.push(data.as_ref());
                    end
                }
                BodyFrame::Headers { end, .. } => end,
                BodyFrame::End => true,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_addresses_with_the_default_port() {
        assert_eq!(
            resolve("127.0.0.1").unwrap(),
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT))
        );
        assert_eq!(
            resolve("127.0.0.1:9").unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 9))
        );
        assert!(matches!(
            resolve("invalid host name"),
            Err(Disconnect::Address(_))
        ));
    }
}
