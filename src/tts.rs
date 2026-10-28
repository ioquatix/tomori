//! Quest playback of PC-synthesized audio; all network/audio work stays off XR.
use crate::{
    speech::audio_frame_count,
    speech_activity::{SpeechJob, SpeechTracker},
    spoken,
};
use anyhow::{Context, Result, ensure};
use ndk::audio::{AudioDirection, AudioFormat, AudioSharingMode, AudioStreamBuilder};
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const SAMPLE_RATE: i32 = 24_000;
const MAX_AUDIO_BYTES: u64 = 4 * 1024 * 1024;

struct Job {
    activity: SpeechJob,
    text: String,
}

pub struct Speaker {
    commands: SyncSender<Job>,
    events: Receiver<String>,
    activity: SpeechTracker,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    enabled: bool,
    notice: Option<String>,
}

impl Speaker {
    pub fn new(audience: crate::audience::AudioSender) -> Self {
        let (commands, receiver) = mpsc::sync_channel::<Job>(8);
        let (sender, events) = mpsc::channel();
        let activity = SpeechTracker::default();
        let shutdown = Arc::new(AtomicBool::new(false));
        let s = shutdown.clone();
        let worker = std::thread::Builder::new()
            .name("quest-tts".into())
            .spawn(move || {
                let agent = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(2))
                    .timeout_read(Duration::from_secs(12))
                    .timeout_write(Duration::from_secs(2))
                    .build();
                while !s.load(Ordering::Relaxed) {
                    let job = match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    for text in spoken::chunks(&job.text, 200) {
                        if stopped(&job.activity, &s) {
                            break;
                        }
                        let result = fetch_audio(&agent, &text).and_then(|samples| {
                            if stopped(&job.activity, &s) {
                                return Ok(());
                            }
                            play(&samples, &job.activity, &s, &audience)
                        });
                        if let Err(error) = result {
                            if !s.load(Ordering::Relaxed) && job.activity.fail() {
                                log::warn!("TTS playback: {error:#}");
                                let _ = sender.send(format!("Speech unavailable: {error}"));
                            }
                            break;
                        }
                    }
                }
            });
        let (worker, notice) = match worker {
            Ok(worker) => (Some(worker), None),
            Err(error) => {
                activity.fail();
                (None, Some(format!("Speech thread unavailable: {error}")))
            }
        };
        Self {
            commands,
            events,
            activity,
            shutdown,
            worker,
            enabled: true,
            notice,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn is_playing(&self) -> bool {
        self.activity.snapshot().playing
    }
    pub fn activity(&self) -> crate::interaction::SpeechActivity {
        let activity = self.activity.snapshot();
        crate::interaction::SpeechActivity {
            pending: activity.pending,
            playing: activity.playing,
            level: activity.level,
            failed: activity.failed,
        }
    }
    pub fn notice(&self) -> Option<String> {
        self.notice.clone()
    }

    pub fn poll(&mut self) {
        while let Ok(error) = self.events.try_recv() {
            self.notice = Some(error);
        }
    }

    /// Cancel current and already queued audio; stale network responses cannot play.
    pub fn stop(&mut self) {
        self.activity.cancel();
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.stop();
        self.activity.clear_failure();
        self.notice = None;
    }

    pub fn speak(&mut self, text: String) {
        if !self.enabled || text.is_empty() {
            return;
        }
        self.notice = None;
        let job = Job {
            activity: self.activity.begin_job(),
            text,
        };
        if let Err(error) = self.commands.try_send(job) {
            let (job, notice) = match error {
                mpsc::TrySendError::Full(job) => {
                    (job, "Speech queue full; some replies were not spoken")
                }
                mpsc::TrySendError::Disconnected(job) => (job, "Speech thread unavailable"),
            };
            job.activity.fail();
            self.notice = Some(notice.into());
        }
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn stopped(job: &SpeechJob, shutdown: &AtomicBool) -> bool {
    shutdown.load(Ordering::Relaxed) || !job.is_current()
}

fn fetch_audio(agent: &ureq::Agent, text: &str) -> Result<Vec<f32>> {
    let response = agent
        .post("http://127.0.0.1:8765/tts")
        .send_json(serde_json::json!({"text": text}))
        .map_err(|error| match error {
            ureq::Error::Status(_, response) => {
                let body: serde_json::Value = response.into_json().unwrap_or_default();
                anyhow::anyhow!(
                    "{}",
                    body["error"].as_str().unwrap_or("PC speech service failed")
                )
            }
            error => anyhow::anyhow!("PC speech bridge: {error}"),
        })?;
    ensure!(
        response.header("x-sample-rate") == Some("24000"),
        "Unexpected TTS sample rate"
    );
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_AUDIO_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= MAX_AUDIO_BYTES && bytes.len().is_multiple_of(4),
        "Invalid speech audio"
    );
    let samples: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    ensure!(
        samples
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() <= 1.0),
        "Invalid speech samples"
    );
    Ok(samples)
}

fn play(
    samples: &[f32],
    job: &SpeechJob,
    shutdown: &AtomicBool,
    audience: &crate::audience::AudioSender,
) -> Result<()> {
    if stopped(job, shutdown) {
        return Ok(());
    }
    // Declared before the stream so it is dropped AFTER stream closure on every
    // return/error path. Cancellation must not hide hardware still playing.
    let playback;
    let stream = AudioStreamBuilder::new()?
        .direction(AudioDirection::Output)
        .sharing_mode(AudioSharingMode::Shared)
        .channel_count(1)
        .sample_rate(SAMPLE_RATE)
        .format(AudioFormat::PCM_Float)
        .open_stream()
        .context("opening headset audio")?;
    ensure!(
        stream.sample_rate() == SAMPLE_RATE
            && stream.channel_count() == 1
            && stream.format() == AudioFormat::PCM_Float,
        "Unexpected output audio format"
    );
    if stopped(job, shutdown) {
        return Ok(());
    }
    stream.request_start()?;
    playback = job.start_playback();
    log::info!("Playing synthesized speech on headset");
    let mut offset = 0;
    let mut last_write = Instant::now();
    while offset < samples.len() && !stopped(job, shutdown) {
        let count = (samples.len() - offset).min(1200);
        // SAFETY: stream is mono f32 and the requested slice remains valid;
        // the stream is owned by this thread without an AAudio callback.
        let result =
            unsafe { stream.write(samples[offset..].as_ptr().cast(), count as i32, 50_000_000) };
        let written = audio_frame_count(result.map_err(i32::from), count)?;
        playback.written(&samples[offset..offset + written]);
        audience.agent(&samples[offset..offset + written]);
        offset += written;
        if written > 0 {
            last_write = Instant::now();
        }
        ensure!(
            last_write.elapsed() < Duration::from_secs(3),
            "Headset audio stalled"
        );
    }
    // The last submitted block is not a persistent loudness measurement while
    // buffered frames drain (or cancellation stops the stream).
    playback.written(&[]);
    // Let the hardware consume the last buffered frames before closing.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !stopped(job, shutdown)
        && stream.frames_read() < stream.frames_written()
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !stopped(job, shutdown) {
        std::thread::sleep(Duration::from_millis(40));
    }
    let _ = stream.request_stop();
    log::info!(
        "Headset speech playback ended: {offset}/{} frames, interrupted={}",
        samples.len(),
        stopped(job, shutdown)
    );
    Ok(())
}
