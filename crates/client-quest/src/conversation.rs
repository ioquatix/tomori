// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The panel's conversation with a Tomori server: choosing a server, the lobby, then the joined space.
//!
//! This is pure state. It applies [`client::Event`]s and controller input, and queues [`Action`]s for [`crate::connection::Connection`] to carry out, so nothing here touches the network.
use crate::{
    caption::{COLUMNS, ROWS, wrapped_lines},
    interaction::SessionActivity,
    server_choice::{self, Choice},
};
use client::{ConnectionState, Event};
use protocol::{
    Activity, ClientMessage, Decision, QueueState, Role, ServerMessage, SpaceState, SpaceSummary,
};

const BODY_ROWS: usize = ROWS - 4;

/// What the conversation needs done with the connection.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Connect to this server, closing any current connection.
    Connect(Choice),
    /// Close the connection.
    Disconnect,
    /// Forget the servers found so far and browse again.
    Rescan,
    Send(ClientMessage),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Servers,
    Lobby,
    Space,
}

pub struct Conversation {
    /// This participant's identity, which the server records on their queued input.
    participant: String,
    name: String,
    mode: Mode,
    servers: Vec<Choice>,
    server: usize,
    /// Whether the stick moved the server selection; until then the best choice, listed first, stays selected as servers appear.
    chosen: bool,
    /// Whether the remembered server may still be connected to when it appears: only once, before the participant moves the selection or connects.
    automatic: bool,
    /// The server chosen in the server list, while in the lobby or a space.
    connected: Option<Choice>,
    state: Option<ConnectionState>,
    spaces: Vec<SpaceSummary>,
    listed: bool,
    selected: usize,
    joining: Option<String>,
    /// Space updates sent before the server handled our leave are ignored until it confirms.
    leaving: bool,
    space: Option<SpaceState>,
    /// The selected message; one past the last is the queue, when it has entries.
    message: usize,
    page: usize,
    follow: bool,
    /// The approval or question being shown, so paging and the chosen option reset when it changes.
    request: Option<String>,
    /// The chosen approval option: 0 allows once, `n` grants the `n`th standing option.
    option: usize,
    /// The approval a decision was sent for, so another press cannot send a second one.
    decided: Option<String>,
    notice: Option<String>,
    error: Option<String>,
    actions: Vec<Action>,
}

impl Conversation {
    /// `participant` is this participant's identity and `name` their display name.
    pub fn new(participant: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            participant: participant.into(),
            name: name.into(),
            mode: Mode::Servers,
            servers: Vec::new(),
            server: 0,
            chosen: false,
            automatic: true,
            connected: None,
            state: None,
            spaces: Vec::new(),
            listed: false,
            selected: 0,
            joining: None,
            leaving: false,
            space: None,
            message: 0,
            page: 0,
            follow: true,
            request: None,
            option: 0,
            decided: None,
            notice: None,
            error: None,
            actions: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the server list is showing, which is the only time it is worth browsing.
    pub fn is_browsing(&self) -> bool {
        self.mode == Mode::Servers
    }

    /// The server chosen in the server list, while connected or connecting to it.
    pub fn server(&self) -> Option<&Choice> {
        self.connected.as_ref()
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    /// Replace the server list, keeping the selection on the same server.
    pub fn set_servers(&mut self, servers: Vec<Choice>) {
        let selected = self
            .servers
            .get(self.server)
            .filter(|_| self.chosen)
            .map(|choice| &choice.address);
        self.server = selected
            .and_then(|address| servers.iter().position(|choice| &choice.address == address))
            .unwrap_or(0);
        self.servers = servers;
        if self.automatic
            && !self.chosen
            && self.mode == Mode::Servers
            && let Some(choice) = server_choice::automatic(&self.servers).cloned()
        {
            self.connect(choice);
        }
    }

    /// Connect to a server without choosing it from the list, as for an address given at startup.
    pub fn connect(&mut self, choice: Choice) {
        log::info!("Connecting to {} at {}", choice.label, choice.address);
        self.automatic = false;
        self.actions.push(Action::Connect(choice.clone()));
        self.connected = Some(choice);
        self.state = None;
        self.spaces.clear();
        self.listed = false;
        self.selected = 0;
        self.close_space();
        self.mode = Mode::Lobby;
        self.error = None;
        self.notice = None;
    }

    /// Report a problem the connection found, such as a client that could not start.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::State(state) => self.connection_changed(state),
            Event::Message(message) => self.receive(message),
            // Nothing records utterances yet.
            Event::UtteranceFailed(_) => {}
        }
    }

    fn connection_changed(&mut self, state: ConnectionState) {
        if let ConnectionState::Connected { server, .. } = &state {
            log::info!("Connected to {server}");
            self.error = None;
            match self.mode {
                // A space joined meanwhile is restored by the client, and the server sends its state.
                Mode::Lobby => self.list(),
                // The client rejoins the space; the server may have lost it while away.
                Mode::Space => self.joining = self.space.as_ref().map(|space| space.id.clone()),
                Mode::Servers => {}
            }
        }
        self.state = Some(state);
    }

    fn receive(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Spaces { spaces } => {
                let selected = self.spaces.get(self.selected).map(|space| &space.id);
                self.selected = selected
                    .and_then(|id| spaces.iter().position(|space| &space.id == id))
                    .unwrap_or(0);
                self.spaces = spaces;
                self.listed = true;
            }
            ServerMessage::Space { state } => self.update_space(*state),
            ServerMessage::Left => {
                let requested = std::mem::take(&mut self.leaving);
                if self.mode != Mode::Servers {
                    self.close_space();
                    self.mode = Mode::Lobby;
                    // Leaving on request is followed by a listing; otherwise ask for one.
                    if !requested {
                        self.list();
                    }
                }
            }
            ServerMessage::Error { error } => {
                log::warn!("Server error: {error}");
                let rejoining = self.mode == Mode::Space
                    && self.joining.is_some()
                    && self.joining.as_deref()
                        == self.space.as_ref().map(|space| space.id.as_str());
                if rejoining {
                    // The space is gone, as after the server restarts.
                    self.close_space();
                    self.mode = Mode::Lobby;
                    self.list();
                }
                // A refused join or decision can be tried again.
                self.joining = None;
                self.decided = None;
                self.error = Some(error);
            }
            // Speech, transcripts and presence come with the microphone, playback and presence work.
            ServerMessage::Welcome { .. }
            | ServerMessage::Transcript { .. }
            | ServerMessage::TranscriptFailed { .. }
            | ServerMessage::Speech { .. }
            | ServerMessage::Presence { .. }
            | ServerMessage::Ping { .. }
            | ServerMessage::Pong { .. } => {}
        }
    }

    /// Show a space's state. The server sends it after a join, on every change, and unasked when it rejoins this participant after a reconnect.
    fn update_space(&mut self, state: SpaceState) {
        if self.mode == Mode::Servers
            || (self.leaving && self.joining.as_deref() != Some(state.id.as_str()))
        {
            return;
        }
        let entered = self.mode != Mode::Space
            || self.space.as_ref().is_none_or(|space| space.id != state.id);
        if entered {
            log::info!("Joined space {} ({})", state.title, state.id);
            self.close_space();
            self.mode = Mode::Space;
        }
        self.joining = None;
        self.leaving = false;
        let request = state
            .approval
            .as_ref()
            .map(|approval| approval.id.clone())
            .or_else(|| state.question.as_ref().map(|question| question.id.clone()));
        if request != self.request {
            self.page = 0;
            self.option = 0;
            self.decided = None;
            self.request = request;
        }
        if let Some(approval) = &state.approval {
            self.option = self.option.min(approval.options.len());
        }
        let previous = self
            .space
            .as_ref()
            .and_then(|space| space.messages.get(self.message))
            .map(|message| message.id.clone());
        if self.follow {
            self.message = state.messages.len().saturating_sub(1);
            if self.request.is_none() {
                self.page = state
                    .messages
                    .last()
                    .map(|message| pages(&message.text).len() - 1)
                    .unwrap_or(0);
            }
        } else if let Some(index) =
            previous.and_then(|id| state.messages.iter().position(|message| message.id == id))
        {
            self.message = index;
        } else {
            self.message = self.message.min(items(&state).saturating_sub(1));
        }
        self.space = Some(state);
    }

    fn close_space(&mut self) {
        self.space = None;
        self.joining = None;
        self.message = 0;
        self.page = 0;
        self.follow = true;
        self.request = None;
        self.option = 0;
        self.decided = None;
        self.notice = None;
    }

    fn list(&mut self) {
        self.actions.push(Action::Send(ClientMessage::ListSpaces));
    }

    fn send(&mut self, message: ClientMessage) {
        self.error = None;
        self.actions.push(Action::Send(message));
    }

    /// The pending approval that a press would decide, if any.
    fn approval(&self) -> Option<&protocol::Approval> {
        self.space
            .as_ref()
            .and_then(|space| space.approval.as_ref())
            .filter(|approval| self.decided.as_ref() != Some(&approval.id))
    }

    /// Whether the right stick chooses approval options rather than presentation cards.
    pub fn is_choosing(&self) -> bool {
        self.mode == Mode::Space
            && self
                .approval()
                .is_some_and(|approval| !approval.options.is_empty())
    }

    /// Left stick: choose a server or space, or page through messages.
    pub fn navigate(&mut self, horizontal: i32, vertical: i32) {
        match self.mode {
            Mode::Servers => {
                self.server = shift(self.server, vertical, self.servers.len());
                self.chosen |= vertical != 0;
            }
            Mode::Lobby => self.selected = shift(self.selected, vertical, self.spaces.len()),
            Mode::Space => {
                let Some(space) = &self.space else {
                    return;
                };
                let (count, messages) = (items(space), space.messages.len());
                if self.request.is_none() && horizontal != 0 {
                    self.message = shift(self.message, horizontal, count);
                    self.page = 0;
                }
                if vertical != 0 {
                    self.page = self.page.saturating_add_signed(vertical as isize);
                }
                let last_page = pages(&self.body().1).len() - 1;
                self.page = self.page.min(last_page);
                // Back at the newest page, follow new messages again.
                self.follow = self.request.is_none()
                    && self.message + 1 == messages.max(1)
                    && self.page == last_page;
            }
        }
    }

    /// Right stick left or right: choose among an approval's options.
    pub fn choose(&mut self, direction: i32) {
        if let Some(count) = self.approval().map(|approval| approval.options.len() + 1) {
            self.option = shift(self.option, direction, count);
        }
    }

    /// A: connect, join, or confirm the chosen approval option. Returns true when A has nothing to do here, so it can keep its swarm-reset meaning.
    pub fn accept(&mut self) -> bool {
        match self.mode {
            Mode::Servers => {
                match self.servers.get(self.server).cloned() {
                    Some(choice) if choice.is_available() => self.connect(choice),
                    Some(choice) => {
                        self.error = Some(format!(
                            "Cannot join {}: {}.",
                            choice.label,
                            choice.unavailable.unwrap_or_default()
                        ));
                    }
                    None => {}
                }
                false
            }
            Mode::Lobby => {
                if let Some(space) = self.spaces.get(self.selected) {
                    let space = space.id.clone();
                    self.joining = Some(space.clone());
                    self.leaving = false;
                    self.send(ClientMessage::Join { space });
                }
                false
            }
            Mode::Space => {
                let Some(approval) = self.approval() else {
                    // A decision on its way still owns the button.
                    return self
                        .space
                        .as_ref()
                        .is_none_or(|space| space.approval.is_none());
                };
                let request = approval.id.clone();
                let (decision, option) = match self.option.checked_sub(1) {
                    None => (Decision::AllowOnce, None),
                    Some(index) => (Decision::Grant, Some(approval.options[index].id.clone())),
                };
                self.decided = Some(request.clone());
                self.send(ClientMessage::Decide {
                    request,
                    decision,
                    option,
                });
                false
            }
        }
    }

    /// B: deny the pending approval, cancel your newest queued input, or leave the space; in the lobby, return to the server list.
    pub fn back(&mut self) {
        match self.mode {
            Mode::Servers => {}
            Mode::Lobby => {
                self.actions.push(Action::Disconnect);
                self.connected = None;
                self.state = None;
                self.spaces.clear();
                self.listed = false;
                self.close_space();
                self.leaving = false;
                self.error = None;
                self.mode = Mode::Servers;
            }
            Mode::Space => {
                let pending = self
                    .space
                    .as_ref()
                    .is_some_and(|space| space.approval.is_some());
                if let Some(approval) = self.approval() {
                    let request = approval.id.clone();
                    self.decided = Some(request.clone());
                    self.send(ClientMessage::Decide {
                        request,
                        decision: Decision::Deny,
                        option: None,
                    });
                } else if pending {
                    // Wait for the decision already sent.
                } else if let Some(entry) = self.own_queued().map(|entry| entry.id.clone()) {
                    self.send(ClientMessage::CancelQueued { entry });
                } else {
                    self.leaving = true;
                    self.close_space();
                    self.listed = false;
                    self.mode = Mode::Lobby;
                    self.send(ClientMessage::Leave);
                }
            }
        }
    }

    /// Y: browse again, list the spaces again, or fetch the space's state again.
    pub fn refresh(&mut self) {
        self.error = None;
        match self.mode {
            Mode::Servers => self.actions.push(Action::Rescan),
            Mode::Lobby => self.list(),
            Mode::Space => {
                if let Some(space) = self.space.as_ref().map(|space| space.id.clone()) {
                    self.joining = Some(space.clone());
                    self.send(ClientMessage::Join { space });
                }
            }
        }
    }

    /// X: speaking to the agent returns once the server recognizes speech.
    pub fn talk(&mut self) {
        self.notice = Some("Voice is moving to the server; X is not available yet.".into());
    }

    /// Your newest entry still waiting in the queue. Entries are matched by participant identity, since display names need not be unique.
    fn own_queued(&self) -> Option<&protocol::QueueEntry> {
        self.space.as_ref()?.queue.iter().rev().find(|entry| {
            entry.state == QueueState::Queued && entry.participant == self.participant
        })
    }

    /// What the swarm should show. `Unknown` and a lost connection are never shown as ready.
    pub fn activity(&self) -> SessionActivity {
        let Some(space) = self.space.as_ref().filter(|_| self.mode == Mode::Space) else {
            return SessionActivity::Unselected;
        };
        if !matches!(self.state, Some(ConnectionState::Connected { .. })) {
            return SessionActivity::Unavailable;
        }
        if self.decided.is_some() {
            return SessionActivity::Working;
        }
        if space.approval.is_some() || space.question.is_some() {
            return SessionActivity::Waiting;
        }
        match space.activity {
            Activity::Idle
                if space.queue.iter().any(|entry| {
                    matches!(entry.state, QueueState::Queued | QueueState::Sending)
                }) =>
            {
                SessionActivity::Working
            }
            Activity::Idle => SessionActivity::Ready,
            Activity::Running => SessionActivity::Working,
            Activity::Waiting => SessionActivity::Waiting,
            Activity::Closed => SessionActivity::Unavailable,
            Activity::Unknown => SessionActivity::Unknown,
        }
    }

    /// The panel text, at most [`ROWS`] lines of at most [`COLUMNS`] characters.
    pub fn text(&mut self) -> String {
        let lines = match self.mode {
            Mode::Servers => self.servers_text(),
            Mode::Lobby => self.lobby_text(),
            Mode::Space => self.space_text(),
        };
        debug_assert!(lines.len() <= ROWS);
        lines.join("\n")
    }

    fn status(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(format!("ERROR: {error}"));
        }
        match &self.state {
            None | Some(ConnectionState::Connecting { attempt: 1 }) => {
                return Some("Connecting...".into());
            }
            Some(ConnectionState::Connecting { attempt }) => {
                return Some(format!("Connecting (attempt {attempt})..."));
            }
            Some(ConnectionState::Disconnected { reason, retry_in }) => {
                return Some(format!(
                    "Offline, retry in {}s: {reason}",
                    retry_in.as_secs_f32().ceil()
                ));
            }
            Some(ConnectionState::Closed) => return Some("Disconnected.".into()),
            Some(ConnectionState::Connected { .. }) => {}
        }
        self.notice.clone()
    }

    fn servers_text(&self) -> Vec<String> {
        let mut lines = vec![
            short(&format!(
                "SERVERS {}/{}",
                (self.server + 1).min(self.servers.len()),
                self.servers.len()
            )),
            "Stick up/down: choose. A: connect.".into(),
        ];
        let rows = ROWS - 4;
        let start = window(self.server, self.servers.len(), rows);
        for (index, choice) in self.servers.iter().enumerate().skip(start).take(rows) {
            let marker = if index == self.server { '>' } else { ' ' };
            lines.push(short(&match &choice.unavailable {
                None => format!("{marker} {} {}", choice.label, choice.address),
                Some(reason) => format!("{marker} ({} - {reason})", choice.label),
            }));
        }
        while lines.len() < ROWS - 2 {
            lines.push(String::new());
        }
        lines.push(short(
            self.error.as_deref().unwrap_or("Searching the network..."),
        ));
        lines.push("A connect | Y search again".into());
        lines
    }

    fn lobby_text(&self) -> Vec<String> {
        let server = self
            .connected
            .as_ref()
            .map_or("server", |choice| choice.label.as_str());
        let mut lines = vec![short(&format!(
            "{server} | SPACES {}/{}",
            (self.selected + 1).min(self.spaces.len()),
            self.spaces.len()
        ))];
        lines.push(short(&self.status().unwrap_or_else(|| {
            if self.joining.is_some() {
                "Joining...".into()
            } else if !self.listed {
                "Listing spaces...".into()
            } else if self.spaces.is_empty() {
                "No spaces yet; create one from the PC.".into()
            } else {
                format!("You are {}.", self.name)
            }
        })));
        let rows = ROWS - 3;
        let start = window(self.selected, self.spaces.len(), rows);
        for (index, space) in self.spaces.iter().enumerate().skip(start).take(rows) {
            let marker = if index == self.selected { '>' } else { ' ' };
            lines.push(short(&format!(
                "{marker} {} [{}{}]",
                space.title,
                activity_label(space.activity),
                match space.participants {
                    0 => String::new(),
                    count => format!(", {count} here"),
                }
            )));
        }
        while lines.len() < ROWS - 1 {
            lines.push(String::new());
        }
        lines.push("A join | B servers | Y refresh".into());
        lines
    }

    /// The label and text of the selected item: the pending request, a message or the queue.
    fn body(&self) -> (String, String) {
        let Some(space) = &self.space else {
            return ("Loading".into(), String::new());
        };
        if let Some(approval) = &space.approval {
            return (
                format!("APPROVAL: {}", approval.title),
                approval.text.clone(),
            );
        }
        if let Some(question) = &space.question {
            let mut text = question.text.clone();
            text.push_str("\n\nAnswer this question on the PC.");
            return (format!("QUESTION: {}", question.title), text);
        }
        if let Some(message) = space.messages.get(self.message) {
            let role = match message.role {
                Role::User => "USER",
                Role::Assistant => "ASSISTANT",
            };
            return (
                format!("{role} {}/{}", self.message + 1, space.messages.len()),
                message.text.clone(),
            );
        }
        if self.message == space.messages.len() && !space.queue.is_empty() {
            let text = space
                .queue
                .iter()
                .map(|entry| {
                    format!(
                        "{} [{}]: {}",
                        entry.speaker,
                        queue_label(entry.state),
                        entry.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            return (format!("QUEUE {}", space.queue.len()), text);
        }
        ("No messages yet".into(), String::new())
    }

    fn space_text(&mut self) -> Vec<String> {
        let (label, body) = self.body();
        // The chosen option is shown in full, since a standing option's label is the whole of what it grants.
        let choice = self
            .space
            .as_ref()
            .and_then(|space| space.approval.as_ref())
            .filter(|_| self.decided.is_none())
            .map(|approval| {
                let option = match self.option.checked_sub(1) {
                    None => "Allow once",
                    Some(index) => approval.options[index].label.as_str(),
                };
                let mut lines = wrapped_lines(&format!(
                    "{}/{}: {option}",
                    self.option + 1,
                    approval.options.len() + 1
                ));
                if lines.len() >= BODY_ROWS {
                    lines.truncate(BODY_ROWS - 1);
                    if let Some(last) = lines.last_mut() {
                        *last = short(&format!("{last} more"));
                    }
                }
                lines
            });
        let pages = match &choice {
            Some(choice) => paginate(&body, BODY_ROWS - choice.len()),
            None => pages(&body),
        };
        self.page = self.page.min(pages.len() - 1);
        let Some(space) = &self.space else {
            return vec![
                "SPACE".into(),
                "Loading...".into(),
                String::new(),
                "B leave".into(),
            ];
        };
        let choosing = space.approval.as_ref().zip(choice);
        let status = self.status().unwrap_or_else(|| {
            if let Some(error) = &space.error {
                format!("ERROR: {error}")
            } else if let Some(entry) = self.own_queued() {
                format!("Queued: {}", entry.text)
            } else {
                let waiting = space
                    .queue
                    .iter()
                    .filter(|entry| entry.state == QueueState::Queued)
                    .count();
                match waiting {
                    0 => format!("{} here", space.participants.len()),
                    count => format!("{count} queued | {} here", space.participants.len()),
                }
            }
        });
        let mut lines = vec![
            short(&format!(
                "[{}] {}",
                activity_label(space.activity),
                space.title
            )),
            short(&status),
            short(&format!("p{}/{} {label}", self.page + 1, pages.len())),
        ];
        lines.extend(pages[self.page].iter().cloned());
        let footer = if let Some((approval, choice)) = choosing {
            while lines.len() + choice.len() < ROWS - 1 {
                lines.push(String::new());
            }
            lines.extend(choice);
            if approval.options.is_empty() {
                "A allow once | B deny | stick page"
            } else {
                "R-stick choose | A confirm | B deny"
            }
        } else if space.approval.is_some() {
            "Sending decision..."
        } else if self.own_queued().is_some() {
            "B cancel queued | stick pages"
        } else {
            "B leave | Y refresh | stick pages"
        };
        while lines.len() < ROWS - 1 {
            lines.push(String::new());
        }
        lines.push(footer.into());
        lines
    }
}

/// Messages, then the queue when it has entries.
fn items(space: &SpaceState) -> usize {
    space.messages.len() + usize::from(!space.queue.is_empty())
}

fn activity_label(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "idle",
        Activity::Running => "running",
        Activity::Waiting => "waiting",
        Activity::Closed => "closed",
        Activity::Unknown => "unknown",
    }
}

fn queue_label(state: QueueState) -> &'static str {
    match state {
        QueueState::Queued => "queued",
        QueueState::Sending => "sending",
        QueueState::Delivered => "sent",
        QueueState::Failed => "failed",
    }
}

/// The first of `rows` visible entries that keeps `selected` in view.
fn window(selected: usize, count: usize, rows: usize) -> usize {
    selected
        .saturating_sub(rows - 1)
        .min(count.saturating_sub(rows))
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
    paginate(text, BODY_ROWS)
}

fn paginate(text: &str, rows: usize) -> Vec<Vec<String>> {
    let lines = wrapped_lines(text);
    if lines.is_empty() {
        vec![vec![String::new()]]
    } else {
        lines.chunks(rows).map(|page| page.to_vec()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_choice::{Source, USB};
    use protocol::{Approval, ApprovalOption, Message, Participant, Question, QueueEntry};
    use std::time::Duration;

    const SPACE: &str = "space-1";

    fn connected() -> Event {
        Event::State(ConnectionState::Connected {
            source: 1,
            server: "studio".into(),
        })
    }

    fn space(change: impl FnOnce(&mut SpaceState)) -> Event {
        let mut state = SpaceState {
            id: SPACE.into(),
            title: "Tomori".into(),
            backend: "claude-code".into(),
            directory: "/work/tomori".into(),
            activity: Activity::Idle,
            participants: vec![Participant {
                id: "p".into(),
                name: "Ada".into(),
                source: 1,
            }],
            ..Default::default()
        };
        change(&mut state);
        Event::Message(ServerMessage::Space {
            state: Box::new(state),
        })
    }

    fn message(id: &str, role: Role, text: &str) -> Message {
        Message {
            id: id.into(),
            role,
            text: text.into(),
            completed: true,
        }
    }

    fn approval(options: &[(&str, &str)]) -> Approval {
        Approval {
            id: "request-1".into(),
            title: "Bash".into(),
            text: "cargo test".into(),
            options: options
                .iter()
                .map(|(id, label)| ApprovalOption {
                    id: (*id).into(),
                    label: (*label).into(),
                })
                .collect(),
        }
    }

    /// An entry spoken by `speaker`, whose participant identity is their name in lower case.
    fn entry(id: &str, speaker: &str, state: QueueState) -> QueueEntry {
        QueueEntry {
            id: id.into(),
            participant: speaker.to_lowercase(),
            speaker: speaker.into(),
            text: format!("text of {id}"),
            state,
        }
    }

    fn sent(conversation: &mut Conversation) -> Vec<ClientMessage> {
        conversation
            .take_actions()
            .into_iter()
            .filter_map(|action| match action {
                Action::Send(message) => Some(message),
                _ => None,
            })
            .collect()
    }

    /// A conversation connected to a server and joined to [`SPACE`].
    fn joined(change: impl FnOnce(&mut SpaceState)) -> Conversation {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
        conversation.apply(connected());
        conversation.apply(space(change));
        conversation.take_actions();
        conversation
    }

    fn assert_fits(conversation: &mut Conversation) {
        let text = conversation.text();
        assert!(text.lines().count() <= ROWS, "{text}");
        assert!(text.lines().all(|line| line.len() <= COLUMNS), "{text}");
    }

    #[test]
    fn connects_lists_and_joins() {
        let mut conversation = Conversation::new("ada", "Ada");
        assert!(conversation.is_browsing());
        conversation.set_servers(vec![
            Choice::new("Studio", "10.0.0.1:8765", Source::Discovered),
            Choice::new("USB", USB, Source::Usb),
        ]);
        assert_fits(&mut conversation);
        conversation.navigate(0, 1);
        assert!(!conversation.accept());
        assert_eq!(
            conversation.take_actions(),
            [Action::Connect(Choice::new("USB", USB, Source::Usb))]
        );
        assert_eq!(conversation.mode(), Mode::Lobby);
        assert!(!conversation.is_browsing());

        conversation.apply(connected());
        assert_eq!(sent(&mut conversation), [ClientMessage::ListSpaces]);
        conversation.apply(Event::Message(ServerMessage::Spaces {
            spaces: vec![SpaceSummary {
                id: SPACE.into(),
                title: "Tomori".into(),
                backend: "claude-code".into(),
                directory: "/work/tomori".into(),
                activity: Activity::Running,
                participants: 2,
            }],
        }));
        assert!(conversation.text().contains("> Tomori [running, 2 here]"));
        assert_fits(&mut conversation);
        assert!(!conversation.accept());
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::Join {
                space: SPACE.into()
            }]
        );
        conversation.apply(space(|_| {}));
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn an_incompatible_server_cannot_be_chosen() {
        let mut conversation = Conversation::new("ada", "Ada");
        let mut old = Choice::new("Old", "10.0.0.3:8765", Source::Discovered);
        old.unavailable = Some("protocol 2, needs 1".into());
        conversation.set_servers(vec![old]);
        assert!(conversation.text().contains("(Old - protocol 2, needs 1)"));
        conversation.accept();
        assert!(conversation.take_actions().is_empty());
        assert_eq!(conversation.mode(), Mode::Servers);
        assert!(conversation.text().contains("Cannot join Old"));
    }

    #[test]
    fn approval_with_options_sends_grant_with_the_chosen_option() {
        let mut conversation = joined(|state| {
            state.activity = Activity::Waiting;
            state.approval = Some(approval(&[
                ("project", "Always allow in this project"),
                ("session", "Allow for this session"),
            ]));
        });
        assert!(conversation.is_choosing());
        assert!(conversation.text().contains("1/3: Allow once"));
        conversation.choose(1);
        conversation.choose(1);
        conversation.choose(1);
        assert!(conversation.text().contains("3/3: Allow for this session"));
        conversation.choose(-1);
        assert_fits(&mut conversation);
        assert!(!conversation.accept());
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::Decide {
                request: "request-1".into(),
                decision: Decision::Grant,
                option: Some("project".into()),
            }]
        );
        // Further presses neither decide again nor leave while the decision is on its way.
        assert!(!conversation.accept());
        conversation.back();
        assert!(conversation.take_actions().is_empty());
        assert_eq!(conversation.mode(), Mode::Space);
        assert!(conversation.text().contains("Sending decision..."));
    }

    #[test]
    fn approval_without_a_choice_allows_once_and_b_denies() {
        let mut conversation = joined(|state| state.approval = Some(approval(&[])));
        assert!(!conversation.is_choosing());
        conversation.choose(1);
        conversation.accept();
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::Decide {
                request: "request-1".into(),
                decision: Decision::AllowOnce,
                option: None,
            }]
        );

        let mut conversation = joined(|state| state.approval = Some(approval(&[("a", "A")])));
        conversation.back();
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::Decide {
                request: "request-1".into(),
                decision: Decision::Deny,
                option: None,
            }]
        );
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn no_decision_is_sent_without_a_press() {
        let mut conversation = joined(|_| {});
        for _ in 0..3 {
            conversation.apply(space(|state| {
                state.approval = Some(approval(&[("a", "A")]));
            }));
            conversation.apply(connected());
            conversation.navigate(1, 1);
            conversation.choose(1);
            conversation.refresh();
            assert_fits(&mut conversation);
        }
        assert!(
            sent(&mut conversation)
                .iter()
                .all(|message| !matches!(message, ClientMessage::Decide { .. }))
        );
    }

    #[test]
    fn a_new_approval_resets_the_choice_and_a_refused_decision_can_be_retried() {
        let mut conversation = joined(|state| state.approval = Some(approval(&[("a", "A")])));
        conversation.choose(1);
        conversation.accept();
        conversation.take_actions();
        conversation.apply(Event::Message(ServerMessage::Error {
            error: "That request is no longer pending.".into(),
        }));
        assert!(conversation.is_choosing());
        assert!(conversation.text().contains("ERROR: That request"));

        conversation.apply(space(|state| {
            let mut next = approval(&[("b", "B")]);
            next.id = "request-2".into();
            state.approval = Some(next);
        }));
        assert!(conversation.text().contains("1/2: Allow once"));
        conversation.accept();
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::Decide {
                request: "request-2".into(),
                decision: Decision::AllowOnce,
                option: None,
            }]
        );
    }

    #[test]
    fn a_standing_option_is_shown_in_full() {
        let label =
            "Yes, and don't ask again for Bash commands starting with cargo test in this project";
        let mut conversation =
            joined(|state| state.approval = Some(approval(&[("always", label)])));
        conversation.choose(1);
        let text = conversation.text();
        assert_eq!(text.lines().count(), ROWS);
        let shown = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(shown.contains(&format!("2/2: {label}")), "{text}");
    }

    #[test]
    fn activity_is_never_ready_unless_the_server_says_idle() {
        let mut conversation = Conversation::new("ada", "Ada");
        assert_eq!(conversation.activity(), SessionActivity::Unselected);

        let cases = [
            (Activity::Idle, SessionActivity::Ready),
            (Activity::Running, SessionActivity::Working),
            (Activity::Waiting, SessionActivity::Waiting),
            (Activity::Closed, SessionActivity::Unavailable),
            (Activity::Unknown, SessionActivity::Unknown),
        ];
        for (activity, expected) in cases {
            conversation = joined(|state| state.activity = activity);
            assert_eq!(conversation.activity(), expected, "{activity:?}");
        }

        let conversation = joined(|state| {
            state.queue = vec![entry("1", "Bea", QueueState::Queued)];
        });
        assert_eq!(conversation.activity(), SessionActivity::Working);
        let conversation = joined(|state| state.question = Some(question()));
        assert_eq!(conversation.activity(), SessionActivity::Waiting);

        let mut conversation = joined(|_| {});
        conversation.apply(Event::State(ConnectionState::Disconnected {
            reason: client::Disconnect::Timeout,
            retry_in: Duration::from_secs(2),
        }));
        assert_eq!(conversation.activity(), SessionActivity::Unavailable);
        assert!(conversation.text().contains("Offline, retry in 2s"));
        assert_fits(&mut conversation);
    }

    #[test]
    fn an_unknown_activity_from_a_newer_server_is_shown_as_unknown() {
        let state: SpaceState = serde_json::from_str(
            r#"{"id":"s","title":"t","backend":"b","directory":"/","activity":"thinking",
            "messages":[],"queue":[],"approval":null,"question":null,"workers":[],
            "presentations":[],"participants":[],"attribution":true,"error":null}"#,
        )
        .unwrap();
        let mut conversation = joined(|_| {});
        conversation.apply(Event::Message(ServerMessage::Space {
            state: Box::new(state),
        }));
        assert_eq!(conversation.activity(), SessionActivity::Unknown);
        assert!(conversation.text().starts_with("[unknown] t"));
    }

    fn question() -> Question {
        Question {
            id: "question-1".into(),
            title: "Which test?".into(),
            text: "Pick one".into(),
            field: "test".into(),
            supported: true,
        }
    }

    #[test]
    fn reconnecting_accepts_the_space_the_server_rejoins() {
        let mut conversation = joined(|state| {
            state.messages = vec![message("m1", Role::Assistant, "Hello")];
        });
        conversation.apply(Event::State(ConnectionState::Disconnected {
            reason: client::Disconnect::Closed,
            retry_in: Duration::from_secs(1),
        }));
        conversation.apply(Event::State(ConnectionState::Connecting { attempt: 2 }));
        assert_eq!(conversation.mode(), Mode::Space);
        conversation.apply(connected());
        // The client restores membership itself; nothing is asked of the server.
        assert!(conversation.take_actions().is_empty());
        conversation.apply(space(|state| {
            state.messages = vec![
                message("m1", Role::Assistant, "Hello"),
                message("m2", Role::Assistant, "Welcome back"),
            ];
        }));
        assert_eq!(conversation.activity(), SessionActivity::Ready);
        assert!(conversation.text().contains("Welcome back"));
    }

    #[test]
    fn a_space_lost_while_reconnecting_returns_to_the_lobby() {
        let mut conversation = joined(|_| {});
        conversation.apply(Event::State(ConnectionState::Disconnected {
            reason: client::Disconnect::Closed,
            retry_in: Duration::from_secs(1),
        }));
        conversation.apply(connected());
        conversation.apply(Event::Message(ServerMessage::Error {
            error: "That space does not exist.".into(),
        }));
        assert_eq!(conversation.mode(), Mode::Lobby);
        assert_eq!(sent(&mut conversation), [ClientMessage::ListSpaces]);
        assert!(conversation.text().contains("That space does not exist."));
    }

    #[test]
    fn a_refused_decision_in_a_space_stays_in_the_space() {
        let mut conversation = joined(|_| {});
        conversation.apply(Event::Message(ServerMessage::Error {
            error: "That approval was already decided.".into(),
        }));
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn the_server_rejoining_after_a_reconnect_in_the_lobby_enters_the_space() {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Override));
        conversation.apply(connected());
        conversation.apply(space(|_| {}));
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn updates_sent_before_a_leave_was_handled_are_ignored() {
        let mut conversation = joined(|_| {});
        conversation.back();
        assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
        assert_eq!(conversation.mode(), Mode::Lobby);
        conversation.apply(space(|_| {}));
        assert_eq!(conversation.mode(), Mode::Lobby);
        conversation.apply(Event::Message(ServerMessage::Left));
        // Leaving is followed by a listing, so none is requested.
        assert!(conversation.take_actions().is_empty());
        conversation.apply(space(|_| {}));
        assert_eq!(conversation.mode(), Mode::Space);
    }

    #[test]
    fn being_left_by_the_server_returns_to_the_lobby_and_lists() {
        let mut conversation = joined(|_| {});
        conversation.apply(Event::Message(ServerMessage::Left));
        assert_eq!(conversation.mode(), Mode::Lobby);
        assert_eq!(sent(&mut conversation), [ClientMessage::ListSpaces]);
    }

    #[test]
    fn b_cancels_your_newest_queued_entry_before_leaving() {
        let mut conversation = joined(|state| {
            state.queue = vec![
                entry("1", "Ada", QueueState::Queued),
                entry("2", "Bea", QueueState::Queued),
                entry("3", "Ada", QueueState::Queued),
                entry("4", "Ada", QueueState::Sending),
            ];
        });
        assert!(conversation.text().contains("Queued: text of 3"));
        assert!(conversation.text().contains("B cancel queued"));
        conversation.back();
        assert_eq!(
            sent(&mut conversation),
            [ClientMessage::CancelQueued { entry: "3".into() }]
        );
        conversation.apply(space(|state| {
            state.queue = vec![
                entry("2", "Bea", QueueState::Queued),
                entry("4", "Ada", QueueState::Sending),
            ];
        }));
        // Others' entries and those already sending are not yours to cancel here.
        conversation.back();
        assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
    }

    #[test]
    fn another_participant_with_your_name_does_not_own_your_entries() {
        let mut conversation = joined(|state| {
            let mut namesake = entry("1", "Ada", QueueState::Queued);
            namesake.participant = "another-ada".into();
            state.queue = vec![namesake];
        });
        assert!(!conversation.text().contains("B cancel queued"));
        conversation.back();
        assert_eq!(sent(&mut conversation), [ClientMessage::Leave]);
    }

    #[test]
    fn the_queue_is_shown_after_the_messages() {
        let mut conversation = joined(|state| {
            state.messages = vec![message("m1", Role::User, "Hi")];
            state.queue = vec![
                entry("1", "Bea", QueueState::Sending),
                entry("2", "Ada", QueueState::Queued),
            ];
        });
        assert!(conversation.text().contains("USER 1/1"));
        conversation.navigate(1, 0);
        let text = conversation.text();
        assert!(text.contains("QUEUE 2"), "{text}");
        assert!(text.contains("Bea [sending]: text of 1"), "{text}");
        assert!(text.contains("Ada [queued]: text of 2"), "{text}");
    }

    #[test]
    fn pages_through_long_messages_and_follows_new_ones_from_the_last_page() {
        let long = (0..30).map(|i| format!("line {i}\n")).collect::<String>();
        let mut conversation = joined(|state| {
            state.messages = vec![
                message("m1", Role::User, &long),
                message("m2", Role::Assistant, &long),
            ];
        });
        // Following shows the newest message's last page.
        let total = pages(&long).len();
        assert!(total > 1);
        assert!(
            conversation
                .text()
                .contains(&format!("p{total}/{total} ASSISTANT 2/2"))
        );
        conversation.navigate(-1, 0);
        assert!(
            conversation
                .text()
                .contains(&format!("p1/{total} USER 1/2"))
        );
        conversation.navigate(0, 1);
        assert!(
            conversation
                .text()
                .contains(&format!("p2/{total} USER 1/2"))
        );

        // While reading an older message, new ones do not move the view.
        conversation.apply(space(|state| {
            state.messages = vec![
                message("m1", Role::User, &long),
                message("m2", Role::Assistant, &long),
                message("m3", Role::Assistant, "New"),
            ];
        }));
        assert!(conversation.text().contains("USER 1/3"));

        // Returning to the newest page follows again.
        conversation.navigate(1, 0);
        conversation.navigate(1, 0);
        assert!(conversation.text().contains("ASSISTANT 3/3"));
        conversation.apply(space(|state| {
            state.messages = vec![
                message("m1", Role::User, &long),
                message("m2", Role::Assistant, &long),
                message("m3", Role::Assistant, "New"),
                message("m4", Role::Assistant, "Newer"),
            ];
        }));
        assert!(conversation.text().contains("ASSISTANT 4/4"));
        assert_fits(&mut conversation);
    }

    #[test]
    fn everything_fits_the_panel() {
        let long = "Very long reply ".repeat(1000);
        let mut conversation = joined(|state| {
            state.title = "A title far too long for one line of the panel".into();
            state.messages = vec![message("m1", Role::Assistant, &long)];
            state.queue = (0..20)
                .map(|i| entry(&i.to_string(), "Ada", QueueState::Queued))
                .collect();
            state.error = Some(long.clone());
        });
        assert_fits(&mut conversation);
        conversation.navigate(1, 0);
        assert_fits(&mut conversation);
        conversation.apply(space(|state| {
            let mut long_approval = approval(&[("a", &long)]);
            long_approval.text = long.clone();
            state.approval = Some(long_approval);
        }));
        conversation.choose(1);
        assert_fits(&mut conversation);
        conversation.apply(space(|state| state.question = Some(question())));
        assert!(
            conversation
                .text()
                .contains("Answer this question on the PC.")
        );
        assert_fits(&mut conversation);

        let mut conversation = Conversation::new("ada", "Ada");
        conversation.set_servers(
            (0..20)
                .map(|i| Choice::new(long.clone(), format!("10.0.0.{i}:8765"), Source::Discovered))
                .collect(),
        );
        conversation.navigate(0, 15);
        assert!(conversation.text().contains("SERVERS 16/20"));
        assert!(conversation.text().contains("> "));
        assert_fits(&mut conversation);
        conversation.accept();
        conversation.fail(long.clone());
        assert_fits(&mut conversation);
        conversation.apply(Event::Message(ServerMessage::Spaces {
            spaces: (0..20)
                .map(|i| SpaceSummary {
                    id: i.to_string(),
                    title: long.clone(),
                    backend: "claude-code".into(),
                    directory: "/".into(),
                    activity: Activity::Unknown,
                    participants: 0,
                })
                .collect(),
        }));
        conversation.navigate(0, 19);
        assert_fits(&mut conversation);

        let paged = pages(&(0..200).map(|i| format!("line {i}\n")).collect::<String>());
        assert!(paged.last().unwrap().last().unwrap().contains("199"));
        assert!(short(&"🦀".repeat(200)).len() <= COLUMNS);
    }

    #[test]
    fn b_in_the_lobby_returns_to_the_server_list() {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.connect(Choice::new("Studio", "10.0.0.1:8765", Source::Discovered));
        conversation.take_actions();
        conversation.back();
        assert_eq!(conversation.take_actions(), [Action::Disconnect]);
        assert!(conversation.is_browsing());
        assert_eq!(conversation.server(), None);
        conversation.refresh();
        assert_eq!(conversation.take_actions(), [Action::Rescan]);
    }

    #[test]
    fn x_explains_that_voice_is_moving() {
        let mut conversation = joined(|_| {});
        conversation.talk();
        assert!(
            conversation
                .text()
                .contains("Voice is moving to the server")
        );
    }

    #[test]
    fn keeps_the_selection_when_lists_change() {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.set_servers(vec![
            Choice::new("A", "a", Source::Discovered),
            Choice::new("B", "b", Source::Discovered),
        ]);
        conversation.navigate(0, 1);
        conversation.set_servers(vec![
            Choice::new("C", "c", Source::Discovered),
            Choice::new("A", "a", Source::Discovered),
            Choice::new("B", "b", Source::Discovered),
        ]);
        conversation.accept();
        assert!(matches!(
            conversation.take_actions().as_slice(),
            [Action::Connect(choice)] if choice.address == "b"
        ));
    }

    fn remembered() -> Choice {
        let mut choice = Choice::new("Studio", "10.0.0.2:8765", Source::Remembered);
        choice.instance = Some("studio".into());
        choice
    }

    #[test]
    fn connects_to_the_remembered_server_when_it_appears_but_only_once() {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.set_servers(vec![Choice::new("USB", USB, Source::Usb)]);
        assert!(conversation.take_actions().is_empty());
        conversation.set_servers(vec![remembered(), Choice::new("USB", USB, Source::Usb)]);
        assert!(matches!(
            conversation.take_actions().as_slice(),
            [Action::Connect(choice)] if choice.address == "10.0.0.2:8765"
        ));
        assert_eq!(conversation.mode(), Mode::Lobby);
        // Returning to the list leaves the choice to the participant.
        conversation.back();
        conversation.take_actions();
        conversation.set_servers(vec![remembered(), Choice::new("USB", USB, Source::Usb)]);
        assert!(conversation.take_actions().is_empty());
        assert_eq!(conversation.mode(), Mode::Servers);
    }

    #[test]
    fn moving_the_selection_stops_the_remembered_server_connecting() {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.set_servers(vec![
            Choice::new("Other", "10.0.0.3:8765", Source::Discovered),
            Choice::new("USB", USB, Source::Usb),
        ]);
        conversation.navigate(0, 1);
        conversation.set_servers(vec![
            remembered(),
            Choice::new("Other", "10.0.0.3:8765", Source::Discovered),
            Choice::new("USB", USB, Source::Usb),
        ]);
        assert!(conversation.take_actions().is_empty());
        assert_eq!(conversation.mode(), Mode::Servers);
    }

    #[test]
    fn selects_the_best_server_until_one_is_chosen() {
        let mut conversation = Conversation::new("ada", "Ada");
        conversation.set_servers(vec![Choice::new("USB", USB, Source::Usb)]);
        conversation.set_servers(vec![
            Choice::new("Studio", "10.0.0.2:8765", Source::Discovered),
            Choice::new("USB", USB, Source::Usb),
        ]);
        conversation.accept();
        assert!(matches!(
            conversation.take_actions().as_slice(),
            [Action::Connect(choice)] if choice.address == "10.0.0.2:8765"
        ));
    }
}
