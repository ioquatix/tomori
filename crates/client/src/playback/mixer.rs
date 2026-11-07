// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::HashMap;
use std::time::Duration;

use tomori_protocol::{AGENT_SOURCE, AudioFrame, Gate, REPLAY_SOURCE};

use super::spatial::{self, Ears, Placement, Scene};
use super::{Bank, Duck, Earcon, JitterBuffer, JitterOptions, Limiter, PlaybackStatistics};

/// How long a source may go without frames before its buffer is released.
const IDLE_LIMIT: Duration = Duration::from_secs(10);
/// How quickly a source's level falls once it plays more quietly: it drops to about a third over this time.
const LEVEL_RELEASE: Duration = Duration::from_millis(150);
/// How long playback takes to fade out when silenced, and back in afterwards, so that it does not click.
const FADE: Duration = Duration::from_millis(10);
/// The most sound cues that play at once; another replaces the one furthest through.
const VOICES: usize = 8;
/// The most ambient sounds that play at once; another replaces the one furthest through.
const AMBIENT: usize = 3;

/// A sound an environment plays at a place in the scene, such as an animal calling: its samples, in `i16`
/// units, at `rate` samples a second, which the mixer plays at its own rate. See `design/spatial-audio.md`.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub samples: std::sync::Arc<[f32]>,
    pub rate: u32,
}

impl Sound {
    pub fn new(samples: impl Into<std::sync::Arc<[f32]>>, rate: u32) -> Self {
        Self {
            samples: samples.into(),
            rate,
        }
    }
}

/// An ambient sound playing: what, from where, and how far through it is, in its own samples.
#[derive(Clone, Debug)]
struct Ambient {
    sound: Sound,
    position: spatial::Position,
    played: f64,
}
/// How far to the side a cue for one hand sounds.
const HAND_PAN: f32 = 0.4;

/// Where a sound cue sounds from, relative to the head.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Centre,
    /// Towards the hand that acted.
    Left,
    Right,
    /// From the agent, for what the agent needs.
    Agent,
}

/// The output's channels: mono, or interleaved left and right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Mono,
    Stereo,
}

impl Layout {
    pub fn channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct MixerOptions {
    /// The output device rate.
    pub sample_rate: u32,
    pub jitter: JitterOptions,
}

impl Default for MixerOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            jitter: JitterOptions::default(),
        }
    }
}

/// One buffered source and the level the mixer last measured for it, kept together so that [`Mixer::mix`] never inserts into a map on the audio thread.
struct Source {
    buffer: JitterBuffer,
    /// See [`Mixer::source_level`].
    level: f32,
    /// Whether the last mix of this source was at least [`super::duck::THRESHOLD`], which ducking follows.
    loud: bool,
    ears: Ears,
}

impl Source {
    fn new(sample_rate: u32, output_rate: u32, options: JitterOptions) -> Self {
        Self {
            buffer: JitterBuffer::new(sample_rate, options),
            level: 0.0,
            loud: false,
            ears: Ears::new(output_rate),
        }
    }
}

fn is_agent(source: u32) -> bool {
    source == AGENT_SOURCE || source == REPLAY_SOURCE
}

/// How `source` sounds now at `sample_rate`: where `scene` places it, or centred. Agent speech played again comes from where the agent is.
fn placement(scene: &Scene, sample_rate: u32, source: u32, layout: Layout) -> Placement {
    let floor = if is_agent(source) {
        spatial::AGENT_FLOOR
    } else {
        spatial::PERSON_FLOOR
    };
    let position = scene.position(source).or_else(|| {
        is_agent(source)
            .then(|| scene.position(AGENT_SOURCE))
            .flatten()
    });
    match (scene.listener, position) {
        (Some(listener), Some(position)) => spatial::place(
            &listener,
            position,
            floor,
            sample_rate,
            scene.mono || layout == Layout::Mono,
        ),
        _ => Placement::CENTRE,
    }
}

/// A sound cue playing, and how far through it is.
#[derive(Clone, Copy, Debug)]
struct Voice {
    earcon: Earcon,
    direction: Direction,
    position: usize,
}

/// Mixes every remote participant and the agent speech stream into one `i16` stream at the device rate, mono or stereo, with this client's own sound cues.
///
/// Feed it every received [`AudioFrame`] with [`Self::push`] and pull output from the audio callback with [`Self::mix`] or [`Self::mix_stereo`]. In stereo, each source is placed where the [`Scene`] says it is, relative to its listener (see [`spatial::place`]); a source the scene does not place plays centred. Agent speech ducks while a person speaks ([`Duck`]). Each source has its own [`JitterBuffer`], so one late or bursty participant does not delay the others. Agent volume and mute are local to this mixer, and apply to agent speech played again from [`REPLAY_SOURCE`] as to live agent speech; muting still consumes agent audio in real time, so unmuting resumes live speech rather than replaying what was missed.
///
/// Sound cues ([`Self::play`]) are local: never sent, never a source, never muted with agent speech. They are rendered once when the mixer is created, so playing one allocates nothing. Silencing ([`Self::set_silenced`]) fades out everything but the cues that bypass it, then the limiter comes last.
pub struct Mixer {
    options: MixerOptions,
    sources: HashMap<u32, Source>,
    /// This client's own participant number, whose audio is never played back.
    local_source: Option<u32>,
    agent_volume: f32,
    agent_muted: bool,
    limiter: Limiter,
    accumulator: Vec<f32>,
    /// One source's contribution to the current [`Self::mix`] call, so its level can be measured.
    scratch: Vec<f32>,
    statistics: PlaybackStatistics,
    bank: Bank,
    voices: [Option<Voice>; VOICES],
    /// The cues that play through silencing, for the current [`Self::mix`] call.
    bypass: Vec<f32>,
    silenced: bool,
    scene: Scene,
    duck: Duck,
    /// How far through the echo probe playback is, while it plays.
    probe: Option<usize>,
    /// The layout the limiter was made for, since it counts its release in samples.
    limited: Layout,
    /// How far playback has faded out, in samples: 0 is full volume, and the fade length is silent.
    faded: u32,
    /// The ambient sounds playing.
    ambient: [Option<Ambient>; AMBIENT],
}

impl Mixer {
    pub fn new(options: MixerOptions) -> Self {
        assert!(
            options.sample_rate > 0,
            "device sample rate must be positive"
        );
        Self {
            options,
            sources: HashMap::new(),
            local_source: None,
            agent_volume: 1.0,
            agent_muted: false,
            limiter: Limiter::new(options.sample_rate),
            accumulator: Vec::new(),
            scratch: Vec::new(),
            statistics: PlaybackStatistics::default(),
            bank: Bank::new(options.sample_rate),
            voices: [None; VOICES],
            bypass: Vec::new(),
            silenced: false,
            faded: 0,
            scene: Scene::default(),
            duck: Duck::default(),
            probe: None,
            limited: Layout::Mono,
            ambient: Default::default(),
        }
    }

    /// Play `sound` from `position`, in the scene, unless someone can be heard speaking now, the agent or a person, so
    /// that it never plays over speech. Placed and made quieter with distance as a person's voice is, but without its
    /// delay or shadow. When [`AMBIENT`] already play, it replaces the one furthest through.
    pub fn ambient(&mut self, sound: Sound, position: spatial::Position) {
        let speaking = self.sources.iter().any(|(&id, source)| {
            if is_agent(id) {
                !self.agent_muted && source.buffer.is_playing()
            } else {
                source.loud
            }
        });
        if speaking || sound.rate == 0 {
            return;
        }
        let slot = self
            .ambient
            .iter()
            .position(Option::is_none)
            .unwrap_or_else(|| {
                (0..AMBIENT)
                    .max_by(|a, b| {
                        let played = |index: usize| {
                            self.ambient[index]
                                .as_ref()
                                .map_or(0.0, |playing| playing.played)
                        };
                        played(*a).total_cmp(&played(*b))
                    })
                    .expect("there are slots")
            });
        self.ambient[slot] = Some(Ambient {
            sound,
            position,
            played: 0.0,
        });
    }

    /// How many ambient sounds are playing.
    pub fn playing_ambient(&self) -> usize {
        self.ambient.iter().flatten().count()
    }

    /// Where the listener and the sources are, which the next mix glides to. The application keeps the scene current, typically once a frame; see [`super::PlayerControl::set_scene`].
    pub fn scene_mut(&mut self) -> &mut Scene {
        &mut self.scene
    }

    /// How far agent speech is ducked now, in decibels.
    pub fn duck_depth(&self) -> f32 {
        self.duck.depth()
    }

    /// Play a sound cue from its start, from `direction`. When [`VOICES`] already play, it replaces the one furthest through.
    pub fn play(&mut self, earcon: Earcon, direction: Direction) {
        let voice = Some(Voice {
            earcon,
            direction,
            position: 0,
        });
        let slot = match self.voices.iter().position(Option::is_none) {
            Some(free) => free,
            None => (0..VOICES)
                .max_by_key(|&index| self.voices[index].map_or(0, |voice| voice.position))
                .expect("there are voices"),
        };
        self.voices[slot] = voice;
    }

    /// Render the echo probe, so that [`Self::play_probe`] can play it. It is synthesized only for the echo test.
    pub fn load_probe(&mut self) {
        self.bank.load_probe();
    }

    /// Play the echo probe from its start, centred, once [`Self::load_probe`] has rendered it: the echo test measures how much of it the microphone hears. See [`super::probe`].
    pub fn play_probe(&mut self) {
        self.probe = (!self.bank.probe().is_empty()).then_some(0);
    }

    /// Whether the echo probe is playing.
    pub fn is_probing(&self) -> bool {
        self.probe.is_some()
    }

    /// How many sound cues are playing.
    pub fn playing_cues(&self) -> usize {
        self.voices.iter().flatten().count()
    }

    /// Silence playback, fading out over [`FADE`], while still consuming what arrives in real time; cues that [`Earcon::bypasses_silence`] still play. Used while the microphone records push-to-talk; see `design/live-audio.md`.
    pub fn set_silenced(&mut self, silenced: bool) {
        self.silenced = silenced;
    }

    /// Silence playback at once, without fading out, as for a mixer made while playback is already silenced.
    pub fn silence_now(&mut self) {
        self.silenced = true;
        self.faded = (FADE.as_secs_f64() * f64::from(self.options.sample_rate))
            .ceil()
            .max(1.0) as u32;
    }

    pub fn sample_rate(&self) -> u32 {
        self.options.sample_rate
    }

    /// Ignore audio from this participant number: a client never hears its own microphone. Set from [`tomori_protocol::ServerMessage::Welcome`].
    pub fn set_local_source(&mut self, source: Option<u32>) {
        self.local_source = source;
        if let Some(source) = source {
            self.clear_source(source);
        }
    }

    pub fn agent_volume(&self) -> f32 {
        self.agent_volume
    }

    /// Set the local agent speech gain. Negative values are treated as zero.
    pub fn set_agent_volume(&mut self, volume: f32) {
        self.agent_volume = volume.max(0.0);
    }

    pub fn is_agent_muted(&self) -> bool {
        self.agent_muted
    }

    pub fn set_agent_muted(&mut self, muted: bool) {
        self.agent_muted = muted;
    }

    pub fn statistics(&self) -> PlaybackStatistics {
        self.statistics
    }

    /// The sources currently buffered, including those still filling.
    pub fn sources(&self) -> impl Iterator<Item = u32> + '_ {
        self.sources.keys().copied()
    }

    /// Discard all buffered audio. The application owns the mixer, so it calls this when the audio it hears changes context: on [`tomori_protocol::ServerMessage::Left`], before joining a different space, and after reconnecting. Sound cues keep playing: they are this client's own, such as the click as it leaves a space.
    pub fn clear(&mut self) {
        self.sources.clear();
    }

    /// Discard one source's buffered audio, for example when a participant leaves the space. Its next frame starts a new buffer.
    pub fn clear_source(&mut self, source: u32) {
        self.sources.remove(&source);
    }

    /// Whether `source` is contributing audio now, rather than buffering, stalled or absent. Agent speech that is muted still counts as playing, because it is consumed in real time.
    pub fn is_source_playing(&self, source: u32) -> bool {
        self.sources
            .get(&source)
            .is_some_and(|source| source.buffer.is_playing())
    }

    /// Whether `source` has played or buffered audio within `within`: true through the short silences of a stream, such as the breath between spoken segments, when [`Self::is_source_playing`] is false.
    pub fn is_source_recent(&self, source: u32, within: Duration) -> bool {
        let within = (within.as_secs_f64() * f64::from(self.options.sample_rate)) as u64;
        self.sources.get(&source).is_some_and(|source| {
            source.buffer.is_playing()
                || source.buffer.buffered_samples() > 0
                || source.buffer.idle_samples() < within
        })
    }

    /// The recent loudness of what [`Self::mix`] played from `source`, before its gain, so that muted agent speech still has a level, from 0 (silent or absent) to 1 (full scale), for visuals such as a speaking indicator. It follows a louder signal at once and falls over about [`LEVEL_RELEASE`] when the source goes quiet.
    pub fn source_level(&self, source: u32) -> f32 {
        self.sources.get(&source).map_or(0.0, |source| source.level)
    }

    /// Queue a received frame for playback.
    pub fn push(&mut self, frame: AudioFrame) {
        if frame.gate == Gate::Off || Some(frame.source) == self.local_source {
            return;
        }
        let source = self.sources.entry(frame.source).or_insert_with(|| {
            Source::new(
                frame.sample_rate,
                self.options.sample_rate,
                self.options.jitter,
            )
        });
        if source.buffer.sample_rate() != frame.sample_rate {
            // A new speech segment can change rate; restart the source rather than play mismatched audio.
            *source = Source::new(
                frame.sample_rate,
                self.options.sample_rate,
                self.options.jitter,
            );
        }
        source.buffer.push(frame, &mut self.statistics);
    }

    /// Fill mono `output` with the next mixed samples. Sources are not panned, but their distance still changes their loudness. Never blocks: missing audio is silence.
    pub fn mix(&mut self, output: &mut [i16]) {
        self.render(output, Layout::Mono);
    }

    /// Fill `output`, interleaved left and right, with the next mixed samples, each source placed by the scene. Never blocks: missing audio is silence.
    pub fn mix_stereo(&mut self, output: &mut [i16]) {
        self.render(output, Layout::Stereo);
    }

    /// Each ear's gain for a cue from `direction`.
    fn direction(&self, direction: Direction, layout: Layout) -> [f32; 2] {
        // Without a listener, as on the console, nothing is placed, cues included.
        if layout == Layout::Mono || self.scene.mono || self.scene.listener.is_none() {
            return [1.0, 1.0];
        }
        match direction {
            Direction::Centre => [1.0, 1.0],
            Direction::Left => Placement::panned(-HAND_PAN).gain,
            Direction::Right => Placement::panned(HAND_PAN).gain,
            Direction::Agent => match (self.scene.listener, self.scene.position(AGENT_SOURCE)) {
                (Some(listener), Some(agent)) => {
                    Placement::panned(spatial::pan(&listener, agent)).gain
                }
                _ => [1.0, 1.0],
            },
        }
    }

    fn render(&mut self, output: &mut [i16], layout: Layout) {
        let channels = layout.channels();
        let frames = output.len() / channels;
        self.accumulator.clear();
        self.accumulator.resize(output.len(), 0.0);
        let rate = self.options.sample_rate;
        if self.limited != layout {
            self.limited = layout;
            self.limiter = Limiter::new(rate * channels as u32);
        }
        let agent_gain = if self.agent_muted {
            0.0
        } else {
            self.agent_volume
        };
        // People speaking in the last mix, as measured then rather than by the slower speaking indicator, duck agent speech.
        let speaking = self
            .sources
            .iter()
            .any(|(&id, source)| !is_agent(id) && source.loud);
        let (duck_from, duck_to) = self.duck.update(speaking, frames, rate);
        let release =
            (-(frames as f64) / (LEVEL_RELEASE.as_secs_f64() * f64::from(rate))).exp() as f32;
        for (&id, source) in self.sources.iter_mut() {
            let placement = placement(&self.scene, rate, id, layout);
            let gain = if is_agent(id) {
                (agent_gain * duck_from, agent_gain * duck_to)
            } else {
                (1.0, 1.0)
            };
            // Mixed at unity gain and measured before the gain is applied, so muted agent speech still shows as speaking.
            self.scratch.clear();
            self.scratch.resize(frames, 0.0);
            source
                .buffer
                .mix_into(&mut self.scratch, rate, 1.0, &mut self.statistics);
            let energy: f32 = self.scratch.iter().map(|sample| sample * sample).sum();
            let rms = (energy / frames.max(1) as f32).sqrt() / f32::from(i16::MAX);
            source.level = rms.min(1.0).max(source.level * release);
            source.loud = rms >= super::duck::THRESHOLD;
            match layout {
                Layout::Mono => {
                    source
                        .ears
                        .render_mono(&self.scratch, placement, gain, &mut self.accumulator)
                }
                Layout::Stereo => {
                    source
                        .ears
                        .render(&self.scratch, placement, gain, &mut self.accumulator)
                }
            }
        }
        let idle_limit = (IDLE_LIMIT.as_secs_f64() * f64::from(rate)) as u64;
        self.sources.retain(|_, source| {
            source.buffer.is_playing()
                || source.buffer.buffered_samples() > 0
                || source.buffer.idle_samples() < idle_limit
        });
        self.bypass.clear();
        self.bypass.resize(output.len(), 0.0);
        for index in 0..VOICES {
            let Some(voice) = self.voices[index] else {
                continue;
            };
            let gains = self.direction(voice.direction, layout);
            let samples = &self.bank.samples(voice.earcon)[voice.position..];
            let target = if voice.earcon.bypasses_silence() {
                &mut self.bypass
            } else {
                &mut self.accumulator
            };
            for (frame, sample) in target.chunks_exact_mut(channels).zip(samples) {
                for (total, gain) in frame.iter_mut().zip(gains) {
                    *total += sample * gain;
                }
            }
            let position = voice.position + samples.len().min(frames);
            self.voices[index] = (position < self.bank.samples(voice.earcon).len())
                .then_some(Voice { position, ..voice });
        }
        for slot in &mut self.ambient {
            let Some(playing) = slot.as_mut() else {
                continue;
            };
            let gains = match self.scene.listener {
                Some(listener) => {
                    spatial::place(
                        &listener,
                        playing.position,
                        spatial::PERSON_FLOOR,
                        rate,
                        self.scene.mono || layout == Layout::Mono,
                    )
                    .gain
                }
                None => [1.0, 1.0],
            };
            // Played at the mixer's rate, in a straight line between its own samples.
            let step = f64::from(playing.sound.rate) / f64::from(rate);
            let samples = &playing.sound.samples;
            for frame in self.accumulator.chunks_exact_mut(channels) {
                let at = playing.played as usize;
                if at + 1 >= samples.len() {
                    playing.played = samples.len() as f64;
                    break;
                }
                let fraction = (playing.played - at as f64) as f32;
                let sample = samples[at] + (samples[at + 1] - samples[at]) * fraction;
                for (total, gain) in frame.iter_mut().zip(gains) {
                    *total += sample * gain;
                }
                playing.played += step;
            }
            if playing.played as usize + 1 >= samples.len() {
                *slot = None;
            }
        }
        if let Some(position) = self.probe {
            let samples = &self.bank.probe()[position..];
            for (frame, sample) in self.accumulator.chunks_exact_mut(channels).zip(samples) {
                frame.iter_mut().for_each(|total| *total += sample);
            }
            let position = position + samples.len().min(frames);
            self.probe = (position < self.bank.probe().len()).then_some(position);
        }
        let length = (FADE.as_secs_f64() * f64::from(rate)).ceil().max(1.0) as u32;
        if self.silenced || self.faded > 0 {
            for frame in self.accumulator.chunks_exact_mut(channels) {
                self.faded = if self.silenced {
                    (self.faded + 1).min(length)
                } else {
                    self.faded.saturating_sub(1)
                };
                let gain = 1.0 - self.faded as f32 / length as f32;
                for sample in frame {
                    *sample *= gain;
                }
            }
        }
        for (total, sample) in self.accumulator.iter_mut().zip(&self.bypass) {
            *total += sample;
        }
        self.limiter.process(&self.accumulator, output);
    }
}

#[cfg(test)]
mod tests;
