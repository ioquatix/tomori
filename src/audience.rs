// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Live, opt-in microphone and assistant audio to the PC spectator. Bounded
//! queues drop audio rather than blocking capture/rendering or building latency.
use crate::speech::{SAMPLE_RATE, audio_frame_count};
use ndk::audio::{
    AudioDirection, AudioFormat, AudioInputPreset, AudioSharingMode, AudioStreamBuilder,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
    time::Duration,
};

enum Source {
    Mic(u64),
    Agent,
}
struct Packet {
    source: Source,
    bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct AudioSender {
    tx: SyncSender<Packet>,
    enabled: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
}

impl AudioSender {
    pub fn mic(&self, samples: &[f32]) {
        if self.enabled.load(Ordering::Acquire) {
            self.enqueue(Source::Mic(self.epoch.load(Ordering::Acquire)), samples, 1);
        }
    }
    pub fn agent(&self, samples: &[f32]) {
        self.enqueue(Source::Agent, samples, 3);
    }
    fn enqueue(&self, source: Source, samples: &[f32], step: usize) {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for (i, sample) in samples.iter().enumerate() {
            // 24 kHz -> 16 kHz for TTS; mic is already 16 kHz.
            if step == 3 && i % 3 == 2 {
                continue;
            }
            let sample = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        let _ = self.tx.try_send(Packet { source, bytes });
    }
}

pub struct Audience {
    enabled: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    pub lock: Arc<Mutex<()>>,
    sender: AudioSender,
    microphone: Option<JoinHandle<()>>,
    transport: Option<JoinHandle<()>>,
}

impl Audience {
    pub fn new(held: Arc<AtomicBool>) -> Self {
        let enabled = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let epoch = Arc::new(AtomicU64::new(0));
        let lock = Arc::new(Mutex::new(()));
        let (tx, rx) = mpsc::sync_channel::<Packet>(4);
        let sender = AudioSender {
            tx,
            enabled: enabled.clone(),
            epoch: epoch.clone(),
        };
        let (e, c, q, stop) = (
            enabled.clone(),
            connected.clone(),
            epoch.clone(),
            shutdown.clone(),
        );
        let transport = std::thread::Builder::new()
            .name("audience-audio-send".into())
            .spawn(move || {
                let client = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_millis(300))
                    .timeout_read(Duration::from_millis(300))
                    .timeout_write(Duration::from_millis(300))
                    .build();
                while !stop.load(Ordering::Relaxed) {
                    let packet = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(packet) => packet,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    let source = match packet.source {
                        Source::Mic(generation)
                            if e.load(Ordering::Acquire)
                                && generation == q.load(Ordering::Acquire) =>
                        {
                            "mic"
                        }
                        Source::Mic(_) => continue,
                        Source::Agent => "agent",
                    };
                    let result = client
                        .post(&format!("http://127.0.0.1:8765/spectator/audio/{source}"))
                        .set("Content-Type", "application/octet-stream")
                        .send_bytes(&packet.bytes);
                    if source == "mic" {
                        c.store(result.is_ok(), Ordering::Release);
                    }
                }
            })
            .expect("start audience audio transport");
        let (e, a, stop, guard, send) = (
            enabled.clone(),
            active.clone(),
            shutdown.clone(),
            lock.clone(),
            sender.clone(),
        );
        let microphone = std::thread::Builder::new()
            .name("audience-microphone".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if !e.load(Ordering::Acquire) || held.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    let Ok(_lock) = guard.try_lock() else {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    };
                    if !e.load(Ordering::Acquire) || held.load(Ordering::Relaxed) {
                        continue;
                    }
                    let result = (|| -> anyhow::Result<()> {
                        let stream = AudioStreamBuilder::new()?
                            .direction(AudioDirection::Input)
                            .sharing_mode(AudioSharingMode::Shared)
                            .channel_count(1)
                            .sample_rate(SAMPLE_RATE as i32)
                            .format(AudioFormat::PCM_Float)
                            .input_preset(AudioInputPreset::VoiceRecognition)
                            .open_stream()?;
                        anyhow::ensure!(
                            stream.sample_rate() == SAMPLE_RATE as i32
                                && stream.channel_count() == 1
                                && stream.format() == AudioFormat::PCM_Float,
                            "Unexpected live microphone format"
                        );
                        stream.request_start()?;
                        a.store(true, Ordering::Release);
                        let mut buffer = [0.0_f32; 800];
                        while e.load(Ordering::Acquire)
                            && !held.load(Ordering::Relaxed)
                            && !stop.load(Ordering::Relaxed)
                        {
                            // SAFETY: stream is mono f32; this thread owns its buffer.
                            let read =
                                unsafe { stream.read(buffer.as_mut_ptr().cast(), 800, 50_000_000) };
                            let count = audio_frame_count(read.map_err(i32::from), buffer.len())?;
                            send.mic(&buffer[..count]);
                        }
                        let _ = stream.request_stop();
                        Ok(())
                    })();
                    a.store(false, Ordering::Release);
                    if let Err(error) = result {
                        log::error!("Live microphone failed: {error:#}");
                        e.store(false, Ordering::Release);
                    }
                }
            })
            .expect("start audience microphone");
        Self {
            enabled,
            active,
            connected,
            shutdown,
            epoch,
            lock,
            sender,
            microphone: Some(microphone),
            transport: Some(transport),
        }
    }

    pub fn sender(&self) -> AudioSender {
        self.sender.clone()
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn set_enabled(&self, on: bool) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(on, Ordering::Release);
        if !on {
            self.connected.store(false, Ordering::Release);
        }
    }
    pub fn label(&self) -> &'static str {
        if !self.enabled() {
            "MIC OFF"
        } else if !self.connected.load(Ordering::Acquire) {
            "MIC LINKING"
        } else if self.active.load(Ordering::Acquire) {
            "MIC LIVE"
        } else {
            "MIC LIVE (X)"
        }
    }
    pub fn shutdown(&mut self) {
        self.set_enabled(false);
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.microphone.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.transport.take() {
            let _ = thread.join();
        }
    }
}
