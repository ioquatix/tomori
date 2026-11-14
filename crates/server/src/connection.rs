// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base_http::body::{BodyError, BodyFrame, ReadableBody};
use base_scheduler::Cancellation;
use futures_util::FutureExt;
use futures_util::future::{Either, select};
use tomori_protocol::{Frame, FrameDecoder, FrameError, ServerMessage};

use crate::access::Access;
use crate::hub::{ConnectionId, Hub, Output};
use crate::inbound::{Pace, Verdict};
use crate::outbox::{Outbox, Popped};

/// Outgoing data is batched into chunks of about this size when frames are already waiting.
const CHUNK_TARGET: usize = 64 * 1024;

/// Output longer than this, as a projection sent whole in one long frame, is handed to the HTTP server in pieces of this
/// size, so that the connection reads its input between them, and the keepalive sweep sees each written, however slow the
/// link: a frame cannot be interleaved with others, but the connection is not stalled while it is sent. See
/// `design/server.md`.
const PIECE_BYTES: usize = 256 * 1024;

/// The most request reads applied before each outgoing chunk when both directions are busy.
const INPUT_READS_PER_OUTPUT: usize = 8;

/// The most request reads applied before the connection yields. Every connection, the keepalive sweep and the space drivers are polled by one task, and a request read completes without waiting while the peer's data is buffered, as does writing output while the socket has room. Without this, a peer that keeps sending faster than its input is applied, whether or not it reads what it is sent, would keep the rest of the server from running.
const INPUT_READS_PER_YIELD: usize = 64;

/// How many times in a row the connection returns pending when it yields. The HTTP server polls each read once and, if it is pending, flushes and then awaits the same read: when nothing is waiting to be written, that second poll comes at once, in the same poll of the task, and would complete a single yield without the task having yielded. The second pending reaches the server's set of connections, which returns to the task once two of its connections have yielded or it has polled each of them, so the keepalive sweep and the space drivers run after at most about twice `INPUT_READS_PER_YIELD + INPUT_READS_PER_OUTPUT` reads of one flooding connection. See `base_http1`'s `Output::write_body` and `FuturesUnordered`.
const YIELDS: usize = 2;

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
    /// Reads the client's frames within what its role sends, [`Output::inbound`], whatever the server sends it.
    decoder: FrameDecoder,
    /// How often the client sends; see `design/security.md`.
    pace: Pace,
    /// Output being handed out in pieces, and how much of it has been; see [`PIECE_BYTES`].
    pending: Vec<u8>,
    sent: usize,
    /// Request reads applied since the connection last yielded; see [`INPUT_READS_PER_YIELD`].
    reads: usize,
    /// Pending returns still to make before reading again; see [`YIELDS`]. Kept here so that a read dropped while yielding leaves the rest for the next.
    yields: usize,
    input_ended: bool,
    finished: bool,
}

impl<B: ReadableBody> ConnectionBody<B> {
    /// `cancellation` must end only this connection's HTTP exchange; the hub cancels it when the connection goes silent. See [`Hub::sweep`]. Returns `None` if the hub refuses the connection, as a visitor beyond the limit.
    pub fn new(
        hub: Arc<Hub>,
        request: B,
        cancellation: Cancellation,
        access: Access,
    ) -> Option<Self> {
        let (id, outbox, received, output) = hub.connect(cancellation, access)?;
        Some(Self {
            hub,
            id,
            outbox,
            received,
            output,
            request,
            decoder: FrameDecoder::new(),
            pace: Pace::new(),
            pending: Vec::new(),
            sent: 0,
            reads: 0,
            yields: 0,
            input_ended: false,
            finished: false,
        })
    }

    /// Append waiting outgoing frames to `output`, up to the chunk target. Returns false once the outbox is closed and drained.
    fn drain(&mut self, output: &mut Vec<u8>) -> bool {
        let limit = self.output.frame_limit();
        while output.len() < CHUNK_TARGET {
            match self.outbox.pop() {
                Popped::Control(item) => {
                    if let Some(message) = self.hub.render(self.id, item) {
                        encode(&message, output, limit);
                    }
                }
                Popped::Audio(frame) | Popped::Image(frame) => output.extend_from_slice(&frame),
                Popped::Empty => break,
                Popped::Closed => return false,
            }
        }
        true
    }

    fn input<D: AsRef<[u8]>>(&mut self, frame: BodyFrame<D>) -> Result<(), BodyError> {
        self.reads += 1;
        let end = frame.is_end();
        if let BodyFrame::Data { data, .. } = &frame {
            // Any data counts as liveness, even part of a frame.
            self.received.store(true, Ordering::Release);
            self.decoder.push(data.as_ref());
            loop {
                // As the client's Hello, or a knock admitting it, decides, from the frame after it on.
                let inbound = self.output.inbound();
                self.decoder.set_exact_limit(inbound.frame_bytes());
                let frame = match self.decoder.next_frame() {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    // Refused from its header, before any of it is held.
                    Err(FrameError::TooLarge(length)) => {
                        self.refuse(format!(
                            "This server takes frames of at most {} bytes from {}, and refused one of {length} bytes.",
                            inbound.frame_bytes(),
                            inbound.whom(),
                        ));
                        return Ok(());
                    }
                    Err(error) => return Err(BodyError::new(error)),
                };
                match self
                    .pace
                    .take(inbound, frame.kind, std::time::Instant::now())
                {
                    Verdict::Take => self.hub.receive(self.id, frame),
                    Verdict::Drop => {}
                    Verdict::Refuse => {
                        self.refuse(format!(
                            "This connection sent more often than this server takes from {}, for too long.",
                            inbound.whom(),
                        ));
                        return Ok(());
                    }
                }
            }
        }
        if end {
            self.decoder.finish().map_err(BodyError::new)?;
            self.input_ended = true;
        }
        Ok(())
    }

    /// Refuse what the client sends from now on, telling it `error`, and end the connection once what is queued for it
    /// is sent, as for any other error that ends one.
    fn refuse(&mut self, error: String) {
        log::warn!("Connection {} was closed: {error}", self.id);
        self.outbox.message(ServerMessage::Error {
            error,
            space: None,
            panel: None,
            wall: None,
        });
        self.outbox.close();
        self.input_ended = true;
        self.request.close(None);
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

/// Append `message` as a frame, written straight into `output`. A message that would exceed the connection's frame limit, `limit`, which the server's size limits should prevent, is replaced by a short error so that the client is not disconnected by a frame it must reject.
fn encode(message: &ServerMessage, output: &mut Vec<u8>, limit: usize) {
    if let Err(error) = Frame::encode_control_within(message, output, limit) {
        log::error!("A server message was not sent: {error}.");
        Frame::server(&ServerMessage::Error {
            error: "A message from the server was too large to send.".to_owned(),
            space: None,
            panel: None,
            wall: None,
        })
        .encode_into(output);
    }
}

impl<B: ReadableBody> ConnectionBody<B> {
    /// The next piece of the output being handed out in pieces.
    fn piece(&mut self) -> BodyFrame<Vec<u8>> {
        let end = (self.sent + PIECE_BYTES).min(self.pending.len());
        let data = self.pending[self.sent..end].to_vec();
        self.sent = end;
        if end == self.pending.len() {
            self.pending = Vec::new();
            self.sent = 0;
        }
        self.output.handed_out();
        BodyFrame::Data { data, end: false }
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
        self.finished || self.sent < self.pending.len() || self.outbox.is_ready()
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
            if self.reads >= INPUT_READS_PER_YIELD {
                self.reads = 0;
                self.yields = YIELDS;
            }
            while self.yields > 0 {
                self.yields -= 1;
                // Nothing has been read since the last input was applied, so dropping this read while it yields loses nothing.
                futures_lite::future::yield_now().await;
            }
            self.take_ready_input(cancellation)?;
            if self.sent < self.pending.len() {
                return Ok(self.piece());
            }
            // Listening before draining, so that a frame queued after the drain wakes the wait below.
            let listener = self.outbox.listen();
            let mut output = Vec::new();
            let open = self.drain(&mut output);
            if !output.is_empty() {
                // Draining stops once the output reaches the chunk target, before it could find the outbox closed,
                // so a response that ends does so after the last piece, as after any other output.
                if output.len() > PIECE_BYTES {
                    self.pending = output;
                    self.sent = 0;
                    return Ok(self.piece());
                }
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll, Waker};

    use tomori_protocol::ClientMessage;

    use super::*;
    use crate::transcriber::NullTranscriber;
    use crate::workspace::{DEFAULT_MAX_SPACES, SpaceSlots};
    use crate::{Access, Backends};

    /// More reads than a connection should ever apply without yielding. A flood stops being ready after this many, so that a connection that never yields fails the test rather than hanging it.
    const FLOOD_LIMIT: usize = 10_000;

    /// The most reads a connection applies before it yields: the limit is checked before each wait, and one wait reads at most `INPUT_READS_PER_OUTPUT + 1` times.
    const MOST_READS_BEFORE_YIELDING: usize = INPUT_READS_PER_YIELD + INPUT_READS_PER_OUTPUT;

    /// A request body whose every read completes at once with `message`, like a peer whose data is always buffered, for [`FLOOD_LIMIT`] reads.
    struct Flood {
        message: Vec<u8>,
        reads: Arc<AtomicUsize>,
    }

    impl ReadableBody for Flood {
        type Data = Vec<u8>;
        type Error = std::io::Error;

        fn close(&mut self, _error: Option<Self::Error>) {}

        async fn read(
            &mut self,
            _cancellation: &Cancellation,
        ) -> Result<BodyFrame<Vec<u8>>, Self::Error> {
            let reads = self.reads.fetch_add(1, Ordering::Relaxed);
            assert!(reads < FLOOD_LIMIT, "the connection never yielded");
            Ok(BodyFrame::Data {
                data: self.message.clone(),
                end: false,
            })
        }
    }

    /// A connection whose peer sends `message` without pause, and a count of the request reads.
    fn flooded(message: &ClientMessage) -> (ConnectionBody<Flood>, Arc<AtomicUsize>) {
        let (jobs, _) = async_channel::unbounded();
        let hub = Hub::new(
            Backends::new(),
            Arc::new(NullTranscriber::new()),
            jobs,
            Vec::new(),
            SpaceSlots::new(DEFAULT_MAX_SPACES),
        );
        let reads = Arc::new(AtomicUsize::new(0));
        let request = Flood {
            message: Frame::client(message).encode(),
            reads: reads.clone(),
        };
        let body = ConnectionBody::new(hub, request, Cancellation::new(), Access::open())
            .expect("an open server admits the connection");
        (body, reads)
    }

    /// Poll one read of `body` as `base_http1`'s `Output::write_body` does when nothing is waiting to be written: once, and if that is pending, at once again, in the same poll of its task. Returns pending only if the read would make the task yield.
    fn poll_read(body: &mut ConnectionBody<Flood>) -> Poll<Result<BodyFrame<Vec<u8>>, BodyError>> {
        let cancellation = Cancellation::new();
        let mut read = pin!(body.read(&cancellation));
        let mut context = Context::from_waker(Waker::noop());
        match read.as_mut().poll(&mut context) {
            Poll::Pending => read.as_mut().poll(&mut context),
            ready => ready,
        }
    }

    #[test]
    fn a_peer_whose_input_is_always_ready_and_needs_no_reply_lets_others_run() {
        // A pong queues no output, so without yielding the read would loop for as long as input is ready.
        let (mut body, reads) = flooded(&ClientMessage::Pong { nonce: 1 });
        assert!(poll_read(&mut body).is_pending());
        let applied = reads.load(Ordering::Relaxed);
        assert!(
            (INPUT_READS_PER_YIELD..=MOST_READS_BEFORE_YIELDING).contains(&applied),
            "{applied} reads before yielding"
        );
    }

    #[test]
    fn a_peer_whose_input_is_always_ready_and_answered_lets_others_run() {
        // Each ping queues a pong, so every read returns output, which the HTTP server may write without waiting; the connection still yields between reads.
        let (mut body, reads) = flooded(&ClientMessage::Ping { nonce: 1 });
        let mut outputs = 0;
        loop {
            let Poll::Ready(result) = poll_read(&mut body) else {
                break;
            };
            let Ok(BodyFrame::Data { .. }) = result else {
                unreachable!("each ping is answered with data, not {result:?}");
            };
            outputs += 1;
            assert!(outputs <= INPUT_READS_PER_YIELD, "never yielded");
        }
        assert!(outputs > 0);
        // Closing the response closes the request.
        body.close(None);
        assert!(body.finished);
        let applied = reads.load(Ordering::Relaxed);
        assert!(
            (INPUT_READS_PER_YIELD..=MOST_READS_BEFORE_YIELDING).contains(&applied),
            "{applied} reads before yielding"
        );
    }

    /// A request body that returns `frames` in turn, each after a wait if `waits`, then waits for ever.
    struct Script {
        frames: std::collections::VecDeque<Result<BodyFrame<Vec<u8>>, std::io::Error>>,
        waits: bool,
    }

    impl ReadableBody for Script {
        type Data = Vec<u8>;
        type Error = std::io::Error;

        fn close(&mut self, _error: Option<Self::Error>) {
            self.frames.clear();
        }

        async fn read(
            &mut self,
            _cancellation: &Cancellation,
        ) -> Result<BodyFrame<Vec<u8>>, Self::Error> {
            if self.waits {
                futures_lite::future::yield_now().await;
            }
            match self.frames.pop_front() {
                Some(frame) => frame,
                None => unreachable!("the test reads no further than its script"),
            }
        }
    }

    fn data(bytes: &[u8]) -> Result<BodyFrame<Vec<u8>>, std::io::Error> {
        Ok(BodyFrame::Data {
            data: bytes.to_vec(),
            end: false,
        })
    }

    fn scripted(
        frames: Vec<Result<BodyFrame<Vec<u8>>, std::io::Error>>,
        waits: bool,
    ) -> ConnectionBody<Script> {
        scripted_as(Access::open(), frames, waits)
    }

    /// A connection admitted as `access` whose client sends `frames` in turn, each after a wait if `waits`.
    fn scripted_as(
        access: Access,
        frames: Vec<Result<BodyFrame<Vec<u8>>, std::io::Error>>,
        waits: bool,
    ) -> ConnectionBody<Script> {
        let (jobs, _) = async_channel::unbounded();
        let hub = Hub::new(
            Backends::new(),
            Arc::new(NullTranscriber::new()),
            jobs,
            Vec::new(),
            SpaceSlots::new(DEFAULT_MAX_SPACES),
        );
        let request = Script {
            frames: frames.into(),
            waits,
        };
        ConnectionBody::new(hub, request, Cancellation::new(), access)
            .expect("the server admits the connection")
    }

    fn read<B: ReadableBody>(
        body: &mut ConnectionBody<B>,
    ) -> Result<BodyFrame<Vec<u8>>, BodyError> {
        futures_lite::future::block_on(body.read(&Cancellation::new()))
    }

    #[test]
    fn input_that_is_not_a_frame_or_ends_inside_one_fails_the_connection() {
        // A kind byte no frame has, read at once or after a wait.
        for waits in [false, true] {
            let mut body = scripted(vec![data(&[9, 0, 0, 0, 0])], waits);
            assert!(read(&mut body).is_err());
        }
        // The upload ends with half a frame.
        let mut body = scripted(
            vec![Ok(BodyFrame::Data {
                data: vec![0, 9],
                end: true,
            })],
            false,
        );
        assert!(read(&mut body).is_err());
        // The request cannot be read, at once or after a wait.
        for waits in [false, true] {
            let mut body = scripted(vec![Err(std::io::Error::other("reset"))], waits);
            assert!(read(&mut body).is_err());
        }
    }

    #[test]
    fn a_response_closed_ends_at_once_and_never_has_trailers() {
        let mut body = scripted(Vec::new(), false);
        assert!(!body.may_have_trailers());
        body.close(None);
        let Ok(BodyFrame::End) = read(&mut body) else {
            unreachable!("a closed response ends");
        };
    }

    #[test]
    fn a_space_rendered_for_a_connection_in_no_space_is_nothing() {
        let mut body = scripted(Vec::new(), false);
        body.outbox.space();
        let mut output = Vec::new();
        assert!(body.drain(&mut output));
        assert!(output.is_empty());
    }

    /// A Hello stating `limits`, encoded.
    fn hello(limits: Option<tomori_protocol::Limits>) -> Vec<u8> {
        Frame::client(&ClientMessage::Hello {
            protocol: tomori_protocol::PROTOCOL_VERSION,
            participant: "ada".into(),
            name: "Ada".into(),
            names: false,
            environments: None,
            environment_state: false,
            projection: true,
            pairing_window: false,
            speech_held: false,
            images: false,
            diagrams: false,
            world_edges: false,
            projection_patches: true,
            live_panels: false,
            spawns: false,
            limits,
        })
        .encode()
    }

    /// A control frame of `length` bytes, a pong padded with spaces, which is valid JSON.
    fn padded_pong(length: usize) -> Vec<u8> {
        let mut payload = br#"{"type":"pong","nonce":1}"#.to_vec();
        payload.resize(length, b' ');
        let mut bytes = Vec::new();
        Frame {
            kind: tomori_protocol::FrameKind::Control,
            payload,
        }
        .try_encode_within(&mut bytes, tomori_protocol::MOST_FRAME_BYTES)
        .unwrap();
        bytes
    }

    fn admitted(
        access: Access,
        frames: Vec<Result<BodyFrame<Vec<u8>>, std::io::Error>>,
    ) -> ConnectionBody<Script> {
        scripted_as(access, frames, false)
    }

    /// Read `body` until its response ends, returning the messages it was sent.
    fn sent_until_ended<B: ReadableBody>(body: &mut ConnectionBody<B>) -> Vec<ServerMessage> {
        let mut decoder = FrameDecoder::with_limit(tomori_protocol::MOST_FRAME_BYTES);
        loop {
            match read(body) {
                Ok(BodyFrame::Data { data, end }) => {
                    decoder.push(&data);
                    if end {
                        break;
                    }
                }
                Ok(BodyFrame::End) => break,
                _ => unreachable!("data until the response ends"),
            }
        }
        std::iter::from_fn(|| decoder.next_frame().unwrap())
            .map(|frame| frame.to_server_message().unwrap())
            .collect()
    }

    /// The errors `body` was sent until its response ended.
    fn errors_until_ended<B: ReadableBody>(body: &mut ConnectionBody<B>) -> Vec<String> {
        sent_until_ended(body)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::Error { error, .. } => Some(error),
                _ => None,
            })
            .collect()
    }

    fn ended(bytes: &[u8]) -> Result<BodyFrame<Vec<u8>>, std::io::Error> {
        Ok(BodyFrame::Data {
            data: bytes.to_vec(),
            end: true,
        })
    }

    /// The header of a control frame declaring `length` bytes, alone, as a client sends before any of its payload.
    fn header(length: usize) -> Vec<u8> {
        let mut bytes = vec![tomori_protocol::FrameKind::Control as u8];
        bytes.extend_from_slice(&(length as u32).to_le_bytes());
        bytes
    }

    fn refused(errors: &[String], limit: usize, whom: &str) -> bool {
        errors.iter().any(|error| {
            error.starts_with(&format!(
                "This server takes frames of at most {limit} bytes from {whom}"
            ))
        })
    }

    #[test]
    fn a_frame_longer_than_a_visitor_sends_is_refused_from_its_header() {
        let watching = crate::inbound::WATCHING_FRAME_BYTES;
        let whom = crate::inbound::Inbound::Watching.whom();
        // Declaring the most a frame may be, with none of it sent: refused from the header, before any of it is held,
        // the client told why and the response ended, the rest of the upload unread.
        for access in [Access::visitor(), Access::pairing()] {
            let mut body = admitted(
                access,
                vec![
                    data(&hello(Some(tomori_protocol::Limits::MOST))),
                    data(&header(tomori_protocol::MOST_FRAME_BYTES)),
                ],
            );
            let errors = errors_until_ended(&mut body);
            assert!(refused(&errors, watching, whom), "{errors:?}");
            assert!(body.input_ended && body.finished && body.request.frames.is_empty());
            // What it is sent is as long as it reads, however little it sends.
            assert_eq!(body.output.frame_limit(), tomori_protocol::MOST_FRAME_BYTES);
            assert_eq!(body.decoder.progress(), None);
        }
        // As long as a visitor sends is taken.
        let mut body = admitted(
            Access::visitor(),
            vec![
                data(&hello(Some(tomori_protocol::Limits::MOST))),
                ended(&padded_pong(watching)),
            ],
        );
        assert!(errors_until_ended(&mut body).is_empty());
        assert_eq!(body.output.inbound(), crate::inbound::Inbound::Watching);
    }

    #[test]
    fn before_its_hello_any_connection_sends_only_as_long_as_a_visitor() {
        let watching = crate::inbound::WATCHING_FRAME_BYTES;
        let whom = crate::inbound::Inbound::Watching.whom();
        let mut body = admitted(Access::open(), vec![ended(&padded_pong(watching + 1))]);
        let errors = errors_until_ended(&mut body);
        assert!(refused(&errors, watching, whom), "{errors:?}");
        // As is the Hello itself.
        let long_hello = Frame::client(&ClientMessage::Hello {
            protocol: tomori_protocol::PROTOCOL_VERSION,
            participant: "a".repeat(watching),
            name: "Ada".into(),
            names: false,
            environments: None,
            environment_state: false,
            projection: false,
            pairing_window: false,
            speech_held: false,
            images: false,
            diagrams: false,
            world_edges: false,
            projection_patches: false,
            live_panels: false,
            spawns: false,
            limits: None,
        })
        .encode();
        let mut body = admitted(Access::open(), vec![ended(&long_hello)]);
        let errors = errors_until_ended(&mut body);
        assert!(refused(&errors, watching, whom), "{errors:?}");
    }

    #[test]
    fn an_owner_s_longest_frames_are_taken_and_longer_refused() {
        let participating = crate::inbound::PARTICIPATING_FRAME_BYTES;
        // The longest audio, 20 ms at 48 kHz, and the longest frame a participant may send.
        let audio = Frame::audio(&tomori_protocol::AudioFrame {
            gate: tomori_protocol::Gate::Live,
            source: 0,
            sequence: 0,
            timestamp: 0,
            sample_rate: 48_000,
            samples: vec![0; 960],
        })
        .encode();
        let mut body = admitted(
            Access::open(),
            vec![
                data(&hello(Some(tomori_protocol::Limits::MOST))),
                data(&audio),
                ended(&padded_pong(participating)),
            ],
        );
        assert!(errors_until_ended(&mut body).is_empty());
        assert_eq!(
            body.output.inbound(),
            crate::inbound::Inbound::Participating
        );
        // One byte longer is refused, whatever the client reads, as is today's longest, which an older client could
        // send.
        for longer in [participating + 1, tomori_protocol::MAX_FRAME_BYTES] {
            let mut body = admitted(
                Access::open(),
                vec![
                    data(&hello(Some(tomori_protocol::Limits::MOST))),
                    data(&header(longer)),
                ],
            );
            let errors = errors_until_ended(&mut body);
            assert!(
                refused(&errors, participating, "an owner or a collaborator"),
                "{errors:?}"
            );
            assert_eq!(body.output.frame_limit(), tomori_protocol::MOST_FRAME_BYTES);
        }
        // From a client stating nothing, as an older one, the same is taken, and it is sent today's.
        let mut body = admitted(
            Access::open(),
            vec![data(&hello(None)), ended(&padded_pong(participating))],
        );
        assert!(errors_until_ended(&mut body).is_empty());
        assert_eq!(body.output.frame_limit(), tomori_protocol::MAX_FRAME_BYTES);
    }

    #[test]
    fn a_visitor_sending_too_often_for_too_long_is_closed() {
        // Its burst, as many again dropped, and then refused, before the rest of its upload is read.
        let ping = Frame::client(&ClientMessage::Ping { nonce: 1 }).encode();
        let mut frames = vec![data(&hello(None))];
        frames.extend((0..1000).map(|_| data(&ping)));
        let mut body = admitted(Access::visitor(), frames);
        let sent = sent_until_ended(&mut body);
        assert!(
            sent.iter().any(|message| matches!(message, ServerMessage::Error { error, .. } if error
                == "This connection sent more often than this server takes from a visitor, a device pairing or a client before its Hello, for too long.")),
            "{sent:?}"
        );
        assert!(body.input_ended && body.request.frames.is_empty());
        // The pings within its burst answered, with the Hello, and those dropped not.
        let pongs = sent
            .iter()
            .filter(|message| matches!(message, ServerMessage::Pong { .. }))
            .count();
        assert!((255..=260).contains(&pongs), "{pongs}");
    }

    #[test]
    fn long_output_is_handed_out_in_pieces_and_a_closed_response_ends_after_them() {
        let mut body = listening();
        let pieces = |body: &mut ConnectionBody<Listening>| {
            let mut bytes = Vec::new();
            let mut sizes = Vec::new();
            loop {
                match read(body) {
                    Ok(BodyFrame::Data { data, end }) => {
                        sizes.push(data.len());
                        bytes.extend(data);
                        if end || !body.is_ready() {
                            return (bytes, sizes, end);
                        }
                    }
                    Ok(BodyFrame::End) => return (bytes, sizes, true),
                    _ => unreachable!("data until the response ends"),
                }
            }
        };
        // The Welcome and what follows Hello.
        pieces(&mut body);
        let message = ServerMessage::Error {
            error: "x".repeat(tomori_protocol::MAX_FRAME_BYTES),
            space: None,
            panel: None,
            wall: None,
        };
        body.outbox.message(message.clone());
        body.outbox.close();
        let (bytes, sizes, end) = pieces(&mut body);
        assert!(end && body.finished);
        // The pieces, then the end on its own.
        assert_eq!(sizes.iter().sum::<usize>(), bytes.len());
        assert!(
            sizes.len() > 4 && sizes.iter().all(|size| *size <= PIECE_BYTES),
            "{sizes:?}"
        );
        let mut decoder =
            tomori_protocol::FrameDecoder::with_limit(tomori_protocol::MOST_FRAME_BYTES);
        decoder.push(&bytes);
        assert_eq!(
            decoder
                .next_frame()
                .unwrap()
                .unwrap()
                .to_server_message()
                .unwrap(),
            message
        );
        decoder.finish().unwrap();
        let Ok(BodyFrame::End) = read(&mut body) else {
            unreachable!("ended");
        };
        // Closing the response closes the request.
        body.close(None);
    }

    /// A request body that says Hello, stating the most, and then nothing, as a client that is listening.
    struct Listening {
        hello: Option<Vec<u8>>,
    }

    impl ReadableBody for Listening {
        type Data = Vec<u8>;
        type Error = std::io::Error;

        fn close(&mut self, _error: Option<Self::Error>) {}

        async fn read(
            &mut self,
            _cancellation: &Cancellation,
        ) -> Result<BodyFrame<Vec<u8>>, Self::Error> {
            match self.hello.take() {
                Some(data) => Ok(BodyFrame::Data { data, end: false }),
                None => std::future::pending().await,
            }
        }
    }

    /// A connection whose client has said Hello, stating the most, and listens.
    fn listening() -> ConnectionBody<Listening> {
        let (jobs, _) = async_channel::unbounded();
        let hub = Hub::new(
            Backends::new(),
            Arc::new(NullTranscriber::new()),
            jobs,
            Vec::new(),
            SpaceSlots::new(DEFAULT_MAX_SPACES),
        );
        let request = Listening {
            hello: Some(hello(Some(tomori_protocol::Limits::MOST))),
        };
        ConnectionBody::new(hub, request, Cancellation::new(), Access::open())
            .expect("an open server admits the connection")
    }

    /// Measures how long agent speech queued just after a projection whole of 30 MB, the longest a frame may be near
    /// enough, waits behind it on a link over loopback, as `design/server.md` reports:
    /// `cargo test -p tomori-server --lib speech_behind -- --nocapture`.
    #[test]
    fn speech_behind_a_projection_whole_of_30_mb_waits_only_as_long_as_it_takes_to_send() {
        use std::io::{Read, Write};
        use std::time::Instant;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // The client reads at a frame limit of 32 MiB, noting when the long frame, then the speech, has arrived.
        let client = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut decoder =
                tomori_protocol::FrameDecoder::with_limit(tomori_protocol::MOST_FRAME_BYTES);
            let mut buffer = vec![0; 1 << 20];
            let mut long = None;
            loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "the speech never arrived");
                decoder.push(&buffer[..read]);
                while let Some(frame) = decoder.next_frame().unwrap() {
                    match frame.kind {
                        tomori_protocol::FrameKind::Audio => {
                            return (long.expect("the long frame first"), Instant::now());
                        }
                        _ if frame.payload.len() > tomori_protocol::MAX_FRAME_BYTES => {
                            long = Some(Instant::now());
                        }
                        _ => {}
                    }
                }
            }
        });
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let mut body = listening();
        let cancellation = Cancellation::new();
        let write = |body: &mut ConnectionBody<Listening>, stream: &mut std::net::TcpStream| {
            let mut largest = 0;
            loop {
                let Ok(BodyFrame::Data { data, .. }) =
                    futures_lite::future::block_on(body.read(&cancellation))
                else {
                    unreachable!("data while there is any");
                };
                largest = data.len().max(largest);
                stream.write_all(&data).unwrap();
                if !body.is_ready() {
                    return largest;
                }
            }
        };
        // The Welcome, and what follows Hello.
        write(&mut body, &mut stream);
        let message = ServerMessage::Error {
            error: "x".repeat(30_000_000),
            space: None,
            panel: None,
            wall: None,
        };
        let speech = Frame::audio(&tomori_protocol::AudioFrame {
            gate: tomori_protocol::Gate::Agent,
            source: tomori_protocol::AGENT_SOURCE,
            sequence: 0,
            timestamp: 0,
            sample_rate: 24_000,
            samples: vec![0; 480],
        })
        .encode();
        let queued = Instant::now();
        body.outbox.message(message);
        body.outbox.agent_audio(speech);
        let largest = write(&mut body, &mut stream);
        let (long, heard) = client.join().unwrap();
        let waited = heard - queued;
        eprintln!(
            "Speech queued just after a 30 MB frame arrived {:.0} ms later, {:.0} ms after the frame, in pieces of at most {} bytes.",
            waited.as_secs_f64() * 1000.0,
            (heard - long).as_secs_f64() * 1000.0,
            largest,
        );
        assert!(largest <= PIECE_BYTES);
        // Generous, for a loaded machine running every test at once; about 40 ms here.
        assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");
    }

    #[test]
    fn a_message_too_large_for_the_connection_is_replaced_by_an_error_saying_so() {
        crate::test_log::record();
        let message = ServerMessage::Error {
            error: "x".repeat(tomori_protocol::MAX_FRAME_BYTES),
            space: None,
            panel: None,
            wall: None,
        };
        // Too large for today's limit, which an older client is sent within.
        let mut output = Vec::new();
        encode(&message, &mut output, tomori_protocol::MAX_FRAME_BYTES);
        let mut decoder = tomori_protocol::FrameDecoder::new();
        decoder.push(&output);
        let frame = decoder.next_frame().unwrap().unwrap();
        let Ok(ServerMessage::Error { error, .. }) = frame.to_server_message() else {
            unreachable!("the replacement is an error");
        };
        assert_eq!(error, "A message from the server was too large to send.");
        // Sent whole on a connection whose sides both state more.
        let mut output = Vec::new();
        encode(&message, &mut output, tomori_protocol::MOST_FRAME_BYTES);
        let mut decoder =
            tomori_protocol::FrameDecoder::with_limit(tomori_protocol::MOST_FRAME_BYTES);
        decoder.push(&output);
        let frame = decoder.next_frame().unwrap().unwrap();
        assert_eq!(frame.to_server_message().unwrap(), message);
        assert!(!crate::test_log::logged("A server message was not sent").is_empty());
    }
}
