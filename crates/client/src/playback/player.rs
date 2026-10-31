// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use protocol::AudioFrame;

use super::{Mixer, MixerOptions, PlaybackStatistics};

/// How long playback takes to fade out when silenced, and back in afterwards, so that it does not click.
const FADE: Duration = Duration::from_millis(10);

enum Command {
    LocalSource(Option<u32>),
    Clear,
}

#[derive(Default)]
struct Shared {
    silenced: AtomicBool,
    statistics: Mutex<PlaybackStatistics>,
}

/// Plays what a [`crate::Client`] receives: drains its audio into a [`Mixer`] and renders the mix for an output device.
///
/// The player belongs to the audio device's callback, and [`PlayerControl`] to the application. Control changes travel through a channel that [`Self::render`] drains without blocking, so the application never holds anything the callback waits for. Rendering never blocks: missing audio is silence.
///
/// The mixer is created at the device rate the first time [`Self::render`] is called, and created again if the device rate changes, keeping the local source.
///
/// Unlike a [`Mixer`] shared with the application, the player also calls [`Mixer::push`] from the callback, so the callback allocates and frees: a buffer for each new source, and each frame's samples once played. Received frames are 20 ms each, so this is a few small allocations per callback; nothing waits on a lock.
pub struct Player {
    audio: Receiver<AudioFrame>,
    commands: Receiver<Command>,
    shared: Arc<Shared>,
    options: MixerOptions,
    mixer: Option<Mixer>,
    local_source: Option<u32>,
    /// How far playback has faded out, in samples: 0 is full volume, and the fade length is silent.
    faded: u32,
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
            faded: 0,
        };
        (player, PlayerControl { commands, shared })
    }

    /// Fill `output` with the next mono samples at `sample_rate`, after taking every frame received since the last call.
    pub fn render(&mut self, sample_rate: u32, output: &mut [i16]) {
        if sample_rate == 0 {
            output.fill(0);
            return;
        }
        if self.mixer.as_ref().map(Mixer::sample_rate) != Some(sample_rate) {
            let mut mixer = Mixer::new(MixerOptions {
                sample_rate,
                ..self.options
            });
            mixer.set_local_source(self.local_source);
            self.mixer = Some(mixer);
        }
        let mixer = self.mixer.as_mut().expect("created above");
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::LocalSource(source) => {
                    self.local_source = source;
                    mixer.set_local_source(source);
                }
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
        // Silenced audio is still mixed, so that it is consumed in real time and resumes live rather than replaying what was missed.
        mixer.mix(output);
        let length = (FADE.as_secs_f64() * f64::from(sample_rate))
            .ceil()
            .max(1.0) as u32;
        let silenced = self.shared.silenced.load(Ordering::Relaxed);
        self.faded = self.faded.min(length);
        if silenced || self.faded > 0 {
            for sample in output.iter_mut() {
                self.faded = if silenced {
                    (self.faded + 1).min(length)
                } else {
                    self.faded.saturating_sub(1)
                };
                let gain = 1.0 - self.faded as f32 / length as f32;
                *sample = (f32::from(*sample) * gain).round() as i16;
            }
        }
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

    /// Silence playback, fading out over a few milliseconds, while still consuming what arrives in real time. Used while the microphone records push-to-talk, so that playback cannot be picked up and transcribed; see `design/live-audio.md`.
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
