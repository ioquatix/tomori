// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base_http::body::{BodyError, BodyFrame, ReadableBody};
use base_scheduler::Cancellation;
use futures_util::FutureExt;
use futures_util::future::{Either, select};
use protocol::{Frame, FrameDecoder, ServerMessage};

use crate::hub::{ConnectionId, Hub, Output};
use crate::outbox::{Outbox, Popped};

/// Outgoing data is batched into chunks of about this size when frames are already waiting.
const CHUNK_TARGET: usize = 64 * 1024;

/// The most request reads applied before each outgoing chunk when both directions are busy.
const INPUT_READS_PER_OUTPUT: usize = 8;

/// The response body of `POST /connect`: server frames out, while reading client frames from the request body it owns.
///
/// The HTTP server pulls this body to write the response. Each read returns the next outgoing frames, and while none are waiting it reads the request body and applies incoming frames to the [`Hub`]. No separate task is needed, so the connection's lifetime is exactly the HTTP exchange's. Dropping the body disconnects the participant.
pub struct ConnectionBody<B: ReadableBody> {
    hub: Arc<Hub>,
    id: ConnectionId,
    outbox: Arc<Outbox>,
    /// Set whenever request data arrives, for the hub's keepalive sweep.
    received: Arc<AtomicBool>,
    /// Tracks whether output is being written, for the hub's keepalive sweep.
    output: Arc<Output>,
    /// The request body. Its reads are raced against outgoing frames and dropped when those win, which the `ReadableBody` contract allows: a dropped read loses no input.
    request: B,
    decoder: FrameDecoder,
    input_ended: bool,
    finished: bool,
}

impl<B: ReadableBody> ConnectionBody<B> {
    /// `cancellation` must end only this connection's HTTP exchange; the hub cancels it when the connection goes silent. See [`Hub::sweep`].
    pub fn new(hub: Arc<Hub>, request: B, cancellation: Cancellation) -> Self {
        let (id, outbox, received, output) = hub.connect(cancellation);
        Self {
            hub,
            id,
            outbox,
            received,
            output,
            request,
            decoder: FrameDecoder::new(),
            input_ended: false,
            finished: false,
        }
    }

    /// Append waiting outgoing frames to `output`, up to the chunk target. Returns false once the outbox is closed and drained.
    fn drain(&mut self, output: &mut Vec<u8>) -> bool {
        while output.len() < CHUNK_TARGET {
            match self.outbox.pop() {
                Popped::Control(item) => {
                    if let Some(message) = self.hub.render(self.id, item) {
                        encode(&message, output);
                    }
                }
                Popped::Audio(frame) => output.extend_from_slice(&frame),
                Popped::Empty => break,
                Popped::Closed => return false,
            }
        }
        true
    }

    fn input<D: AsRef<[u8]>>(&mut self, frame: BodyFrame<D>) -> Result<(), BodyError> {
        let end = frame.is_end();
        if let BodyFrame::Data { data, .. } = &frame {
            // Any data counts as liveness, even part of a frame.
            self.received.store(true, Ordering::Release);
            self.decoder.push(data.as_ref());
            while let Some(frame) = self.decoder.next_frame().map_err(BodyError::new)? {
                self.hub.receive(self.id, frame);
            }
        }
        if end {
            self.decoder.finish().map_err(BodyError::new)?;
            self.input_ended = true;
        }
        Ok(())
    }

    /// Apply request data that has already arrived, without waiting. Output is only written when the HTTP server asks for it, so without this a connection whose outbox never empties would never read its input, and would look silent to the keepalive sweep.
    fn take_ready_input(&mut self, cancellation: &Cancellation) -> Result<(), BodyError> {
        // Bounded, so that a fast sender cannot starve output either.
        for _ in 0..INPUT_READS_PER_OUTPUT {
            if self.input_ended {
                break;
            }
            // A read that is not ready is dropped, losing nothing; the next real wait reads again with a real waker.
            let Some(result) = self.request.read(cancellation).now_or_never() else {
                break;
            };
            self.input(result.map_err(BodyError::new)?)?;
        }
        Ok(())
    }
}

/// Append `message` as a frame. A message that would exceed the frame limit, which the server's size limits should prevent, is replaced by a short error so that the client is not disconnected by a frame it must reject.
fn encode(message: &ServerMessage, output: &mut Vec<u8>) {
    if let Err(error) = Frame::server(message).try_encode_into(output) {
        log::error!("A server message was not sent: {error}.");
        Frame::server(&ServerMessage::Error {
            error: "A message from the server was too large to send.".to_owned(),
            space: None,
        })
        .encode_into(output);
    }
}

impl<B: ReadableBody> Drop for ConnectionBody<B> {
    fn drop(&mut self) {
        self.hub.disconnect(self.id);
    }
}

impl<B: ReadableBody> ReadableBody for ConnectionBody<B> {
    type Data = Vec<u8>;
    type Error = BodyError;

    fn is_ready(&self) -> bool {
        self.finished || self.outbox.is_ready()
    }

    fn may_have_trailers(&self) -> bool {
        false
    }

    fn close(&mut self, _error: Option<BodyError>) {
        self.finished = true;
        self.request.close(None);
    }

    async fn read(&mut self, cancellation: &Cancellation) -> Result<BodyFrame<Vec<u8>>, BodyError> {
        // The HTTP server reads the body again only once the previous chunk is written. Until then, the request body is not read either, so input from the peer is not seen while a write is blocked; the keepalive sweep releases a connection whose write makes no progress.
        self.output.asked();
        loop {
            if self.finished {
                return Ok(BodyFrame::End);
            }
            self.take_ready_input(cancellation)?;
            let mut output = Vec::new();
            let open = self.drain(&mut output);
            if !output.is_empty() {
                if !open {
                    self.finished = true;
                }
                self.output.handed_out();
                return Ok(BodyFrame::Data {
                    data: output,
                    end: !open,
                });
            }
            if !open || self.input_ended {
                // The client finished its upload, or the server ended the connection: end the response once drained.
                self.finished = true;
                return Ok(BodyFrame::End);
            }
            let listener = self.outbox.listen();
            if self.outbox.is_ready() {
                continue;
            }
            // When outgoing frames win, the pending read is dropped, losing no input; the next wait reads again from the same position.
            let received = match select(pin!(listener), pin!(self.request.read(cancellation))).await
            {
                Either::Left(_) => None,
                Either::Right((result, _)) => Some(result.map_err(BodyError::new)?),
            };
            if let Some(frame) = received {
                self.input(frame)?;
            }
        }
    }
}
