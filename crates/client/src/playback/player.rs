// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tomori_protocol::{AudioFrame, REPLAY_SOURCE};

use super::spatial::{Position, Scene};
use super::{Direction, Earcon, Layout, Mixer, MixerOptions, PlaybackStatistics, Sound};

/// How long after its last audio agent speech played again still counts as playing: longer than the breath between spoken segments, so that a replay counts as playing throughout.
const REPLAY_PAUSE: Duration = Duration::from_millis(700);

enum Command {
    LocalSource(Option<u32>),
    Clear,
    AgentMuted(bool),
    Play(Earcon, Direction),
    Ambient(Sound, Position),
    Probe,
}

/// The scene the application last set, until the callback takes it.
#[derive(Default)]
struct SceneSlot {
    scene: Scene,
    fresh: bool,
}

#[derive(Default)]
struct Shared {
    silenced: AtomicBool,
    /// Whether agent speech played again for this participant was playing at the last render.
    replaying: AtomicBool,
    statistics: Mutex<PlaybackStatistics>,
    scene: Mutex<SceneSlot>,
}

/// Plays what a [`crate::Client`] receives: drains its audio into a [`Mixer`] and renders the mix for an output device.
///
/// The player belongs to the audio device's callback, and [`PlayerControl`] to the application. Control changes travel through a channel that [`Self::render`] drains without blocking, so the application never holds anything the callback waits for. Rendering never blocks: missing audio is silence.
///
/// The mixer is created at the device rate by [`Self::prepare`], which an output calls before it starts, since creating one synthesizes the sound cues; [`Self::render`] creates it again if the device rate changes, keeping the local source.
///
/// Unlike a [`Mixer`] shared with the application, the player also calls [`Mixer::push`] from the callback, so the callback allocates and frees: a buffer for each new source, and each frame's samples once played. Received frames are 20 ms each, so this is a few small allocations per callback; nothing waits on a lock.
pub struct Player {
    audio: Receiver<AudioFrame>,
    commands: Receiver<Command>,
    shared: Arc<Shared>,
    options: MixerOptions,
    mixer: Option<Mixer>,
    local_source: Option<u32>,
    agent_muted: bool,
    /// Whether mixers load the echo probe.
    probe: bool,
}

/// The application's handle on a [`Player`]. Cloneable, and never blocks the player.
#[derive(Clone)]
pub struct PlayerControl {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

impl Player {
    /// Play `audio`, typically from [`crate::Client::take_audio`]. `options.sample_rate` is replaced by the rate given to [`Self::render`].
    pub fn new(audio: Receiver<AudioFrame>, options: MixerOptions) -> (Self, PlayerControl) {
        let (commands, receiver) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let player = Self {
            audio,
            commands: receiver,
            shared: shared.clone(),
            options,
            mixer: None,
            local_source: None,
            agent_muted: false,
            probe: false,
        };
        (player, PlayerControl { commands, shared })
    }

    /// Synthesize the echo probe now and for later mixers, outside the audio callback, so that [`PlayerControl::play_probe`] can play it. Only the echo test needs it.
    pub fn load_probe(&mut self) {
        self.probe = true;
        if let Some(mixer) = &mut self.mixer {
            mixer.load_probe();
        }
    }

    /// Make the mixer for `sample_rate` now, outside the audio callback, since making one synthesizes every sound cue. An output calls this with the device's rate before it starts; [`Self::render`] makes one only if the rate changes after that.
    pub fn prepare(&mut self, sample_rate: u32) {
        if sample_rate == 0 || self.mixer.as_ref().map(Mixer::sample_rate) == Some(sample_rate) {
            return;
        }
        let mut mixer = Mixer::new(MixerOptions {
            sample_rate,
            ..self.options
        });
        mixer.set_local_source(self.local_source);
        mixer.set_agent_muted(self.agent_muted);
        if self.probe {
            mixer.load_probe();
        }
        // A mixer made while push-to-talk records starts silent, rather than fading out what the microphone would hear.
        if self.shared.silenced.load(Ordering::Relaxed) {
            mixer.silence_now();
        }
        self.mixer = Some(mixer);
    }

    /// Fill `output` with the next samples at `sample_rate`, mono or interleaved stereo as `layout` says, after taking every frame received since the last call and the latest scene.
    pub fn render(&mut self, sample_rate: u32, layout: Layout, output: &mut [i16]) {
        if sample_rate == 0 {
            output.fill(0);
            return;
        }
        self.prepare(sample_rate);
        let mixer = self.mixer.as_mut().expect("prepared above");
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::LocalSource(source) => {
                    self.local_source = source;
                    mixer.set_local_source(source);
                }
                Command::AgentMuted(muted) => {
                    self.agent_muted = muted;
                    mixer.set_agent_muted(muted);
                }
                Command::Play(earcon, direction) => mixer.play(earcon, direction),
                Command::Ambient(sound, position) => mixer.ambient(sound, position),
                Command::Probe => mixer.play_probe(),
                Command::Clear => {
                    mixer.clear();
                    // Including what arrived before the clear but was not yet rendered, such as a backlog from while no output played.
                    while self.audio.try_recv().is_ok() {}
                }
            }
        }
        while let Ok(frame) = self.audio.try_recv() {
            mixer.push(frame);
        }
        // The latest scene, swapped rather than copied, so that the callback never allocates for it; one the application is writing is taken next time.
        let slot = match self.shared.scene.try_lock() {
            Ok(slot) => Some(slot),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(mut slot) = slot
            && slot.fresh
        {
            std::mem::swap(mixer.scene_mut(), &mut slot.scene);
            slot.fresh = false;
        }
        // Silenced audio is still mixed, so that it is consumed in real time and resumes live rather than replaying what was missed.
        mixer.set_silenced(self.shared.silenced.load(Ordering::Relaxed));
        match layout {
            Layout::Mono => mixer.mix(output),
            Layout::Stereo => mixer.mix_stereo(output),
        }
        self.shared.replaying.store(
            mixer.is_source_recent(REPLAY_SOURCE, REPLAY_PAUSE),
            Ordering::Relaxed,
        );
        // Only the application reads the statistics, rarely, so the callback skips publishing them rather than wait.
        if let Ok(mut statistics) = self.shared.statistics.try_lock() {
            *statistics = mixer.statistics();
        }
    }
}

impl PlayerControl {
    /// Never play audio attributed to this participant number: a client does not hear its own microphone. Set from [`crate::ConnectionState::Connected`].
    pub fn set_local_source(&self, source: Option<u32>) {
        // Fails only once the player is gone, when nothing plays.
        let _ = self.commands.send(Command::LocalSource(source));
    }

    /// Discard buffered audio when what the participant hears changes context: on leaving a space, after reconnecting, and when an output starts after nothing played. Every frame received before the next render is discarded too, including any that arrived just after this call.
    pub fn clear(&self) {
        let _ = self.commands.send(Command::Clear);
    }

    /// Mute or unmute agent speech locally, live or played again, while other participants still play. Muted speech is still consumed in real time, so unmuting resumes it live. See `design/speech.md`.
    pub fn set_agent_muted(&self, muted: bool) {
        let _ = self.commands.send(Command::AgentMuted(muted));
    }

    /// Whether agent speech played again for this participant, a replay or catch-up, was playing at the last render, including the breaths between its segments.
    pub fn is_replaying(&self) -> bool {
        self.shared.replaying.load(Ordering::Relaxed)
    }

    /// Play a sound cue, once, from `direction`. It is local: never sent and never a source, and [`Self::clear`] does not stop it.
    pub fn play(&self, earcon: Earcon, direction: Direction) {
        let _ = self.commands.send(Command::Play(earcon, direction));
    }

    /// Play an environment's `sound`, once, from `position` in the scene, unless someone is speaking. See [`Mixer::ambient`].
    pub fn ambient(&self, sound: Sound, position: Position) {
        let _ = self.commands.send(Command::Ambient(sound, position));
    }

    /// Play the echo probe, once [`Player::load_probe`] has rendered it, for the echo test to measure what the microphone hears of it.
    pub fn play_probe(&self) {
        let _ = self.commands.send(Command::Probe);
    }

    /// Set where the listener and the sources are, typically once a frame. `change` must write the whole scene: it is given an older one to reuse its storage, which may be one the callback has finished with. The callback takes only the latest, at its next render, and never waits for this.
    pub fn set_scene(&self, change: impl FnOnce(&mut Scene)) {
        let mut slot = self
            .shared
            .scene
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        change(&mut slot.scene);
        slot.fresh = true;
    }

    /// Silence playback, fading out over a few milliseconds, while still consuming what arrives in real time; the cues that [`Earcon::bypasses_silence`] still play. Used while the microphone records push-to-talk, so that playback cannot be picked up and transcribed; see `design/live-audio.md`.
    pub fn set_silenced(&self, silenced: bool) {
        self.shared.silenced.store(silenced, Ordering::Relaxed);
    }

    pub fn is_silenced(&self) -> bool {
        self.shared.silenced.load(Ordering::Relaxed)
    }

    /// How playback has handled imperfect input, as of a recent render.
    pub fn statistics(&self) -> PlaybackStatistics {
        *self
            .shared
            .statistics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
