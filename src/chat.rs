// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Headset session browser and a background HTTP client for the loopback bridge.
use crate::caption::{COLUMNS, ROWS, wrapped_lines};
use anyhow::{Context, Result};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const BRIDGE: &str = "http://127.0.0.1:8765";
const BODY_ROWS: usize = ROWS - 4;
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(3);
// An accepted bridge action can precede the server's running/message update.
// Require a later fetch after a bounded grace period, not just the POST reply.
const ACTION_SETTLE: Duration = Duration::from_secs(2);

#[derive(Clone, Default, Deserialize)]
struct Session {
    id: String,
    title: String,
    directory: String,
    #[serde(default)]
    running: bool,
}

#[derive(Clone, Default, Deserialize)]
struct Message {
    id: String,
    role: String,
    text: String,
    #[serde(default)]
    speech_text: String,
    #[serde(default)]
    completed: bool,
}

#[derive(Clone, Default, Deserialize)]
struct Approval {
    id: String,
    title: String,
    text: String,
}

#[derive(Clone, Default, Deserialize)]
struct Form {
    id: String,
    title: String,
    text: String,
    field_key: String,
    supported: bool,
}

#[derive(Clone, Default, Deserialize)]
struct Snapshot {
    id: String,
    title: String,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    messages: Vec<Message>,
    approval: Option<Approval>,
    form: Option<Form>,
    #[serde(
        default = "missing_workspace",
        deserialize_with = "deserialize_workspace"
    )]
    delegation: crate::delegation::Workspace,
}

// A malformed optional projection must not discard messages or permission UI.
fn deserialize_workspace<'de, D>(
    deserializer: D,
) -> std::result::Result<crate::delegation::Workspace, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(missing_workspace());
    }
    // Missing projection fields cannot be taken as authoritative empty data.
    let fields_present = |value: &Value, fields: &[&str]| {
        value
            .as_object()
            .is_some_and(|object| fields.iter().all(|key| object.contains_key(*key)))
    };
    let records_complete = |value: &Value, fields: &[&str]| {
        value
            .as_array()
            .is_some_and(|records| records.iter().all(|record| fields_present(record, fields)))
    };
    if !fields_present(&value, &["nodes", "presentations", "error", "truncated"])
        || !records_complete(&value["nodes"], &["id", "parent_id", "title", "state"])
        || !records_complete(
            &value["presentations"],
            &[
                "id",
                "source_id",
                "source_message_id",
                "title",
                "kind",
                "text",
                "columns",
                "rows",
            ],
        )
    {
        return Ok(invalid_workspace());
    }
    Ok(serde_json::from_value(value).unwrap_or_else(|_| invalid_workspace()))
}

fn missing_workspace() -> crate::delegation::Workspace {
    crate::delegation::Workspace {
        error: Some("Delegation projection unavailable.".into()),
        stale: true,
        ..Default::default()
    }
}

fn invalid_workspace() -> crate::delegation::Workspace {
    crate::delegation::Workspace {
        error: Some("Invalid delegation projection; normal chat remains available.".into()),
        stale: true,
        ..Default::default()
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Message(String),
    Form {
        session: String,
        form: String,
        key: String,
    },
    Blocked(String),
}

enum Command {
    Watch(Option<String>, u64),
    Refresh,
    Send(Target, String, String),
    Approve {
        action: String,
        session: String,
        request: String,
        decision: &'static str,
    },
}
enum Event {
    Sessions(Vec<Session>),
    Snapshot(Box<Snapshot>, Instant, u64),
    ActionDone {
        action: String,
        completed_at: Instant,
        notice: String,
    },
    Error {
        session: Option<String>,
        error: String,
        watch_generation: u64,
    },
    ActionError {
        action: String,
        error: String,
    },
}

struct PendingAction {
    id: String,
    session: String,
    acknowledged: Option<Instant>,
}
#[derive(PartialEq)]
enum Mode {
    Sessions,
    Chat,
    Voice,
}

pub struct Chat {
    commands: SyncSender<Command>,
    events: Receiver<Event>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    sessions: Vec<Session>,
    selected: usize,
    open: Option<String>,
    watch_generation: u64,
    snapshot: Option<Snapshot>,
    snapshot_at: Option<Instant>,
    snapshot_failed: bool,
    stale_workspace: Option<crate::delegation::Workspace>,
    pending_actions: Vec<PendingAction>,
    mode: Mode,
    message: usize,
    page: usize,
    follow: bool,
    pending_id: String,
    target: Option<Target>,
    notice: String,
    error: Option<String>,
    action_error: Option<String>,
    speech_tracker: crate::spoken::Tracker,
    speech_ready: Vec<String>,
    tts_enabled: bool,
    tts_notice: Option<String>,
    audience_label: &'static str,
}

impl Chat {
    pub fn new() -> Self {
        let (commands, receiver) = mpsc::sync_channel(16);
        let (sender, events) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let worker = std::thread::Builder::new()
            .name("opencode-chat".into())
            .spawn(move || network(receiver, sender, stop));
        let (worker, error) = match worker {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            commands,
            events,
            shutdown,
            worker,
            sessions: vec![],
            selected: 0,
            open: None,
            watch_generation: 0,
            snapshot: None,
            snapshot_at: None,
            snapshot_failed: false,
            stale_workspace: None,
            pending_actions: Vec::new(),
            mode: Mode::Sessions,
            message: 0,
            page: 0,
            follow: true,
            pending_id: String::new(),
            target: None,
            notice: "Connecting to OpenCode...".into(),
            error,
            action_error: None,
            speech_tracker: Default::default(),
            speech_ready: Vec::new(),
            tts_enabled: true,
            tts_notice: None,
            audience_label: "MIC OFF",
        }
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Sessions(sessions) => {
                    if self.sessions.is_empty() && !sessions.is_empty() {
                        log::info!(
                            "VR connected: {} existing OpenCode sessions",
                            sessions.len()
                        );
                    }
                    let selected = self.sessions.get(self.selected).map(|s| s.id.clone());
                    self.selected = selected
                        .and_then(|id| sessions.iter().position(|s| s.id == id))
                        .unwrap_or(0);
                    self.sessions = sessions;
                    self.error = None;
                }
                Event::Snapshot(mut snapshot, fetched_at, generation)
                    if self.mode == Mode::Chat
                        && self.watch_generation == generation
                        && self.open.as_deref() == Some(&snapshot.id) =>
                {
                    self.snapshot_at = Some(fetched_at);
                    self.snapshot_failed = false;
                    if !snapshot.delegation.valid() {
                        snapshot.delegation = invalid_workspace();
                    }
                    let mut stale = snapshot.delegation.clone();
                    stale.stale = true;
                    stale.error = Some(
                        "Delegation stale: no fresh snapshot for over 3 seconds; showing last known data."
                            .into(),
                    );
                    self.stale_workspace = Some(stale);
                    self.pending_actions.retain(|action| {
                        action.session != snapshot.id
                            || action.acknowledged.is_none_or(|acknowledged| {
                                fetched_at.saturating_duration_since(acknowledged) < ACTION_SETTLE
                            })
                    });
                    self.speech_ready.extend(self.speech_tracker.collect(
                        snapshot.messages.iter().map(|message| {
                            (
                                message.id.as_str(),
                                message.role.as_str(),
                                message.speech_text.as_str(),
                                message.completed,
                            )
                        }),
                    ));
                    let old_message = self
                        .snapshot
                        .as_ref()
                        .and_then(|s| s.messages.get(self.message))
                        .map(|m| m.id.clone());
                    let pending = snapshot
                        .approval
                        .as_ref()
                        .map(|p| p.id.clone())
                        .or_else(|| {
                            snapshot
                                .form
                                .as_ref()
                                .map(|f| format!("{}/{}", f.id, f.field_key))
                        })
                        .unwrap_or_default();
                    if pending != self.pending_id {
                        self.page = 0;
                        self.pending_id = pending;
                    }
                    if self.follow {
                        self.message = snapshot.messages.len().saturating_sub(1);
                        if self.pending_id.is_empty() {
                            self.page = snapshot
                                .messages
                                .last()
                                .map(|m| pages(&m.text).len().saturating_sub(1))
                                .unwrap_or(0);
                        }
                    } else {
                        self.message = old_message
                            .and_then(|id| snapshot.messages.iter().position(|m| m.id == id))
                            .unwrap_or(0);
                    }
                    self.snapshot = Some(*snapshot);
                    self.error = None;
                }
                Event::Snapshot(..) => {} // an old request completed after switching sessions
                Event::ActionDone {
                    action,
                    completed_at,
                    notice,
                } => {
                    if let Some(pending) = self.pending_actions.iter_mut().find(|p| p.id == action)
                    {
                        pending.acknowledged = Some(completed_at);
                    }
                    log::info!("VR OpenCode: {notice}");
                    self.notice = notice;
                    self.error = None;
                    self.action_error = None;
                }
                Event::Error {
                    session,
                    error,
                    watch_generation,
                } if (self.mode == Mode::Chat
                    && watch_generation == self.watch_generation
                    && session.as_ref() == self.open.as_ref())
                    || (self.mode != Mode::Chat && session.is_none()) =>
                {
                    self.snapshot_failed = self.mode == Mode::Chat;
                    if self.snapshot_failed
                        && let Some(snapshot) = &mut self.snapshot
                    {
                        snapshot.delegation.stale = true;
                        snapshot.delegation.error = Some(
                            "Delegation unavailable: bridge fetch failed; showing last known data."
                                .into(),
                        );
                    }
                    self.error = Some(error);
                }
                Event::Error { .. } => {} // a fetch for a previous watch failed
                Event::ActionError { action, error } => {
                    self.pending_actions.retain(|pending| pending.id != action);
                    self.action_error = Some(error);
                    self.page = 0;
                    self.follow = false;
                }
            }
        }
    }

    /// Borrow only the currently watched root's projection, never a previous selection.
    pub fn workspace(&self) -> Option<(&str, &crate::delegation::Workspace)> {
        self.workspace_at(Instant::now())
    }

    fn workspace_at(&self, now: Instant) -> Option<(&str, &crate::delegation::Workspace)> {
        if self.mode != Mode::Chat {
            return None;
        }
        let root = self.open.as_deref()?;
        let snapshot = self
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.id == root)?;
        let fetched_at = self.snapshot_at?;
        let workspace = if !self.snapshot_failed
            && now.saturating_duration_since(fetched_at) > SNAPSHOT_MAX_AGE
        {
            self.stale_workspace.as_ref()?
        } else {
            &snapshot.delegation
        };
        Some((root, workspace))
    }

    fn command(&mut self, command: Command) {
        let action = match &command {
            Command::Send(Target::Message(session), id, _)
            | Command::Send(Target::Form { session, .. }, id, _)
            | Command::Approve {
                session,
                action: id,
                ..
            } => Some(PendingAction {
                id: id.clone(),
                session: session.clone(),
                acknowledged: None,
            }),
            _ => None,
        };
        if self.commands.try_send(command).is_err() {
            self.action_error = Some("Bridge busy/unavailable. Nothing queued; try again.".into());
        } else if let Some(action) = action {
            self.pending_actions.push(action);
        }
    }

    pub fn activity(&self) -> crate::interaction::SessionActivity {
        self.activity_at(Instant::now())
    }

    fn activity_at(&self, now: Instant) -> crate::interaction::SessionActivity {
        use crate::interaction::SessionActivity;
        if self.mode != Mode::Chat || self.open.is_none() {
            return SessionActivity::Unselected;
        }
        if self.error.is_some() || self.action_error.is_some() || self.snapshot_failed {
            return SessionActivity::Unavailable;
        }
        let (Some(snapshot), Some(fetched_at)) = (&self.snapshot, self.snapshot_at) else {
            return SessionActivity::Unknown;
        };
        if now.saturating_duration_since(fetched_at) > SNAPSHOT_MAX_AGE {
            return SessionActivity::Unknown;
        }
        if self
            .pending_actions
            .iter()
            .any(|action| Some(&action.session) == self.open.as_ref())
        {
            SessionActivity::Working
        } else if snapshot.approval.is_some() || snapshot.form.is_some() {
            SessionActivity::Waiting
        } else if snapshot.running {
            SessionActivity::Working
        } else {
            SessionActivity::Ready
        }
    }

    pub fn has_pending_speech(&self) -> bool {
        self.mode == Mode::Chat && !self.speech_ready.is_empty()
    }

    pub fn navigate(&mut self, horizontal: i32, vertical: i32) {
        match self.mode {
            Mode::Sessions => {
                self.selected = shift(self.selected, vertical, self.sessions.len());
            }
            Mode::Chat => {
                let Some(snapshot) = &self.snapshot else {
                    return;
                };
                if snapshot.approval.is_none() && snapshot.form.is_none() && horizontal != 0 {
                    self.message = shift(self.message, horizontal, snapshot.messages.len());
                    self.page = 0;
                }
                if vertical != 0 {
                    self.page = if vertical < 0 {
                        self.page.saturating_sub(1)
                    } else {
                        self.page.saturating_add(1)
                    };
                }
                self.follow = false;
            }
            Mode::Voice => {}
        }
    }

    /// Returns true only when A should retain its original swarm-reset meaning.
    pub fn accept(&mut self) -> bool {
        match self.mode {
            Mode::Sessions => {
                if let Some(session) = self.sessions.get(self.selected) {
                    let id = session.id.clone();
                    log::info!("VR selected OpenCode session {id}");
                    self.open = Some(id.clone());
                    self.watch_generation = self.watch_generation.wrapping_add(1);
                    self.speech_tracker.reset();
                    self.speech_ready.clear();
                    self.snapshot = None;
                    self.snapshot_at = None;
                    self.snapshot_failed = false;
                    self.stale_workspace = None;
                    self.mode = Mode::Chat;
                    self.page = 0;
                    self.follow = true;
                    self.pending_id.clear();
                    self.notice = "Hold X: speak and send to this session.".into();
                    self.command(Command::Watch(Some(id), self.watch_generation));
                }
                false
            }
            Mode::Chat => {
                if let Some((session, request)) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.approval.as_ref().map(|p| (s.id.clone(), p.id.clone())))
                {
                    self.command(Command::Approve {
                        action: message_id(),
                        session,
                        request,
                        decision: "once",
                    });
                    self.notice = "Submitting allow-once decision...".into();
                    false
                } else {
                    true
                }
            }
            Mode::Voice => true,
        }
    }

    pub fn back(&mut self) {
        if self.mode == Mode::Chat {
            if let Some((session, request)) = self
                .snapshot
                .as_ref()
                .and_then(|s| s.approval.as_ref().map(|p| (s.id.clone(), p.id.clone())))
            {
                self.command(Command::Approve {
                    action: message_id(),
                    session,
                    request,
                    decision: "reject",
                });
                self.notice = "Submitting deny decision...".into();
                return;
            }
            self.mode = Mode::Sessions;
        } else {
            self.mode = if self.mode == Mode::Sessions {
                Mode::Voice
            } else {
                Mode::Sessions
            };
        }
        self.speech_ready.clear();
        self.watch_generation = self.watch_generation.wrapping_add(1);
        self.command(Command::Watch(None, self.watch_generation));
    }

    pub fn refresh(&mut self) {
        self.mode = Mode::Sessions;
        self.speech_ready.clear();
        self.watch_generation = self.watch_generation.wrapping_add(1);
        self.command(Command::Watch(None, self.watch_generation));
        self.command(Command::Refresh);
    }

    /// Keep newly completed replies while the user is recording, then speak
    /// them on release. Muting, leaving the session, or losing focus discards
    /// them instead of replaying unexpected stale speech later.
    pub fn take_speech(&mut self, focused: bool, talking: bool, enabled: bool) -> Vec<String> {
        if !focused || !enabled || self.mode != Mode::Chat {
            self.speech_ready.clear();
            Vec::new()
        } else if talking {
            Vec::new()
        } else {
            std::mem::take(&mut self.speech_ready)
        }
    }
    pub fn set_tts_enabled(&mut self, enabled: bool) {
        self.tts_enabled = enabled;
    }
    pub fn set_tts_notice(&mut self, notice: Option<String>) {
        self.tts_notice = notice;
    }
    pub fn set_audience_label(&mut self, label: &'static str) {
        self.audience_label = label;
    }
    pub fn replay_speech(&self) -> Option<String> {
        if self.mode != Mode::Chat {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let selected = snapshot
            .messages
            .get(self.message)
            .filter(|m| m.role == "assistant" && !m.speech_text.is_empty());
        let message = selected.or_else(|| {
            snapshot
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant" && m.completed && !m.speech_text.is_empty())
        })?;
        let text = crate::spoken::clean(&message.speech_text);
        (!text.is_empty()).then_some(text)
    }

    pub fn begin_utterance(&mut self) {
        // Bind at RECORD START, not decode completion. A later session switch
        // must never redirect the user's spoken message to another session.
        if self.mode == Mode::Chat && self.snapshot.is_none() {
            self.target = Some(Target::Blocked(
                "Wait for the session to load before speaking.".into(),
            ));
            return;
        }
        self.target = self
            .snapshot
            .as_ref()
            .filter(|_| self.mode == Mode::Chat)
            .map(|snapshot| {
                if snapshot.approval.is_some() {
                    Target::Blocked("Use A/B for the pending permission. Nothing sent.".into())
                } else if let Some(form) = &snapshot.form {
                    if form.supported {
                        Target::Form {
                            session: snapshot.id.clone(),
                            form: form.id.clone(),
                            key: form.field_key.clone(),
                        }
                    } else {
                        Target::Blocked("Complete this question in desktop OpenCode.".into())
                    }
                } else {
                    Target::Message(snapshot.id.clone())
                }
            });
    }

    pub fn transcript(&mut self, text: String) {
        match self.target.take() {
            Some(Target::Blocked(note)) => self.action_error = Some(note),
            Some(target) => {
                self.follow = true;
                self.notice = "Sending transcript to OpenCode...".into();
                self.action_error = None;
                self.command(Command::Send(target, message_id(), text));
            }
            None => {
                self.mode = Mode::Voice;
                self.notice = "Voice preview only: no session selected.".into();
            }
        }
    }

    pub fn text(
        &mut self,
        voice_status: &str,
        voice_preview: &str,
        live_draft: Option<&str>,
    ) -> String {
        if let Some(draft) = live_draft {
            let destination = match &self.target {
                Some(Target::Message(id)) | Some(Target::Form { session: id, .. }) => self
                    .sessions
                    .iter()
                    .find(|s| &s.id == id)
                    .map(|s| s.title.as_str())
                    .unwrap_or(id),
                Some(Target::Blocked(_)) => "Not sending",
                None => "Voice preview",
            };
            let mut lines = vec![
                short(&format!("{} | LIVE DRAFT", self.audience_label)),
                short(&format!("To: {destination}")),
                "Draft may change while speaking.".into(),
            ];
            // Only layout the tail; the full transcript remains untouched for
            // sending even after a very long recording.
            let tail: String = draft
                .chars()
                .rev()
                .take(1200)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let wrapped = wrapped_lines(if tail.is_empty() {
                "Listening..."
            } else {
                &tail
            });
            lines.extend(
                wrapped
                    .into_iter()
                    .rev()
                    .take(ROWS - 4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
            while lines.len() < ROWS - 1 {
                lines.push(String::new());
            }
            lines.push(
                if matches!(
                    self.target,
                    Some(Target::Message(_)) | Some(Target::Form { .. })
                ) {
                    "Release X to finalize and send".into()
                } else {
                    "Release X to finalize preview".into()
                },
            );
            return lines.join("\n");
        }
        if self.mode == Mode::Voice {
            let mut lines = wrapped_lines(voice_preview);
            lines.truncate(ROWS - 1);
            lines.push(format!("{} | Y: sessions", self.audience_label));
            return lines.join("\n");
        }
        let voice = short(voice_status);
        if self.mode == Mode::Sessions {
            let mut lines = vec![
                format!(
                    "{} | SESSIONS {}/{}",
                    self.audience_label,
                    (self.selected + 1).min(self.sessions.len()),
                    self.sessions.len()
                ),
                "Stick up/down: choose. A: open.".into(),
                String::new(),
            ];
            if let Some(session) = self.sessions.get(self.selected) {
                lines.extend(wrapped_lines(&session.title).into_iter().take(2));
                lines.push(short(&format!(
                    "Project: {}",
                    session
                        .directory
                        .rsplit('/')
                        .next()
                        .unwrap_or(&session.directory)
                )));
                lines.push(
                    if session.running {
                        "Agent running"
                    } else {
                        "Agent idle"
                    }
                    .into(),
                );
            }
            if let Some(error) = self.action_error.as_ref().or(self.error.as_ref()) {
                lines.extend(wrapped_lines(error).into_iter().take(2));
                lines.push("Start scripts/opencode-bridge.sh on PC".into());
            } else if self.sessions.is_empty() {
                lines.push(self.notice.clone());
            }
            lines.truncate(ROWS - 2);
            lines.push(voice);
            lines.push("A open | B voice demo | Y refresh".into());
            return lines.join("\n");
        }
        let Some(snapshot) = &self.snapshot else {
            return "OPENCODE\nLoading session...\n\nB: sessions".into();
        };
        let (label, body, footer) = if let Some(approval) = &snapshot.approval {
            (
                format!("TOOL APPROVAL: {}", approval.title),
                approval.text.clone(),
                "A allow once | B deny | stick page",
            )
        } else if let Some(form) = &snapshot.form {
            (
                format!("QUESTION: {}", form.title),
                form.text.clone(),
                "X answer | B sessions | stick page",
            )
        } else if let Some(message) = snapshot.messages.get(self.message) {
            (
                format!(
                    "{} {}/{}",
                    message.role.to_uppercase(),
                    self.message + 1,
                    snapshot.messages.len()
                ),
                message.text.clone(),
                "X talk | B back | R-stick replay",
            )
        } else {
            (
                "No messages yet".into(),
                "Hold X to speak to this existing session.".into(),
                "X send | B sessions | Y refresh",
            )
        };
        let body = if let Some(error) = self.action_error.as_ref().or(self.error.as_ref()) {
            format!("ERROR: {error}\n\n{body}")
        } else {
            body
        };
        let pages = pages(&body);
        self.page = self.page.min(pages.len().saturating_sub(1));
        let header = short(&format!(
            "{}{}",
            if snapshot.running { "* " } else { "" },
            snapshot.title
        ));
        let status = if let Some(error) = self.action_error.as_ref().or(self.error.as_ref()) {
            short(&format!("ERROR: {error}"))
        } else if let Some(notice) = &self.tts_notice {
            short(&format!("VOICE: {notice}"))
        } else if voice_status.starts_with("Ready") {
            short(&self.notice)
        } else {
            voice
        };
        let mut lines = vec![
            short(&format!(
                "{} {} {header}",
                self.audience_label,
                if self.tts_enabled {
                    "[SOUND]"
                } else {
                    "[MUTED]"
                }
            )),
            status,
            short(&format!("p{}/{} {label}", self.page + 1, pages.len())),
        ];
        lines.extend(pages[self.page].clone());
        while lines.len() < ROWS - 1 {
            lines.push(String::new());
        }
        lines.push(footer.into());
        lines.join("\n")
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn short(text: &str) -> String {
    let ascii = wrapped_lines(&text.replace('\n', " ")).join(" ");
    if ascii.len() <= COLUMNS {
        ascii
    } else {
        format!("{}...", &ascii[..COLUMNS - 3])
    }
}
fn shift(index: usize, delta: i32, count: usize) -> usize {
    index
        .saturating_add_signed(delta as isize)
        .min(count.saturating_sub(1))
}
fn pages(text: &str) -> Vec<Vec<String>> {
    let lines = wrapped_lines(text);
    if lines.is_empty() {
        vec![vec![String::new()]]
    } else {
        lines.chunks(BODY_ROWS).map(|page| page.to_vec()).collect()
    }
}
fn message_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "msg_vr_{:x}_{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn response(result: std::result::Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(_, response)) => {
            let error: Value = response.into_json().unwrap_or_default();
            anyhow::bail!(
                "{}",
                error["error"].as_str().unwrap_or("Bridge request failed")
            );
        }
        Err(error) => Err(error).context("USB bridge unavailable; run scripts/opencode-bridge.sh"),
    }
}
fn get<T: DeserializeOwned>(agent: &ureq::Agent, path: &str) -> Result<T> {
    Ok(response(agent.get(&format!("{BRIDGE}{path}")).call())?.into_json()?)
}
fn post(agent: &ureq::Agent, path: &str, body: Value) -> Result<String> {
    let result: Value =
        response(agent.post(&format!("{BRIDGE}{path}")).send_json(body))?.into_json()?;
    Ok(result["note"].as_str().unwrap_or("Done.").to_owned())
}

fn network(commands: Receiver<Command>, events: mpsc::Sender<Event>, shutdown: Arc<AtomicBool>) {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(1))
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .build();
    let mut watched: Option<String> = None;
    let mut watch_generation = 0;
    let mut next = Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(Command::Watch(session, generation)) => {
                watched = session;
                watch_generation = generation;
                next = Instant::now();
            }
            Ok(Command::Refresh) => next = Instant::now(),
            Ok(Command::Send(target, id, text)) => {
                let action = id.clone();
                let result = match target {
                    Target::Message(session) => post(
                        &agent,
                        &format!("/session/{session}/message"),
                        json!({ "id": id, "text": text }),
                    ),
                    Target::Form { session, form, key } => post(
                        &agent,
                        &format!("/session/{session}/form/{form}"),
                        json!({ "key": key, "text": text }),
                    ),
                    Target::Blocked(note) => Err(anyhow::anyhow!(note)),
                };
                let event = match result {
                    Ok(notice) => Event::ActionDone {
                        action,
                        completed_at: Instant::now(),
                        notice,
                    },
                    Err(error) => Event::ActionError {
                        action,
                        error: format!(
                            "{error}. Send not confirmed: check the session before retrying."
                        ),
                    },
                };
                let _ = events.send(event);
                next = Instant::now();
            }
            Ok(Command::Approve {
                action,
                session,
                request,
                decision,
            }) => {
                let event = match post(
                    &agent,
                    &format!("/session/{session}/permission/{request}"),
                    json!({ "decision": decision }),
                ) {
                    Ok(notice) => Event::ActionDone {
                        action,
                        completed_at: Instant::now(),
                        notice,
                    },
                    Err(error) => Event::ActionError {
                        action,
                        error: error.to_string(),
                    },
                };
                let _ = events.send(event);
                next = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if Instant::now() >= next {
            let event = if let Some(session) = &watched {
                get::<Snapshot>(&agent, &format!("/session/{session}")).map(|snapshot| {
                    Event::Snapshot(Box::new(snapshot), Instant::now(), watch_generation)
                })
            } else {
                get::<Vec<Session>>(&agent, "/sessions").map(Event::Sessions)
            };
            let _ = events.send(event.unwrap_or_else(|error| Event::Error {
                session: watched.clone(),
                error: error.to_string(),
                watch_generation,
            }));
            next = Instant::now()
                + if watched.is_some() {
                    Duration::from_millis(700)
                } else {
                    Duration::from_secs(4)
                };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::SessionActivity;

    fn snapshot_event(snapshot: Snapshot) -> Event {
        snapshot_event_for(snapshot, 0)
    }

    fn snapshot_event_for(snapshot: Snapshot, generation: u64) -> Event {
        Event::Snapshot(Box::new(snapshot), Instant::now(), generation)
    }

    fn unrelated_notice(notice: &str) -> Event {
        Event::ActionDone {
            action: "unrelated-action".into(),
            completed_at: Instant::now(),
            notice: notice.into(),
        }
    }

    fn fake() -> (Chat, Receiver<Command>, mpsc::Sender<Event>) {
        let (commands, receiver) = mpsc::sync_channel(16);
        let (sender, events) = mpsc::channel();
        let chat = Chat {
            commands,
            events,
            shutdown: Arc::new(AtomicBool::new(false)),
            worker: None,
            sessions: vec![
                Session {
                    id: "ses_alpha".into(),
                    title: "Alpha".into(),
                    ..Default::default()
                },
                Session {
                    id: "ses_beta".into(),
                    title: "Beta".into(),
                    ..Default::default()
                },
            ],
            selected: 0,
            open: Some("ses_alpha".into()),
            watch_generation: 0,
            snapshot: Some(Snapshot {
                id: "ses_alpha".into(),
                title: "Alpha".into(),
                ..Default::default()
            }),
            snapshot_at: Some(Instant::now()),
            snapshot_failed: false,
            stale_workspace: None,
            pending_actions: Vec::new(),
            mode: Mode::Chat,
            message: 0,
            page: 0,
            follow: true,
            pending_id: String::new(),
            target: None,
            notice: String::new(),
            error: None,
            action_error: None,
            speech_tracker: Default::default(),
            speech_ready: Vec::new(),
            tts_enabled: true,
            tts_notice: None,
            audience_label: "MIC OFF",
        };
        (chat, receiver, sender)
    }

    fn projected_snapshot() -> Snapshot {
        serde_json::from_value(json!({
            "id": "ses_alpha", "title": "Alpha", "messages": [],
            "delegation": {
                "nodes": [
                    {"id": "ses_worker", "parent_id": "ses_alpha", "title": "Worker", "state": "completed"}
                ],
                "presentations": [{
                    "id": "card_worker", "source_id": "ses_worker", "source_message_id": "msg_worker",
                    "title": "Worker result", "kind": "markdown", "text": "Worker-only reply",
                    "columns": [], "rows": []
                }],
                "error": null, "truncated": false
            }
        })).unwrap()
    }

    #[test]
    fn delegation_roundtrip_and_malformed_projection_do_not_discard_chat() {
        let snapshot = projected_snapshot();
        assert!(snapshot.delegation.valid());
        assert!(!snapshot.delegation.stale); // Wire compatibility with older bridges.
        let value = serde_json::to_value(&snapshot.delegation).unwrap();
        let decoded: crate::delegation::Workspace = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);

        for malformed in [
            json!({}),
            json!({"nodes": [], "presentations": [], "error": null}),
            json!({"nodes": [], "presentations": [], "error": null, "truncated": false, "stale": "bad"}),
            json!({"nodes": "not an array"}),
            json!({"nodes": [{"id": "ses_worker"}], "presentations": [], "error": null, "truncated": false}),
            json!({"nodes": [], "presentations": [{"id": "card"}], "error": null, "truncated": false}),
            json!({"nodes": [{"id": "bad", "parent_id": "ses_alpha", "title": "Bad", "state": "new-unknown-state"}], "presentations": [], "error": null, "truncated": false}),
            json!({"nodes": [], "presentations": [{"id": "bad", "source_id": "ses_alpha", "source_message_id": "msg", "title": "Bad", "kind": "unknown-kind", "text": "Bad", "columns": [], "rows": []}], "error": null, "truncated": false}),
        ] {
            let snapshot: Snapshot = serde_json::from_value(json!({
                "id": "ses_alpha", "title": "Alpha", "running": true,
                "messages": [{"id": "msg", "role": "assistant", "text": "Keep this reply"}],
                "approval": {"id": "perm", "title": "shell", "text": "cargo test"},
                "delegation": malformed
            }))
            .unwrap();
            assert_eq!(snapshot.messages[0].text, "Keep this reply");
            assert!(snapshot.running);
            assert_eq!(snapshot.approval.as_ref().unwrap().id, "perm");
            assert!(snapshot.delegation.nodes.is_empty());
            assert!(snapshot.delegation.presentations.is_empty());
            assert!(snapshot.delegation.error.is_some());
            assert!(snapshot.delegation.stale);
        }
    }

    #[test]
    fn missing_or_null_projection_is_unknown_but_chat_deserializes() {
        for projection in [None, Some(Value::Null)] {
            let mut value = json!({
                "id": "ses_alpha", "title": "Alpha",
                "messages": [{"id": "msg", "role": "assistant", "text": "Still here"}],
                "approval": {"id": "perm", "title": "shell", "text": "cargo test"}
            });
            if let Some(projection) = projection {
                value["delegation"] = projection;
            }
            let snapshot: Snapshot = serde_json::from_value(value).unwrap();
            assert!(snapshot.delegation.error.is_some());
            assert!(snapshot.delegation.stale);
            assert_eq!(snapshot.messages[0].text, "Still here");
            assert_eq!(snapshot.approval.unwrap().id, "perm");
        }
    }

    #[test]
    fn invalid_projection_is_cleared_at_receipt_without_losing_messages_or_permissions() {
        let (mut chat, _, events) = fake();
        for oversized in [false, true] {
            let mut snapshot = projected_snapshot();
            if oversized {
                snapshot.delegation.nodes[0].title = "x".repeat(100_000);
            } else {
                snapshot.delegation.nodes[0].parent_id = Some("ses_worker".into());
            }
            assert!(!snapshot.delegation.valid());
            snapshot.messages.push(Message {
                id: "msg".into(),
                text: "Still readable".into(),
                ..Default::default()
            });
            snapshot.approval = Some(Approval {
                id: "perm".into(),
                ..Default::default()
            });
            events.send(snapshot_event(snapshot)).unwrap();
            chat.poll();
            let (_, workspace) = chat.workspace().unwrap();
            assert!(workspace.nodes.is_empty());
            assert!(workspace.presentations.is_empty());
            assert!(workspace.error.is_some());
            assert!(workspace.stale);
            assert_eq!(
                chat.snapshot.as_ref().unwrap().messages[0].text,
                "Still readable"
            );
            assert_eq!(chat.activity(), SessionActivity::Waiting);
        }
    }

    #[test]
    fn workspace_is_hidden_immediately_on_back_switch_refresh_and_voice_mode() {
        let (mut chat, _commands, events) = fake();
        let alpha = projected_snapshot();
        events.send(snapshot_event(alpha.clone())).unwrap();
        chat.poll();
        assert_eq!(chat.workspace().unwrap().0, "ses_alpha");
        chat.back();
        assert!(chat.workspace().is_none());
        chat.navigate(0, 1);
        chat.accept();
        assert!(chat.workspace().is_none());
        events.send(snapshot_event(alpha.clone())).unwrap();
        chat.poll();
        assert!(chat.workspace().is_none());
        let mut beta = alpha;
        beta.id = "ses_beta".into();
        beta.delegation = Default::default();
        events
            .send(snapshot_event_for(beta, chat.watch_generation))
            .unwrap();
        chat.poll();
        assert_eq!(chat.workspace().unwrap().0, "ses_beta");
        assert!(chat.workspace().unwrap().1.nodes.is_empty());
        chat.refresh();
        assert!(chat.workspace().is_none());
        chat.back();
        assert!(chat.mode == Mode::Voice);
        assert!(chat.workspace().is_none());
        chat.mode = Mode::Chat;
        chat.open = Some("ses_unrelated".into());
        assert!(chat.workspace().is_none());
    }

    #[test]
    fn reopening_root_ignores_late_snapshot_and_error_from_prior_watch() {
        let (mut chat, _commands, events) = fake();
        let alpha = projected_snapshot();
        events.send(snapshot_event(alpha.clone())).unwrap();
        chat.poll();
        chat.back();
        chat.navigate(0, 1);
        chat.accept();
        chat.back();
        chat.navigate(0, -1);
        chat.accept();
        let current = chat.watch_generation;
        assert!(chat.workspace().is_none());
        events.send(snapshot_event(alpha.clone())).unwrap();
        events
            .send(Event::Error {
                session: Some("ses_alpha".into()),
                error: "old error".into(),
                watch_generation: 0,
            })
            .unwrap();
        chat.poll();
        assert!(chat.workspace().is_none());
        assert_eq!(chat.activity(), SessionActivity::Unknown);
        events.send(snapshot_event_for(alpha, current)).unwrap();
        chat.poll();
        events
            .send(Event::Error {
                session: Some("ses_alpha".into()),
                error: "old error".into(),
                watch_generation: 0,
            })
            .unwrap();
        chat.poll();
        assert!(chat.workspace().unwrap().1.error.is_none());
        assert_eq!(chat.activity(), SessionActivity::Ready);
    }

    #[test]
    fn malformed_missing_or_error_projection_makes_existing_workers_unknown() {
        let (mut chat, _, events) = fake();
        let mut scene = crate::delegation::Scene::new();
        events.send(snapshot_event(projected_snapshot())).unwrap();
        chat.poll();
        let (root, workspace) = chat.workspace().unwrap();
        scene.update(Some(root), workspace, 0.05);
        assert_eq!(
            scene.stations()[0].state,
            crate::delegation::State::Completed
        );
        for projection in [
            None,
            Some(json!({"nodes": "bad"})),
            Some(json!({"nodes": [], "presentations": [], "error": "offline", "truncated": false})),
        ] {
            let mut value = json!({"id": "ses_alpha", "title": "Alpha", "messages": []});
            if let Some(projection) = projection {
                value["delegation"] = projection;
            }
            events
                .send(snapshot_event(serde_json::from_value(value).unwrap()))
                .unwrap();
            chat.poll();
            let (root, workspace) = chat.workspace().unwrap();
            assert!(workspace.error.is_some());
            scene.update(Some(root), workspace, 0.05);
            assert_eq!(scene.stations()[0].state, crate::delegation::State::Unknown);
        }
    }

    #[test]
    fn fresh_partial_status_survives_notice_but_snapshot_expiry_does_not() {
        use crate::delegation::{Node, Scene, State};
        let (mut chat, _, events) = fake();
        let mut scene = Scene::new();
        let mut snapshot = projected_snapshot();
        snapshot.delegation.nodes.push(Node {
            id: "ses_omitted".into(),
            parent_id: Some("ses_alpha".into()),
            title: "Omitted".into(),
            state: State::Running,
        });
        events.send(snapshot_event(snapshot.clone())).unwrap();
        chat.poll();
        let (root, workspace) = chat.workspace().unwrap();
        scene.update(Some(root), workspace, 0.05);

        snapshot.delegation.nodes.pop();
        snapshot.delegation.error = Some("Presentation store unavailable".into());
        let fetched_at = Instant::now();
        events
            .send(Event::Snapshot(Box::new(snapshot), fetched_at, 0))
            .unwrap();
        chat.poll();
        let (root, workspace) = chat.workspace_at(fetched_at).unwrap();
        assert!(!workspace.stale);
        scene.update(Some(root), workspace, 0.05);
        assert_eq!(scene.stations().len(), 2);
        assert_eq!(scene.stations()[0].state, State::Unknown);
        assert_eq!(scene.stations()[1].state, State::Completed);

        let (root, workspace) = chat
            .workspace_at(fetched_at + SNAPSHOT_MAX_AGE + Duration::from_millis(1))
            .unwrap();
        assert!(workspace.stale);
        scene.update(Some(root), workspace, 0.05);
        assert!(
            scene
                .stations()
                .iter()
                .all(|station| station.state == State::Unknown)
        );
        assert_eq!(scene.stations().len(), 2);
    }

    #[test]
    fn stale_projection_preserves_data_and_fresh_snapshot_clears_warning() {
        let (mut chat, _, events) = fake();
        let snapshot = projected_snapshot();
        let fetched_at = Instant::now();
        events
            .send(Event::Snapshot(Box::new(snapshot.clone()), fetched_at, 0))
            .unwrap();
        chat.poll();
        assert!(
            chat.workspace_at(fetched_at + SNAPSHOT_MAX_AGE)
                .unwrap()
                .1
                .error
                .is_none()
        );
        let (_, workspace) = chat
            .workspace_at(fetched_at + SNAPSHOT_MAX_AGE + Duration::from_millis(1))
            .unwrap();
        assert!(workspace.error.as_ref().unwrap().contains("stale"));
        assert!(workspace.stale);
        assert_eq!(workspace.nodes.len(), 1);
        assert_eq!(workspace.presentations[0].text, "Worker-only reply");
        // Projection warnings do not alter core activity or speech state.
        assert_eq!(chat.activity_at(fetched_at), SessionActivity::Ready);
        assert!(!chat.has_pending_speech());
        events.send(snapshot_event(snapshot.clone())).unwrap();
        chat.poll();
        assert!(chat.workspace().unwrap().1.error.is_none());
        assert!(!chat.workspace().unwrap().1.stale);
        // A delayed event must use fetch completion, not event delivery, for freshness.
        events
            .send(Event::Snapshot(
                Box::new(snapshot.clone()),
                Instant::now() - SNAPSHOT_MAX_AGE - Duration::from_millis(1),
                0,
            ))
            .unwrap();
        chat.poll();
        assert!(
            chat.workspace()
                .unwrap()
                .1
                .error
                .as_ref()
                .unwrap()
                .contains("stale")
        );
        events.send(snapshot_event(snapshot)).unwrap();
        chat.poll();
        assert!(chat.workspace().unwrap().1.error.is_none());
    }

    #[test]
    fn watched_errors_preserve_projection_until_fresh_fetch_and_ignore_other_roots() {
        let (mut chat, _, events) = fake();
        let snapshot = projected_snapshot();
        events.send(snapshot_event(snapshot.clone())).unwrap();
        events
            .send(Event::Error {
                session: Some("ses_other".into()),
                error: "old fetch failed".into(),
                watch_generation: 0,
            })
            .unwrap();
        chat.poll();
        assert!(chat.workspace().unwrap().1.error.is_none());
        assert_eq!(chat.activity(), SessionActivity::Ready);
        events
            .send(Event::Error {
                session: Some("ses_alpha".into()),
                error: "offline".into(),
                watch_generation: 0,
            })
            .unwrap();
        events.send(unrelated_notice("Unrelated success")).unwrap();
        events.send(Event::Sessions(Vec::new())).unwrap();
        chat.poll();
        let (_, workspace) = chat.workspace().unwrap();
        assert!(workspace.error.as_ref().unwrap().contains("fetch failed"));
        assert!(workspace.stale);
        assert_eq!(workspace.nodes.len(), 1);
        assert_eq!(workspace.presentations.len(), 1);
        assert_eq!(chat.activity(), SessionActivity::Unavailable);
        events.send(snapshot_event(snapshot)).unwrap();
        chat.poll();
        assert!(chat.workspace().unwrap().1.error.is_none());
        assert_eq!(chat.activity(), SessionActivity::Ready);
    }

    #[test]
    fn worker_presentations_never_enter_coordinator_speech_or_replay() {
        let (mut chat, _, events) = fake();
        let mut snapshot = projected_snapshot();
        events.send(snapshot_event(snapshot.clone())).unwrap();
        chat.poll();
        snapshot.delegation.presentations[0].text = "Another worker-only reply".into();
        events.send(snapshot_event(snapshot.clone())).unwrap();
        chat.poll();
        assert!(!chat.has_pending_speech());
        assert!(chat.replay_speech().is_none());
        snapshot.messages.push(Message {
            id: "msg_coordinator".into(),
            role: "assistant".into(),
            text: "Coordinator reply".into(),
            speech_text: "Coordinator reply".into(),
            completed: true,
        });
        events.send(snapshot_event(snapshot)).unwrap();
        chat.poll();
        assert_eq!(chat.take_speech(true, false, true), ["Coordinator reply"]);
        assert_eq!(chat.replay_speech().as_deref(), Some("Coordinator reply"));
    }

    #[test]
    fn activity_distinguishes_selected_fresh_running_waiting_and_missing_state() {
        let (mut chat, _, _) = fake();
        assert_eq!(chat.activity(), SessionActivity::Ready);
        chat.snapshot.as_mut().unwrap().running = true;
        assert_eq!(chat.activity(), SessionActivity::Working);
        chat.snapshot.as_mut().unwrap().approval = Some(Approval::default());
        assert_eq!(chat.activity(), SessionActivity::Waiting);
        chat.snapshot.as_mut().unwrap().approval = None;
        chat.snapshot.as_mut().unwrap().form = Some(Form::default());
        assert_eq!(chat.activity(), SessionActivity::Waiting);
        let fetched_at = chat.snapshot_at.unwrap();
        assert_eq!(
            chat.activity_at(fetched_at + SNAPSHOT_MAX_AGE),
            SessionActivity::Waiting
        );
        chat.snapshot_at = Some(Instant::now() - SNAPSHOT_MAX_AGE - Duration::from_millis(1));
        assert_eq!(chat.activity(), SessionActivity::Unknown);
        chat.action_error = Some("failed".into());
        assert_eq!(chat.activity(), SessionActivity::Unavailable);
        chat.mode = Mode::Sessions;
        assert_eq!(chat.activity(), SessionActivity::Unselected);
        chat.mode = Mode::Voice;
        assert_eq!(chat.activity(), SessionActivity::Unselected);
        chat.mode = Mode::Chat;
        chat.action_error = None;
        chat.snapshot = None;
        assert_eq!(chat.activity(), SessionActivity::Unknown);
        chat.snapshot_at = Some(Instant::now());
        assert_eq!(chat.activity(), SessionActivity::Unknown);
    }

    #[test]
    fn snapshot_freshness_uses_fetch_time_not_event_delivery_or_notices() {
        let (mut chat, _, events) = fake();
        let fetched_at = Instant::now() - SNAPSHOT_MAX_AGE - Duration::from_millis(1);
        events
            .send(Event::Snapshot(
                Box::new(chat.snapshot.clone().unwrap()),
                fetched_at,
                0,
            ))
            .unwrap();
        events.send(Event::Sessions(Vec::new())).unwrap();
        events.send(unrelated_notice("Connected")).unwrap();
        chat.poll();
        assert_eq!(chat.snapshot_at, Some(fetched_at));
        assert_eq!(chat.activity(), SessionActivity::Unknown);
        events
            .send(snapshot_event(Snapshot {
                id: "ses_other".into(),
                ..Default::default()
            }))
            .unwrap();
        chat.poll();
        assert_eq!(chat.snapshot_at, Some(fetched_at));
        assert_eq!(chat.activity(), SessionActivity::Unknown);
        events
            .send(snapshot_event(chat.snapshot.clone().unwrap()))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Ready);
    }

    #[test]
    fn watched_fetch_failure_requires_a_watched_snapshot_to_recover() {
        let (mut chat, _, events) = fake();
        events
            .send(Event::Error {
                session: Some("ses_alpha".into()),
                error: "offline".into(),
                watch_generation: 0,
            })
            .unwrap();
        events.send(Event::Sessions(Vec::new())).unwrap();
        events.send(unrelated_notice("not a fetch")).unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Unavailable);
        events
            .send(snapshot_event(chat.snapshot.clone().unwrap()))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Ready);
    }

    #[test]
    fn send_waits_for_its_ack_and_a_post_settling_snapshot_even_if_already_idle() {
        let (mut chat, commands, events) = fake();
        chat.begin_utterance();
        chat.transcript("Hello".into());
        let Command::Send(_, action, _) = commands.try_recv().unwrap() else {
            panic!("missing send")
        };
        assert_eq!(chat.activity(), SessionActivity::Working);
        // Neither an idle snapshot nor an unrelated notice acknowledges a send.
        events
            .send(snapshot_event(chat.snapshot.clone().unwrap()))
            .unwrap();
        events
            .send(unrelated_notice("not an acknowledgment"))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Working);
        assert_eq!(
            chat.activity_at(
                chat.snapshot_at.unwrap() + SNAPSHOT_MAX_AGE + Duration::from_millis(1)
            ),
            SessionActivity::Unknown
        );
        let completed_at = Instant::now();
        events
            .send(Event::ActionDone {
                action: "other".into(),
                completed_at,
                notice: "Other action".into(),
            })
            .unwrap();
        events
            .send(Event::Snapshot(
                Box::new(chat.snapshot.clone().unwrap()),
                completed_at + ACTION_SETTLE,
                0,
            ))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Working);
        events
            .send(Event::ActionDone {
                action,
                completed_at,
                notice: "Sent".into(),
            })
            .unwrap();
        events
            .send(Event::Snapshot(
                Box::new(chat.snapshot.clone().unwrap()),
                completed_at,
                0,
            ))
            .unwrap();
        chat.poll();
        assert_eq!(
            chat.activity_at(completed_at + Duration::from_secs(1)),
            SessionActivity::Working
        );
        // Elapsed time alone cannot bless the pre-action/early idle snapshot.
        assert_eq!(
            chat.activity_at(completed_at + ACTION_SETTLE),
            SessionActivity::Working
        );
        let settled = completed_at + ACTION_SETTLE;
        events
            .send(Event::Snapshot(
                Box::new(chat.snapshot.clone().unwrap()),
                settled,
                0,
            ))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity_at(settled), SessionActivity::Ready);
        assert!(chat.pending_actions.is_empty());
    }

    #[test]
    fn approval_guard_returns_to_waiting_or_running_after_bounded_settling() {
        let (mut chat, commands, events) = fake();
        chat.snapshot.as_mut().unwrap().approval = Some(Approval {
            id: "permission".into(),
            ..Default::default()
        });
        assert_eq!(chat.activity(), SessionActivity::Waiting);
        chat.accept();
        let Command::Approve { action, .. } = commands.try_recv().unwrap() else {
            panic!("missing approval")
        };
        assert_eq!(chat.activity(), SessionActivity::Working);
        let completed_at = Instant::now();
        events
            .send(Event::ActionDone {
                action,
                completed_at,
                notice: "Approved".into(),
            })
            .unwrap();
        events
            .send(Event::Snapshot(
                Box::new(chat.snapshot.clone().unwrap()),
                completed_at,
                0,
            ))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Working);
        let settled = completed_at + ACTION_SETTLE;
        events
            .send(Event::Snapshot(
                Box::new(chat.snapshot.clone().unwrap()),
                settled,
                0,
            ))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity_at(settled), SessionActivity::Waiting);
        let mut snapshot = chat.snapshot.clone().unwrap();
        snapshot.approval = None;
        snapshot.running = true;
        events
            .send(Event::Snapshot(Box::new(snapshot), settled, 0))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity_at(settled), SessionActivity::Working);
    }

    #[test]
    fn failed_action_is_unavailable_and_does_not_leave_a_permanent_working_guard() {
        let (mut chat, commands, events) = fake();
        chat.begin_utterance();
        chat.transcript("Hello".into());
        let Command::Send(_, action, _) = commands.try_recv().unwrap() else {
            panic!("missing send")
        };
        events
            .send(Event::ActionError {
                action,
                error: "Send failed".into(),
            })
            .unwrap();
        events
            .send(snapshot_event(chat.snapshot.clone().unwrap()))
            .unwrap();
        chat.poll();
        assert!(chat.pending_actions.is_empty());
        assert_eq!(chat.activity(), SessionActivity::Unavailable);
    }

    #[test]
    fn rejected_command_is_unavailable_without_a_pending_action() {
        let (mut chat, commands, _) = fake();
        drop(commands);
        chat.begin_utterance();
        chat.transcript("Hello".into());
        assert_eq!(chat.activity(), SessionActivity::Unavailable);
        assert!(chat.pending_actions.is_empty());
    }

    #[test]
    fn pending_action_survives_leaving_and_reopening_its_session() {
        let (mut chat, commands, events) = fake();
        chat.begin_utterance();
        chat.transcript("Hello".into());
        let Command::Send(_, action, _) = commands.try_recv().unwrap() else {
            panic!("missing send")
        };
        chat.back();
        assert_eq!(chat.activity(), SessionActivity::Unselected);
        chat.accept();
        assert_eq!(chat.activity(), SessionActivity::Unknown);
        events
            .send(snapshot_event_for(
                Snapshot {
                    id: "ses_alpha".into(),
                    ..Default::default()
                },
                chat.watch_generation,
            ))
            .unwrap();
        chat.poll();
        assert_eq!(chat.activity(), SessionActivity::Working);
        assert_eq!(chat.pending_actions[0].id, action);
    }

    #[test]
    fn an_action_bound_to_another_session_does_not_mark_the_selected_one_working() {
        let (mut chat, _commands, events) = fake();
        chat.begin_utterance();
        chat.back();
        chat.navigate(0, 1);
        chat.accept();
        events
            .send(snapshot_event_for(
                Snapshot {
                    id: "ses_beta".into(),
                    ..Default::default()
                },
                chat.watch_generation,
            ))
            .unwrap();
        chat.poll();
        chat.transcript("Bound to Alpha".into());
        assert_eq!(chat.activity(), SessionActivity::Ready);
        assert_eq!(chat.pending_actions.len(), 1);
        assert_eq!(chat.pending_actions[0].session, "ses_alpha");
    }

    #[test]
    fn speech_is_bound_to_the_session_selected_when_recording_started() {
        let (mut chat, commands, events) = fake();
        chat.begin_utterance();
        chat.back();
        chat.navigate(0, 1);
        assert!(!chat.accept());
        events
            .send(snapshot_event_for(
                Snapshot {
                    id: "ses_beta".into(),
                    title: "Beta".into(),
                    ..Default::default()
                },
                chat.watch_generation,
            ))
            .unwrap();
        events
            .send(snapshot_event(Snapshot {
                id: "ses_alpha".into(),
                ..Default::default()
            }))
            .unwrap();
        chat.poll();
        assert_eq!(chat.snapshot.as_ref().unwrap().id, "ses_beta");
        chat.transcript("Hello original session".into());
        let mut sent = 0;
        for command in commands.try_iter() {
            if let Command::Send(target, id, text) = command {
                assert_eq!(target, Target::Message("ses_alpha".into()));
                assert!(id.starts_with("msg_vr_"));
                assert_eq!(text, "Hello original session");
                sent += 1;
            }
        }
        assert_eq!(sent, 1);
    }

    #[test]
    fn failed_sends_remain_visible_after_successful_background_refreshes() {
        let (mut chat, _, events) = fake();
        events
            .send(Event::ActionError {
                action: "failed".into(),
                error: "Send not confirmed".into(),
            })
            .unwrap();
        events
            .send(snapshot_event(chat.snapshot.clone().unwrap()))
            .unwrap();
        chat.poll();
        assert!(chat.text("Ready", "", None).contains("Send not confirmed"));
        events.send(unrelated_notice("Sent")).unwrap();
        chat.poll();
        assert!(!chat.text("Ready", "", None).contains("Send not confirmed"));
    }

    #[test]
    fn pending_tool_permissions_require_explicit_buttons_not_speech() {
        let (mut chat, commands, _) = fake();
        chat.snapshot.as_mut().unwrap().approval = Some(Approval {
            id: "perm_1".into(),
            title: "shell".into(),
            text: "cargo test".into(),
        });
        chat.begin_utterance();
        chat.transcript("yes".into());
        assert!(commands.try_recv().is_err());
        assert!(!chat.accept());
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Approve {
                decision: "once",
                ..
            }
        ));
        chat.back();
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Approve {
                decision: "reject",
                ..
            }
        ));
    }

    #[test]
    fn session_browser_and_chat_fit_the_caption_grid() {
        let (mut chat, _, _) = fake();
        chat.snapshot.as_mut().unwrap().messages.push(Message {
            id: "msg_1".into(),
            role: "assistant".into(),
            text: "Very long reply ".repeat(1000),
            ..Default::default()
        });
        for mode in [Mode::Chat, Mode::Sessions] {
            chat.mode = mode;
            let text = chat.text("Ready. Hold X to talk; mic is off.", "", None);
            assert!(text.lines().count() <= ROWS);
            assert!(text.lines().all(|line| line.len() <= COLUMNS));
        }
        chat.refresh();
        assert!(chat.mode == Mode::Sessions);
    }

    #[test]
    fn live_draft_shows_bound_destination_and_latest_words_without_truncating_data() {
        let (mut chat, _, _) = fake();
        chat.begin_utterance();
        let draft = format!("{} final words", "old ".repeat(300));
        let panel = chat.text("RECORDING", "", Some(&draft));
        assert!(panel.contains("To: Alpha"));
        assert!(panel.contains("final words"));
        assert!(panel.lines().count() <= ROWS);
        assert!(matches!(chat.target, Some(Target::Message(ref id)) if id == "ses_alpha"));
    }

    #[test]
    fn replay_uses_only_assistant_text_not_user_or_tool_messages() {
        let (mut chat, _, _) = fake();
        let snapshot = chat.snapshot.as_mut().unwrap();
        snapshot.messages = vec![
            Message {
                id: "msg_1".into(),
                role: "assistant".into(),
                speech_text: "**Hello**\n```\nsecret\n```".into(),
                completed: true,
                ..Default::default()
            },
            Message {
                id: "msg_2".into(),
                role: "user".into(),
                text: "User text".into(),
                ..Default::default()
            },
        ];
        chat.message = 1;
        assert_eq!(chat.replay_speech().as_deref(), Some("Hello"));
        chat.mode = Mode::Sessions;
        assert!(chat.replay_speech().is_none());
    }

    #[test]
    fn speech_completed_during_recording_waits_for_release() {
        let (mut chat, _, events) = fake();
        let snapshot = chat.snapshot.clone().unwrap();
        events.send(snapshot_event(snapshot.clone())).unwrap();
        chat.poll(); // establish the history baseline
        let mut next = snapshot;
        next.messages.push(Message {
            id: "msg_new".into(),
            role: "assistant".into(),
            text: "A reply".into(),
            speech_text: "A reply".into(),
            completed: true,
        });
        events.send(snapshot_event(next)).unwrap();
        chat.poll();
        assert!(chat.has_pending_speech());
        assert!(chat.take_speech(true, true, true).is_empty());
        assert!(chat.has_pending_speech());
        assert!(chat.take_speech(true, true, true).is_empty());
        assert_eq!(chat.take_speech(true, false, true), ["A reply"]);
        assert!(!chat.has_pending_speech());
        assert!(chat.take_speech(true, false, true).is_empty());
    }

    #[test]
    fn pending_recording_speech_is_discarded_on_mute_or_focus_loss() {
        let (mut chat, _, _) = fake();
        chat.speech_ready.push("muted reply".into());
        assert!(chat.take_speech(true, true, false).is_empty());
        assert!(chat.take_speech(true, false, true).is_empty());
        chat.speech_ready.push("unfocused reply".into());
        assert!(chat.take_speech(false, true, true).is_empty());
        assert!(chat.take_speech(true, false, true).is_empty());
        chat.speech_ready.push("old session".into());
        chat.mode = Mode::Sessions;
        assert!(chat.take_speech(true, false, true).is_empty());
    }

    #[test]
    fn pages_keep_all_reply_text_and_fit_the_panel() {
        let text = (0..200).map(|i| format!("line {i}\n")).collect::<String>();
        let paged = pages(&text);
        assert!(paged.len() > 1);
        assert!(
            paged.iter().all(
                |page| page.len() <= BODY_ROWS && page.iter().all(|line| line.len() <= COLUMNS)
            )
        );
        assert!(paged.last().unwrap().last().unwrap().contains("199"));
        assert!(short(&"🦀".repeat(200)).len() <= COLUMNS);
    }

    #[test]
    fn bridge_snapshot_accepts_live_session_shape() {
        let snapshot: Snapshot = serde_json::from_str(r#"{"id":"ses_existing","title":"My work","running":true,"messages":[{"id":"msg_1","role":"assistant","text":"Hello VR"}],"approval":null,"form":null}"#).unwrap();
        assert_eq!(snapshot.messages[0].text, "Hello VR");
        assert!(snapshot.running);
        assert!(snapshot.delegation.nodes.is_empty());
        assert!(snapshot.delegation.presentations.is_empty());
        assert!(snapshot.delegation.error.is_some());
        assert_eq!(shift(0, -1, 3), 0);
        assert_eq!(shift(2, 1, 3), 2);
        assert_ne!(message_id(), message_id());
    }
}
