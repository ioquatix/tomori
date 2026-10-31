// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::{HashSet, VecDeque};

use backend::{AnswerValue, Event, Part, Permission, Prompt, Snapshot};
use protocol::{Activity, Participant, QueueState, SpaceState, SpaceSummary};

use crate::projection;
use crate::queue::InputQueue;

/// Messages kept in the shared conversation.
const MESSAGE_LIMIT: usize = 50;

/// Longest message text sent to clients.
const MESSAGE_TEXT_LIMIT: usize = 20_000;

/// The shared state of one space, driven by backend events and participant actions. Pure: no I/O.
#[derive(Debug)]
pub struct SpaceModel {
    pub id: String,
    pub title: String,
    pub backend: String,
    pub directory: String,
    /// The bound session's identifier, namespaced by backend.
    pub session: String,
    /// Participant identifiers, in join order.
    pub participants: Vec<String>,
    busy: Option<bool>,
    closed: bool,
    messages: Vec<MessageModel>,
    /// Backend user messages, whose text is already shown from the delivered prompt.
    user_messages: HashSet<String>,
    queue: InputQueue,
    in_flight: Option<Prompt>,
    /// Pending permissions, oldest first. The first is presented; the rest wait their turn.
    approvals: VecDeque<Permission>,
    /// Permissions whose decision has been sent to the backend but not yet confirmed.
    deciding: Vec<Permission>,
    /// Pending questions, oldest first. The first is presented and collects answers.
    questions: VecDeque<PendingQuestion>,
    /// Questions whose answers have been sent to the backend but not yet confirmed.
    submitting: Vec<PendingQuestion>,
    workers: Vec<protocol::Worker>,
    attribution: bool,
    error: Option<String>,
}

#[derive(Debug)]
struct MessageModel {
    id: String,
    role: protocol::Role,
    text: String,
    tools: Vec<ToolModel>,
    completed: bool,
}

#[derive(Debug)]
struct ToolModel {
    id: String,
    name: String,
    state: &'static str,
}

#[derive(Clone, Debug)]
struct PendingQuestion {
    question: backend::Question,
    answers: Vec<(String, AnswerValue)>,
}

/// The outcome of a participant's answer to the current question field.
#[derive(Debug, PartialEq)]
pub enum AnswerOutcome {
    /// The answer was recorded; the question has more fields.
    Recorded,
    /// The question is complete and these answers must be sent to the backend.
    Complete(Vec<(String, AnswerValue)>),
}

impl MessageModel {
    /// Displayable text: the message text, or its tool activity when it has no text yet.
    fn display(&self) -> String {
        let text = if self.text.is_empty() {
            self.tools
                .iter()
                .map(|tool| format!("[{}: {}]", tool.name, tool.state))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            self.text.clone()
        };
        match text.char_indices().nth(MESSAGE_TEXT_LIMIT) {
            Some((index, _)) => text[..index].to_owned(),
            None => text,
        }
    }
}

fn role(role: backend::Role) -> protocol::Role {
    match role {
        backend::Role::User => protocol::Role::User,
        backend::Role::Assistant => protocol::Role::Assistant,
    }
}

fn worker(worker: backend::Worker) -> protocol::Worker {
    use backend::WorkerState as From;
    use protocol::WorkerState as To;
    protocol::Worker {
        id: worker.id,
        parent: worker.parent,
        title: worker.title,
        state: match worker.state {
            From::Running => To::Running,
            From::Waiting => To::Waiting,
            From::Idle => To::Idle,
            From::Completed => To::Completed,
            From::Error => To::Error,
            From::Unknown => To::Unknown,
        },
    }
}

/// The text delivered to the agent for a queue entry. With attribution, the speaker's display name prefixes the text as `Name: text`.
pub fn attributed(speaker: &str, text: &str, attribution: bool) -> String {
    if attribution && !speaker.is_empty() {
        format!("{speaker}: {text}")
    } else {
        text.to_owned()
    }
}

impl SpaceModel {
    pub fn new(
        id: String,
        title: String,
        backend: String,
        directory: String,
        session: String,
        snapshot: Snapshot,
    ) -> Self {
        let mut model = Self {
            id,
            title,
            backend,
            directory,
            session,
            participants: Vec::new(),
            busy: snapshot.busy,
            closed: false,
            messages: Vec::new(),
            user_messages: HashSet::new(),
            queue: InputQueue::default(),
            in_flight: None,
            approvals: snapshot.permission.into_iter().collect(),
            deciding: Vec::new(),
            questions: snapshot
                .question
                .into_iter()
                .map(|question| PendingQuestion {
                    question,
                    answers: Vec::new(),
                })
                .collect(),
            submitting: Vec::new(),
            workers: snapshot.workers.into_iter().map(worker).collect(),
            attribution: true,
            error: None,
        };
        for message in snapshot.messages {
            let mut text = Vec::new();
            let mut tools = Vec::new();
            for part in message.parts {
                match part {
                    Part::Text { text: part } => text.push(part),
                    Part::Reasoning { .. } => {}
                    Part::Tool {
                        id, tool, finished, ..
                    } => tools.push(ToolModel {
                        id,
                        name: tool,
                        state: if finished { "done" } else { "working" },
                    }),
                }
            }
            model.push_message(MessageModel {
                id: message.id,
                role: role(message.role),
                text: text.join("\n\n"),
                tools,
                completed: message.completed,
            });
        }
        model
    }

    pub fn activity(&self) -> Activity {
        if self.is_waiting() {
            return Activity::Waiting;
        }
        match self.busy {
            Some(true) => Activity::Running,
            Some(false) => Activity::Idle,
            None => Activity::Unknown,
        }
    }

    /// Whether a permission or question is pending, including one whose decision or answers are still being delivered.
    fn is_waiting(&self) -> bool {
        !self.approvals.is_empty()
            || !self.deciding.is_empty()
            || !self.questions.is_empty()
            || !self.submitting.is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn set_attribution(&mut self, enabled: bool) {
        self.attribution = enabled;
    }

    fn push_message(&mut self, message: MessageModel) {
        self.messages.push(message);
        if self.messages.len() > MESSAGE_LIMIT {
            let excess = self.messages.len() - MESSAGE_LIMIT;
            self.messages.drain(..excess);
        }
    }

    /// The message with this identifier, created as an assistant message if it is new.
    fn message(&mut self, id: &str) -> &mut MessageModel {
        let index = match self.messages.iter().rposition(|message| message.id == id) {
            Some(index) => index,
            None => {
                self.push_message(MessageModel {
                    id: id.to_owned(),
                    role: protocol::Role::Assistant,
                    text: String::new(),
                    tools: Vec::new(),
                    completed: false,
                });
                self.messages.len() - 1
            }
        };
        &mut self.messages[index]
    }

    /// Apply a backend event to the shared state.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Busy => self.busy = Some(true),
            Event::Idle => self.busy = Some(false),
            Event::Delivered { key } => self.delivered(&key),
            Event::MessageStarted { message, role } => match role {
                backend::Role::User => {
                    self.user_messages.insert(message);
                }
                backend::Role::Assistant => {
                    self.message(&message);
                }
            },
            Event::TextDelta { message, text } => {
                if !self.user_messages.contains(&message) {
                    self.message(&message).text.push_str(&text);
                }
            }
            Event::MessageCompleted { message } => {
                if !self.user_messages.remove(&message) {
                    self.message(&message).completed = true;
                }
            }
            Event::ToolStarted {
                message, tool, id, ..
            } => self.message(&message).tools.push(ToolModel {
                id,
                name: tool,
                state: "working",
            }),
            Event::ToolFinished { id, error } => {
                if let Some(tool) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .flat_map(|message| message.tools.iter_mut())
                    .find(|tool| tool.id == id)
                {
                    tool.state = if error.is_some() { "failed" } else { "done" };
                }
            }
            Event::PermissionRequested(permission) => {
                // Parallel tool calls produce several requests; each waits its turn.
                let known = self
                    .approvals
                    .iter()
                    .chain(&self.deciding)
                    .any(|pending| pending.id == permission.id);
                if !known {
                    self.approvals.push_back(permission);
                }
            }
            Event::PermissionResolved { id } => {
                self.approvals.retain(|pending| pending.id != id);
                self.deciding.retain(|pending| pending.id != id);
            }
            Event::QuestionAsked(question) => {
                let known = self
                    .questions
                    .iter()
                    .chain(&self.submitting)
                    .any(|pending| pending.question.id == question.id);
                if !known {
                    self.questions.push_back(PendingQuestion {
                        question,
                        answers: Vec::new(),
                    });
                }
            }
            Event::QuestionResolved { id } => {
                self.questions.retain(|pending| pending.question.id != id);
                self.submitting.retain(|pending| pending.question.id != id);
            }
            Event::WorkersChanged { workers } => {
                self.workers = workers.into_iter().map(worker).collect();
            }
            Event::Error { message } => self.error = Some(message),
            Event::Closed { reason } => {
                self.closed = true;
                self.busy = None;
                self.error = Some(reason.unwrap_or_else(|| "The agent session ended.".to_owned()));
                if let Some(prompt) = self.in_flight.take() {
                    self.queue.finish(&prompt.key, QueueState::Failed);
                }
                // A closed session can no longer receive decisions or answers.
                self.approvals.clear();
                self.deciding.clear();
                self.questions.clear();
                self.submitting.clear();
            }
        }
    }

    /// Queue finalized input. Returns false if this key was already queued.
    pub fn enqueue(&mut self, key: String, speaker: String, text: String) -> bool {
        self.queue.push(key, speaker, text)
    }

    pub fn cancel(&mut self, key: &str) -> Result<(), &'static str> {
        self.queue.cancel(key)
    }

    /// The next prompt to deliver, if the agent is idle, nothing is pending and input is queued. Marks the entry as sending.
    pub fn next_prompt(&mut self) -> Option<Prompt> {
        if self.closed || self.activity() != Activity::Idle || self.in_flight.is_some() {
            return None;
        }
        let attribution = self.attribution;
        let entry = self.queue.begin_next()?;
        let prompt = Prompt {
            key: entry.id.clone(),
            text: attributed(&entry.speaker, &entry.text, attribution),
        };
        self.in_flight = Some(prompt.clone());
        Some(prompt)
    }

    fn delivered(&mut self, key: &str) {
        if !self
            .in_flight
            .as_ref()
            .is_some_and(|prompt| prompt.key == key)
        {
            return;
        }
        let prompt = self.in_flight.take().expect("checked above");
        self.queue.finish(key, QueueState::Delivered);
        // The agent accepted the prompt, so its turn has started even if `Busy` has not arrived yet.
        self.busy = Some(true);
        self.push_message(MessageModel {
            id: prompt.key,
            role: protocol::Role::User,
            text: prompt.text,
            tools: Vec::new(),
            completed: true,
        });
    }

    /// The backend finished a `send` call.
    pub fn sent(&mut self, key: &str, result: Result<(), String>) {
        match result {
            Ok(()) => self.delivered(key),
            Err(error) => {
                if self
                    .in_flight
                    .as_ref()
                    .is_some_and(|prompt| prompt.key == key)
                {
                    self.in_flight = None;
                    self.queue.finish(key, QueueState::Failed);
                    self.error = Some(format!("Could not deliver input: {error}"));
                }
            }
        }
    }

    /// Claim the presented approval for a decision. Only the first claim succeeds; the next pending approval is presented at once.
    pub fn claim_approval(&mut self, request: &str) -> Result<(), &'static str> {
        match self.approvals.front() {
            Some(pending) if pending.id == request => {
                let claimed = self.approvals.pop_front().expect("checked above");
                self.deciding.push(claimed);
                Ok(())
            }
            _ => Err("That approval is no longer pending."),
        }
    }

    /// The backend accepted a decision.
    pub fn decided(&mut self, request: &str) {
        self.deciding.retain(|pending| pending.id != request);
    }

    /// The backend rejected a decision for a reason other than the request being gone: present the approval again, ahead of later ones.
    pub fn restore_approval(&mut self, request: &str) {
        if let Some(index) = self
            .deciding
            .iter()
            .position(|pending| pending.id == request)
        {
            let pending = self.deciding.remove(index);
            self.approvals.push_front(pending);
        }
    }

    /// Record an answer to the current field. The first answer to a field wins.
    pub fn answer(
        &mut self,
        question: &str,
        field: &str,
        text: &str,
    ) -> Result<AnswerOutcome, String> {
        let pending = self
            .questions
            .front_mut()
            .filter(|pending| pending.question.id == question)
            .ok_or("That question is no longer pending.")?;
        if !projection::supported(&pending.question) {
            return Err("Complete this question outside Tomori.".to_owned());
        }
        let current = projection::current_field(&pending.question, &pending.answers)
            .filter(|current| current.key == field)
            .ok_or("The question changed; read the current field and answer again.")?;
        let value = projection::answer_value(current, text)?;
        pending.answers.push((field.to_owned(), value));
        if projection::current_field(&pending.question, &pending.answers).is_some() {
            return Ok(AnswerOutcome::Recorded);
        }
        let complete = self.questions.pop_front().expect("checked above");
        let answers = complete.answers.clone();
        self.submitting.push(complete);
        Ok(AnswerOutcome::Complete(answers))
    }

    /// The backend accepted the answers to a question.
    pub fn answered(&mut self, question: &str) {
        self.submitting
            .retain(|pending| pending.question.id != question);
    }

    /// The backend rejected the completed answers for a reason other than the question being gone: present it again from the start, ahead of later questions.
    pub fn restore_question(&mut self, question: &str) {
        if let Some(index) = self
            .submitting
            .iter()
            .position(|pending| pending.question.id == question)
        {
            let mut pending = self.submitting.remove(index);
            pending.answers.clear();
            self.questions.push_front(pending);
        }
    }

    pub fn state(&self, participants: Vec<Participant>) -> SpaceState {
        SpaceState {
            id: self.id.clone(),
            title: self.title.clone(),
            backend: self.backend.clone(),
            directory: self.directory.clone(),
            activity: self.activity(),
            messages: self
                .messages
                .iter()
                .filter_map(|message| {
                    let text = message.display();
                    (!text.is_empty()).then(|| protocol::Message {
                        id: message.id.clone(),
                        role: message.role,
                        text,
                        completed: message.completed,
                    })
                })
                .collect(),
            queue: self.queue.entries().to_vec(),
            approval: self.approvals.front().map(projection::approval),
            question: self
                .questions
                .front()
                .map(|pending| projection::question(&pending.question, &pending.answers)),
            workers: self.workers.clone(),
            presentations: Vec::new(),
            participants,
            attribution: self.attribution,
            error: self.error.clone(),
        }
    }

    pub fn summary(&self) -> SpaceSummary {
        SpaceSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            backend: self.backend.clone(),
            directory: self.directory.clone(),
            activity: self.activity(),
            participants: self.participants.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> SpaceModel {
        SpaceModel::new(
            "s1".into(),
            "Title".into(),
            "fake".into(),
            "/tmp".into(),
            "fake-1".into(),
            Snapshot {
                busy: Some(false),
                ..Snapshot::default()
            },
        )
    }

    #[test]
    fn input_is_delivered_only_while_idle() {
        let mut space = model();
        space.enqueue("a".into(), "Ada".into(), "hello".into());
        space.enqueue("b".into(), "Bob".into(), "there".into());
        let prompt = space.next_prompt().unwrap();
        assert_eq!(prompt.text, "Ada: hello");
        assert!(space.next_prompt().is_none());
        space.apply(Event::Delivered { key: "a".into() });
        assert_eq!(space.activity(), Activity::Running);
        assert!(space.next_prompt().is_none());
        space.apply(Event::Idle);
        space.set_attribution(false);
        assert_eq!(space.next_prompt().unwrap().text, "there");
    }

    #[test]
    fn pending_requests_block_delivery_and_resolve_once() {
        let mut space = model();
        space.apply(Event::PermissionRequested(Permission {
            id: "p".into(),
            tool: "Bash".into(),
            input: serde_json::Value::Null,
            description: None,
        }));
        space.enqueue("a".into(), "Ada".into(), "hello".into());
        assert_eq!(space.activity(), Activity::Waiting);
        assert!(space.next_prompt().is_none());
        assert!(space.claim_approval("p").is_ok());
        assert!(space.claim_approval("p").is_err());
        space.restore_approval("p");
        assert!(space.claim_approval("p").is_ok());
        space.apply(Event::PermissionResolved { id: "p".into() });
        assert!(space.next_prompt().is_some());
    }

    fn permission(id: &str) -> Permission {
        Permission {
            id: id.into(),
            tool: "Bash".into(),
            input: serde_json::Value::Null,
            description: None,
        }
    }

    #[test]
    fn parallel_permissions_are_presented_in_order() {
        let mut space = model();
        space.apply(Event::PermissionRequested(permission("p1")));
        space.apply(Event::PermissionRequested(permission("p2")));
        space.apply(Event::PermissionRequested(permission("p1")));
        space.enqueue("a".into(), "Ada".into(), "hello".into());
        assert_eq!(space.state(Vec::new()).approval.unwrap().id, "p1");
        assert!(space.claim_approval("p2").is_err());
        assert!(space.claim_approval("p1").is_ok());
        assert_eq!(space.state(Vec::new()).approval.unwrap().id, "p2");

        // A recoverable failure presents the first request again, ahead of the second.
        space.restore_approval("p1");
        assert_eq!(space.state(Vec::new()).approval.unwrap().id, "p1");
        assert!(space.claim_approval("p1").is_ok());
        space.decided("p1");
        assert!(space.claim_approval("p2").is_ok());

        // Nothing is delivered until every request is resolved.
        assert_eq!(space.activity(), Activity::Waiting);
        assert!(space.next_prompt().is_none());
        space.apply(Event::PermissionResolved { id: "p2".into() });
        assert_eq!(space.activity(), Activity::Idle);
        assert!(space.next_prompt().is_some());
    }

    #[test]
    fn text_deltas_stream_into_messages() {
        let mut space = model();
        space.apply(Event::MessageStarted {
            message: "m".into(),
            role: backend::Role::Assistant,
        });
        space.apply(Event::TextDelta {
            message: "m".into(),
            text: "Hel".into(),
        });
        let state = space.state(Vec::new());
        assert_eq!(state.messages[0].text, "Hel");
        assert!(!state.messages[0].completed);
        space.apply(Event::TextDelta {
            message: "m".into(),
            text: "lo".into(),
        });
        space.apply(Event::MessageCompleted {
            message: "m".into(),
        });
        let state = space.state(Vec::new());
        assert_eq!(state.messages[0].text, "Hello");
        assert!(state.messages[0].completed);
    }

    #[test]
    fn user_message_events_do_not_duplicate_delivered_input() {
        let mut space = model();
        space.enqueue("a".into(), "Ada".into(), "hello".into());
        space.next_prompt().unwrap();
        space.sent("a", Ok(()));
        space.apply(Event::MessageStarted {
            message: "u".into(),
            role: backend::Role::User,
        });
        space.apply(Event::TextDelta {
            message: "u".into(),
            text: "Ada: hello".into(),
        });
        space.apply(Event::MessageCompleted {
            message: "u".into(),
        });
        let state = space.state(Vec::new());
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, protocol::Role::User);
        assert_eq!(state.queue[0].state, QueueState::Delivered);
    }

    #[test]
    fn tool_activity_is_shown_until_text_arrives() {
        let mut space = model();
        space.apply(Event::ToolStarted {
            message: "m".into(),
            tool: "Bash".into(),
            id: "t".into(),
            input: serde_json::Value::Null,
        });
        assert_eq!(space.state(Vec::new()).messages[0].text, "[Bash: working]");
        space.apply(Event::ToolFinished {
            id: "t".into(),
            error: None,
        });
        assert_eq!(space.state(Vec::new()).messages[0].text, "[Bash: done]");
    }
}
