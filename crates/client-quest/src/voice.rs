// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Android push-to-talk capture; recognition happens on the PC bridge.
use crate::speech::{SAMPLE_RATE, audio_frame_count};
use android_activity::AndroidApp;
use anyhow::{Context, Result, ensure};
use jni::{
    JavaVM,
    objects::{GlobalRef, JObject, JValue},
};
use ndk::audio::{
    AudioDirection, AudioFormat, AudioInputPreset, AudioSharingMode, AudioStreamBuilder,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

enum Event {
    Ready,
    Recording(Instant),
    Transcribing,
    Draft(String),
    Text(String),
    Error(String),
}

pub struct Voice {
    permission: Option<Permission>,
    commands: SyncSender<()>,
    events: Receiver<Event>,
    held: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    ready: bool,
    busy: bool,
    error: bool,
    was_held: bool,
    status: String,
    transcript: String,
    completed: Option<String>,
    recording_started: Option<Instant>,
    audience: crate::audience::Audience,
}

impl Voice {
    pub fn new(app: &AndroidApp) -> Self {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (sender, events) = mpsc::channel();
        let held = Arc::new(AtomicBool::new(false));
        let audience = crate::audience::Audience::new(held.clone());
        let cancelled = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let permission = Permission::new(app);
        let mut status = "Ready. Hold X to talk to the PC.".to_owned();
        let mut failed = false;
        let permission = match permission {
            Ok(p) => Some(p),
            Err(error) => {
                failed = true;
                status = format!("Microphone setup error: {error}");
                None
            }
        };
        let (worker_held, worker_cancelled, worker_shutdown) =
            (held.clone(), cancelled.clone(), shutdown.clone());
        let capture_context = CaptureContext {
            lock: audience.lock.clone(),
            audience: audience.sender(),
        };
        let worker = std::thread::Builder::new()
            .name("quest-speech".into())
            .spawn(move || {
                let result = worker(
                    receiver,
                    &sender,
                    worker_held,
                    worker_cancelled,
                    worker_shutdown,
                    capture_context,
                );
                if let Err(error) = result {
                    log::error!("Voice worker: {error:#}");
                    let _ = sender.send(Event::Error(format!("Voice unavailable: {error}")));
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                failed = true;
                status = format!("Voice thread failed: {error}");
                None
            }
        };
        Self {
            permission,
            commands,
            events,
            held,
            cancelled,
            shutdown,
            worker,
            ready: true,
            busy: false,
            error: failed,
            was_held: false,
            status,
            transcript: "Hold X and speak. The PC shows a live draft; release to send.".into(),
            completed: None,
            recording_started: None,
            audience,
        }
    }

    pub fn poll(&mut self, focused: bool) {
        if !focused {
            if self.audience.enabled() {
                self.audience.set_enabled(false);
            }
            self.held.store(false, Ordering::Relaxed);
            self.cancelled.store(true, Ordering::Relaxed);
            self.was_held = false;
            self.recording_started = None;
        }
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Ready => {
                    self.ready = true;
                    self.busy = false;
                    self.error = self.permission.is_none() || self.worker.is_none();
                    self.recording_started = None;
                    self.status = "Ready. Hold X to talk to the PC.".into();
                }
                Event::Recording(started) => {
                    if self.held.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed)
                    {
                        self.busy = true;
                        self.error = false;
                        self.recording_started = Some(started);
                        self.status = "RECORDING - live transcription on PC".into();
                    }
                }
                Event::Transcribing => {
                    self.busy = true;
                    self.error = false;
                    self.recording_started = None;
                    self.status = "Finalizing transcript on PC...".into()
                }
                Event::Draft(text) => {
                    if self.held.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed)
                    {
                        self.transcript = text;
                    }
                }
                Event::Text(text) => {
                    if !text.is_empty() {
                        self.completed = Some(text.clone());
                    }
                    self.busy = false;
                    self.error = self.permission.is_none() || self.worker.is_none();
                    self.recording_started = None;
                    self.status = "Ready. Hold X to speak again.".into();
                    self.transcript = if text.is_empty() {
                        "No speech detected. Hold X and try again.".into()
                    } else {
                        text
                    };
                }
                Event::Error(error) => {
                    self.busy = false;
                    self.error = true;
                    self.recording_started = None;
                    self.status = error;
                }
            }
        }
    }

    pub fn update(&mut self, pressed: bool, focused: bool) -> bool {
        let mut started = false;
        if !focused {
            self.cancelled.store(true, Ordering::Relaxed);
        }
        let pressed = pressed && focused;
        self.held.store(pressed, Ordering::Relaxed);
        if !pressed {
            if self.recording_started.is_some() {
                self.status = "Finishing audio upload to PC...".into();
            }
            self.recording_started = None;
        }
        if pressed && !self.was_held && self.ready && !self.busy {
            match self
                .permission
                .as_mut()
                .context("Microphone permission API unavailable")
                .and_then(Permission::check_or_request)
            {
                Ok(true) => {
                    self.cancelled.store(false, Ordering::Relaxed);
                    if self.commands.try_send(()).is_ok() {
                        self.busy = true;
                        self.error = false;
                        started = true;
                        self.status = "Starting PC transcription...".into();
                        self.transcript.clear();
                    } else {
                        self.error = true;
                        self.status =
                            "Voice worker busy/unavailable. Nothing recorded; try again.".into();
                    }
                }
                Ok(false) => {
                    self.error = true;
                    self.status = "Allow microphone access, then hold X again. If denied, enable it in app permissions.".into();
                }
                Err(error) => {
                    self.error = true;
                    self.status = format!("Microphone permission error: {error}");
                }
            }
        }
        self.was_held = pressed;
        started
    }

    pub fn take_transcript(&mut self) -> Option<String> {
        self.completed.take()
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    /// Includes startup, capture, upload, and final transcription. Recording
    /// itself remains separately observable through `recording_elapsed`.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn has_error(&self) -> bool {
        self.error
    }

    pub fn draft(&self) -> &str {
        &self.transcript
    }

    pub fn audio_sender(&self) -> crate::audience::AudioSender {
        self.audience.sender()
    }
    pub fn audience_label(&self) -> &'static str {
        self.audience.label()
    }

    pub fn toggle_audience(&mut self) {
        if self.audience.enabled() {
            self.audience.set_enabled(false);
            return;
        }
        match self
            .permission
            .as_mut()
            .context("Microphone permission API unavailable")
            .and_then(Permission::check_or_request)
        {
            Ok(true) => {
                self.error = self.permission.is_none() || self.worker.is_none();
                self.audience.set_enabled(true);
            }
            Ok(false) => {
                self.error = true;
                self.status = "Allow microphone access, then press left grip again.".into();
            }
            Err(error) => {
                self.error = true;
                self.status = format!("Microphone permission error: {error}");
            }
        }
    }

    pub fn recording_elapsed(&self) -> Option<Duration> {
        self.recording_started
            .filter(|_| {
                self.held.load(Ordering::Relaxed) && !self.cancelled.load(Ordering::Relaxed)
            })
            .map(|started| started.elapsed())
    }

    pub fn text(&self) -> String {
        format!(
            "VOICE TO TEXT / PC ENGLISH\n{}\n\n{}",
            self.status, self.transcript
        )
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.cancelled.store(true, Ordering::Relaxed);
        self.held.store(false, Ordering::Relaxed);
        self.audience.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct CaptureContext {
    lock: Arc<std::sync::Mutex<()>>,
    audience: crate::audience::AudioSender,
}

fn worker(
    commands: Receiver<()>,
    events: &mpsc::Sender<Event>,
    held: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    capture_context: CaptureContext,
) -> Result<()> {
    while !shutdown.load(Ordering::Relaxed) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(()) => {}
        }
        if !held.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
            events.send(Event::Ready)?;
            continue;
        }
        let result = transcribe_utterance(&held, &cancelled, events, &capture_context);
        if cancelled.load(Ordering::Relaxed) {
            events.send(Event::Ready)?;
            continue;
        }
        match result {
            Ok(text) => {
                log::info!("PC transcription finalized ({} characters)", text.len());
                events.send(Event::Text(text))?;
            }
            Err(error) => {
                log::warn!("PC transcription: {error:#}");
                events.send(Event::Error(format!(
                    "PC transcription failed: {error}. Nothing sent."
                )))?;
            }
        }
    }
    Ok(())
}

fn pc_response(result: std::result::Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(_, response)) => {
            let body: serde_json::Value = response.into_json().unwrap_or_default();
            anyhow::bail!(
                "{}",
                body["error"].as_str().unwrap_or("PC transcription failed")
            );
        }
        Err(error) => Err(error).context("PC transcription unavailable; check the USB bridge"),
    }
}

fn transcribe_utterance(
    held: &AtomicBool,
    cancelled: &AtomicBool,
    events: &mpsc::Sender<Event>,
    context: &CaptureContext,
) -> Result<String> {
    let id = format!(
        "vr_{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let client = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(8))
        .timeout_write(Duration::from_secs(2))
        .build();
    let base = format!("http://127.0.0.1:8765/transcription/{id}");
    pc_response(
        client
            .post("http://127.0.0.1:8765/transcription/start")
            .send_json(serde_json::json!({"stream": id})),
    )?;
    let cancel = || {
        let _ = client
            .post(&format!("{base}/cancel"))
            .send_json(serde_json::json!({}));
    };
    if !held.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
        cancel();
        return Ok(String::new());
    }
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(16);
    let failed = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        let client = &client;
        let base = &base;
        let failed = &failed;
        let network = scope.spawn(move || -> Result<String> {
            let mut sequence = 0u64;
            let mut last_draft = String::new();
            while let Ok(bytes) = receiver.recv() {
                if cancelled.load(Ordering::Relaxed) || failed.load(Ordering::Relaxed) {
                    anyhow::bail!("Recording cancelled");
                }
                let response = pc_response(
                    client
                        .post(&format!("{base}/audio"))
                        .set("Content-Type", "application/octet-stream")
                        .set("x-audio-sequence", &sequence.to_string())
                        .send_bytes(&bytes),
                )?;
                let data: serde_json::Value = response.into_json()?;
                let draft = data["text"]
                    .as_str()
                    .context("PC transcription returned invalid draft")?;
                if draft != last_draft {
                    last_draft = draft.to_owned();
                    events.send(Event::Draft(last_draft.clone()))?;
                }
                sequence += 1;
            }
            if cancelled.load(Ordering::Relaxed) || failed.load(Ordering::Relaxed) {
                anyhow::bail!("Recording cancelled");
            }
            events.send(Event::Transcribing)?;
            let response = pc_response(
                client
                    .post(&format!("{base}/finish"))
                    .send_json(serde_json::json!({"sequence": sequence})),
            )?;
            let data: serde_json::Value = response.into_json()?;
            Ok(data["text"]
                .as_str()
                .context("PC transcription returned invalid result")?
                .to_owned())
        });
        let captured = capture(held, cancelled, events, context, &sender);
        if captured.is_err() {
            failed.store(true, Ordering::Relaxed);
        }
        drop(sender);
        let decoded = network
            .join()
            .map_err(|_| anyhow::anyhow!("PC transcription thread failed"))?;
        captured?;
        decoded
    });
    if result.is_err() || cancelled.load(Ordering::Relaxed) {
        cancel();
    }
    result
}

fn capture(
    held: &AtomicBool,
    cancelled: &AtomicBool,
    events: &mpsc::Sender<Event>,
    context: &CaptureContext,
    chunks: &SyncSender<Vec<u8>>,
) -> Result<()> {
    let _exclusive = context
        .lock
        .lock()
        .map_err(|_| anyhow::anyhow!("microphone lock poisoned"))?;
    if !held.load(Ordering::Relaxed) || cancelled.load(Ordering::Relaxed) {
        return Ok(());
    }
    // Shared mode supports Android's resampler, so request Whisper's exact
    // 16 kHz mono format rather than misinterpreting native-rate audio.
    let stream = AudioStreamBuilder::new()?
        .direction(AudioDirection::Input)
        .sharing_mode(AudioSharingMode::Shared)
        .channel_count(1)
        .sample_rate(SAMPLE_RATE as i32)
        .format(AudioFormat::PCM_Float)
        .input_preset(AudioInputPreset::VoiceRecognition)
        .open_stream()?;
    ensure!(
        stream.sample_rate() == SAMPLE_RATE as i32
            && stream.channel_count() == 1
            && stream.format() == AudioFormat::PCM_Float,
        "Unexpected microphone format"
    );
    stream.request_start()?;
    let started = Instant::now();
    events.send(Event::Recording(started))?;
    log::info!("Voice microphone opened (16 kHz mono)");
    let mut pcm = Vec::with_capacity(8000);
    let mut buffer = [0.0_f32; 800];
    while held.load(Ordering::Relaxed) && !cancelled.load(Ordering::Relaxed) {
        // SAFETY: buffer is 800 f32 samples; negotiated input format is mono
        // PCM float. No callback or other thread accesses this stream/buffer.
        let result =
            unsafe { stream.read(buffer.as_mut_ptr().cast(), buffer.len() as i32, 50_000_000) };
        let count = audio_frame_count(result.map_err(i32::from), buffer.len())?;
        context.audience.mic(&buffer[..count]);
        for &sample in &buffer[..count] {
            pcm.extend_from_slice(
                &((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes(),
            );
        }
        if pcm.len() >= 8000 {
            chunks.try_send(std::mem::take(&mut pcm)).map_err(|_| {
                anyhow::anyhow!("PC transcription cannot keep up; recording cancelled")
            })?;
            pcm = Vec::with_capacity(8000);
        }
    }
    if !pcm.is_empty() && !cancelled.load(Ordering::Relaxed) {
        chunks
            .try_send(pcm)
            .map_err(|_| anyhow::anyhow!("PC transcription cannot keep up; recording cancelled"))?;
    }
    let _ = stream.request_stop();
    drop(stream); // closes the microphone BEFORE recognition begins
    log::info!(
        "Voice microphone closed ({:.1}s captured)",
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

struct Permission {
    vm: JavaVM,
    activity: GlobalRef,
    requested: bool,
}

impl Permission {
    fn new(app: &AndroidApp) -> Result<Self> {
        // SAFETY: AndroidApp owns the live VM and Activity references. Create
        // our own GlobalRef so JNI use remains valid for this Voice lifetime.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
        let activity = {
            let env = vm.attach_current_thread()?;
            let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
            env.new_global_ref(activity)?
        };
        Ok(Self {
            vm,
            activity,
            requested: false,
        })
    }

    fn check_or_request(&mut self) -> Result<bool> {
        let mut env = self.vm.attach_current_thread()?;
        env.with_local_frame(8, |env| -> jni::errors::Result<bool> {
            let name = env.new_string("android.permission.RECORD_AUDIO")?;
            let granted = env
                .call_method(
                    self.activity.as_obj(),
                    "checkSelfPermission",
                    "(Ljava/lang/String;)I",
                    &[JValue::Object(&name)],
                )?
                .i()?
                == 0;
            if !granted && !self.requested {
                let permissions = env.new_object_array(1, "java/lang/String", &name)?;
                env.call_method(
                    self.activity.as_obj(),
                    "requestPermissions",
                    "([Ljava/lang/String;I)V",
                    &[JValue::Object(&permissions), JValue::Int(101)],
                )?;
                self.requested = true;
            }
            Ok(granted)
        })
        .map_err(Into::into)
    }
}
