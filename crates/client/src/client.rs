// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use base_scheduler::Cancellation;
use base_scheduler_direct::DirectScheduler;
use protocol::{AudioFrame, ClientMessage};

use crate::outbox::{Membership, Outbox};
use crate::{ConnectionState, Event, Options, SendError, session};

/// State shared by the application handles and the connection thread.
pub(crate) struct Shared {
    pub outbox: Outbox,
    pub state: Mutex<ConnectionState>,
    /// Cancelled when the client closes, to interrupt reconnection delays and bound the final connection.
    pub closing: Cancellation,
}

impl Shared {
    pub fn set_state(&self, state: ConnectionState, events: &mpsc::Sender<Event>) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = state.clone();
        let _ = events.send(Event::State(state));
    }
}

/// A cloneable, thread-safe handle for sending to the server. Sending never blocks.
#[derive(Clone)]
pub struct Sender {
    shared: Arc<Shared>,
}

impl Sender {
    /// Queue a control message. Control messages are never dropped while the client is open: those sent while disconnected are delivered after the next connection's Hello and restored membership. `Join`, `Leave` and `Gate` also update the membership restored on reconnection; while disconnected they, and `Presence`, only update local state.
    pub fn send(&self, message: ClientMessage) -> Result<(), SendError> {
        self.shared.outbox.send(message)
    }

    /// Queue a microphone frame. Live audio is dropped while disconnected and, when the link is congested, the oldest queued frames are dropped first. Frames with [`protocol::Gate::Off`] are ignored.
    pub fn send_audio(&self, frame: AudioFrame) -> Result<(), SendError> {
        self.shared.outbox.send_audio(frame)
    }

    pub fn state(&self) -> ConnectionState {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The space and gate restored after reconnecting.
    pub fn membership(&self) -> Membership {
        self.shared.outbox.membership()
    }

    /// Outgoing audio frames dropped so far because of disconnection or congestion.
    pub fn dropped_audio_frames(&self) -> u64 {
        self.shared.outbox.dropped_audio()
    }

    /// Begin closing without waiting: stop accepting messages, deliver queued control messages and finish the connection. [`Client::close`] also waits for the connection thread.
    pub fn close(&self) {
        self.shared.outbox.close();
        self.shared.closing.cancel();
    }
}

/// A connection to the Tomori server, running on its own thread.
///
/// The client keeps one duplex `POST /connect` exchange open, sends Hello on every connection, restores the joined space and microphone gate after reconnecting, and reconnects with backoff. Nothing here blocks: poll [`Self::try_event`] from the application loop and drain audio with [`Self::try_audio`] or move the receiver from [`Self::take_audio`] to the audio thread.
pub struct Client {
    sender: Sender,
    events: Receiver<Event>,
    audio: Option<Receiver<AudioFrame>>,
    thread: Option<JoinHandle<()>>,
}

impl Client {
    /// Start connecting in the background. Fails only if the connection thread or its scheduler cannot be created; connection failures are reported as [`ConnectionState::Disconnected`] and retried.
    pub fn connect(options: Options) -> io::Result<Self> {
        let scheduler = DirectScheduler::new(Default::default())?;
        let shared = Arc::new(Shared {
            outbox: Outbox::new(options.outgoing_audio_limit),
            state: Mutex::new(ConnectionState::Connecting { attempt: 1 }),
            closing: Cancellation::new(),
        });
        let (events_sender, events) = mpsc::channel();
        let (audio_sender, audio): (SyncSender<AudioFrame>, _) =
            mpsc::sync_channel(options.incoming_audio_limit.max(1));
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("tomori-client".into())
            .spawn(move || {
                let context = session::Context {
                    scheduler: &scheduler,
                    shared: &thread_shared,
                    options: &options,
                    events: &events_sender,
                    audio: &audio_sender,
                };
                futures_lite::future::block_on(session::run(&context));
                futures_lite::future::block_on(scheduler.shutdown());
            })?;
        Ok(Self {
            sender: Sender { shared },
            events,
            audio: Some(audio),
            thread: Some(thread),
        })
    }

    /// A handle for sending from other threads.
    pub fn sender(&self) -> Sender {
        self.sender.clone()
    }

    /// See [`Sender::send`].
    pub fn send(&self, message: ClientMessage) -> Result<(), SendError> {
        self.sender.send(message)
    }

    /// See [`Sender::send_audio`].
    pub fn send_audio(&self, frame: AudioFrame) -> Result<(), SendError> {
        self.sender.send_audio(frame)
    }

    /// The current connection state. Every change is also delivered as [`Event::State`].
    pub fn state(&self) -> ConnectionState {
        self.sender.state()
    }

    /// Connection state changes and server messages, in order.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// The next event, if one is waiting.
    pub fn try_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Move received audio to another thread, typically the one feeding a [`crate::playback::Mixer`]. Returns `None` if already taken.
    pub fn take_audio(&mut self) -> Option<Receiver<AudioFrame>> {
        self.audio.take()
    }

    /// The next received audio frame, unless the receiver was taken.
    pub fn try_audio(&self) -> Option<AudioFrame> {
        self.audio.as_ref()?.try_recv().ok()
    }

    /// Close gracefully and wait for the connection thread, bounded by [`Options::close_timeout`].
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.sender.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Client {
    /// Closes as [`Self::close`] does, waiting briefly for the connection thread.
    fn drop(&mut self) {
        self.shutdown();
    }
}
