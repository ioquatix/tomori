// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::pin::{Pin, pin};
use std::sync::Arc;

use base_http::body::{BodyError, BodyFrame, ReadableBody};
use base_scheduler::Cancellation;
use futures_util::future::{Either, select};
use protocol::{Frame, FrameDecoder};

use crate::hub::{ConnectionId, Hub};
use crate::outbox::{Outbox, Popped};

/// Outgoing data is batched into chunks of about this size when frames are already waiting.
const CHUNK_TARGET: usize = 64 * 1024;

type ReadResult<B> = Result<BodyFrame<<B as ReadableBody>::Data>, <B as ReadableBody>::Error>;

/// A request body read that survives across response reads, owning the body until it completes.
type PendingRead<B> = Pin<Box<dyn Future<Output = (B, ReadResult<B>)> + Send>>;

/// The request body, either idle or owned by a read in progress.
enum Input<B: ReadableBody> {
    Idle(B),
    Reading(PendingRead<B>),
    Closed,
}

impl<B: ReadableBody> Input<B> {
    fn insert_reading(&mut self, pending: PendingRead<B>) -> &mut PendingRead<B> {
        *self = Input::Reading(pending);
        match self {
            Input::Reading(pending) => pending,
            _ => unreachable!(),
        }
    }
}

/// Start reading `request`, returning a future that hands the body back with the result.
///
/// TODO(upstream): base-http1's `IncomingBody` poisons itself when a pending `read` is dropped ("previous body read failed or was abandoned"), although the `ReadableBody` contract says dropping a pending read must not lose data. Racing the request read against outgoing frames would drop it, so the read is kept alive here instead. Once `IncomingBody::read` is drop-safe, select on `request.read` directly and remove this.
fn start_read<'a, B: ReadableBody + 'a>(
    mut request: B,
    cancellation: Cancellation,
) -> PendingRead<B> {
    let future: Pin<Box<dyn Future<Output = (B, ReadResult<B>)> + Send + 'a>> =
        Box::pin(async move {
            let result = request.read(&cancellation).await;
            (request, result)
        });
    // SAFETY: This only erases the lifetime `'a` that the future borrows through `B`. The result is stored solely in `Input<B>` inside `ConnectionBody<B>`, whose type still names `B` and so cannot outlive `'a`; it is only ever polled or dropped there. The layout is identical because only the trait object's lifetime bound changes.
    unsafe {
        std::mem::transmute::<
            Pin<Box<dyn Future<Output = (B, ReadResult<B>)> + Send + 'a>>,
            PendingRead<B>,
        >(future)
    }
}

/// The response body of `POST /connect`: server frames out, while reading client frames from the request body it owns.
///
/// The HTTP server pulls this body to write the response. Each read returns the next outgoing frames, and while none are waiting it reads the request body and applies incoming frames to the [`Hub`]. No separate task is needed, so the connection's lifetime is exactly the HTTP exchange's. Dropping the body disconnects the participant.
pub struct ConnectionBody<B: ReadableBody> {
    hub: Arc<Hub>,
    id: ConnectionId,
    outbox: Arc<Outbox>,
    request: Input<B>,
    decoder: FrameDecoder,
    input_ended: bool,
    finished: bool,
}

impl<B: ReadableBody> ConnectionBody<B> {
    pub fn new(hub: Arc<Hub>, request: B) -> Self {
        let (id, outbox) = hub.connect();
        Self {
            hub,
            id,
            outbox,
            request: Input::Idle(request),
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
                        Frame::server(&message).encode_into(output);
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
        if let Input::Idle(request) = &mut self.request {
            request.close(None);
        }
        // Dropping a read in progress abandons the request body, which is what closing means.
        self.request = Input::Closed;
    }

    async fn read(&mut self, cancellation: &Cancellation) -> Result<BodyFrame<Vec<u8>>, BodyError> {
        loop {
            if self.finished {
                return Ok(BodyFrame::End);
            }
            let mut output = Vec::new();
            let open = self.drain(&mut output);
            if !output.is_empty() {
                if !open {
                    self.finished = true;
                }
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
            let pending = match std::mem::replace(&mut self.request, Input::Closed) {
                Input::Idle(request) => self
                    .request
                    .insert_reading(start_read(request, cancellation.clone())),
                Input::Reading(pending) => self.request.insert_reading(pending),
                Input::Closed => {
                    self.finished = true;
                    return Ok(BodyFrame::End);
                }
            };
            // When outgoing frames win, the read stays pending in `self.request`, so no input is lost.
            let received = match select(pin!(listener), pending).await {
                Either::Left(_) => None,
                Either::Right(((request, result), _)) => {
                    self.request = Input::Idle(request);
                    Some(result.map_err(BodyError::new)?)
                }
            };
            if let Some(frame) = received {
                self.input(frame)?;
            }
        }
    }
}
