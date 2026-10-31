// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Translation between Claude Code's stream-json protocol and backend [`Event`]s.
//!
//! [`State`] is a pure state machine: it consumes output lines and produces events and lines to write back. It has no
//! knowledge of the process, so it can be tested directly against recorded output.

use std::collections::{HashMap, HashSet};

use backend::{
    AnswerValue, Decision, Error, ErrorKind, Event, Field, FieldKind, FieldOption, Part,
    Permission, Prompt, Question, Result, Role, Snapshot, SnapshotMessage, Worker, WorkerState,
};
use serde_json::{Map, Value, json};

/// Tools that delegate to a subagent. Claude Code 2.1 names it `Agent`; earlier versions name it `Task`.
const WORKER_TOOLS: [&str; 2] = ["Agent", "Task"];

/// The tool Claude Code uses to ask the user structured questions.
const QUESTION_TOOL: &str = "AskUserQuestion";

/// The model Claude Code names on messages it generates itself, such as API errors.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// The agent-facing reason given when a participant denies a request.
const DENIED: &str = "The user denied this request.";

/// What handling one line produced.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    pub events: Vec<Event>,
    /// Lines to write to the agent, in order.
    pub writes: Vec<Value>,
}

#[derive(Debug)]
enum Pending {
    Permission(Permission),
    Question { question: Question, input: Value },
}

#[derive(Debug)]
pub(crate) struct State {
    session: String,
    busy: bool,
    messages: Vec<SnapshotMessage>,
    /// The index of the assistant message being streamed.
    current: Option<usize>,
    /// Content block indexes of the current message, mapped to part indexes.
    blocks: HashMap<u64, usize>,
    /// Text and reasoning parts of the current message whose complete content arrived in an `assistant` record.
    settled: HashSet<usize>,
    tools: HashSet<String>,
    /// Unresolved `can_use_tool` requests by request id, oldest first.
    pending: Vec<(String, Pending)>,
    workers: Vec<Worker>,
    /// Worker tool uses that run in the background, so their tool result does not mean completion.
    background: HashSet<String>,
    /// Prompt keys that were sent, so a key is delivered at most once.
    sent: HashSet<String>,
    /// Sent prompts awaiting acknowledgement, by message uuid.
    prompts: HashMap<String, Prompt>,
    /// Control requests sent to the agent and awaiting a response, by request id.
    requests: HashMap<String, &'static str>,
}

impl State {
    pub(crate) fn new(session: String) -> Self {
        Self {
            session,
            busy: false,
            messages: Vec::new(),
            current: None,
            blocks: HashMap::new(),
            settled: HashSet::new(),
            tools: HashSet::new(),
            pending: Vec::new(),
            workers: Vec::new(),
            background: HashSet::new(),
            sent: HashSet::new(),
            prompts: HashMap::new(),
            requests: HashMap::new(),
        }
    }

    pub(crate) fn session(&self) -> &str {
        &self.session
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        Snapshot {
            busy: Some(self.busy),
            messages: self.messages.clone(),
            permission: self.pending.iter().find_map(|(_, pending)| match pending {
                Pending::Permission(permission) => Some(permission.clone()),
                Pending::Question { .. } => None,
            }),
            question: self.pending.iter().find_map(|(_, pending)| match pending {
                Pending::Question { question, .. } => Some(question.clone()),
                Pending::Permission(_) => None,
            }),
            workers: self.workers.clone(),
        }
    }

    /// The user message for `prompt`, or `None` if its key was already sent.
    pub(crate) fn prompt(&mut self, prompt: Prompt) -> Option<Value> {
        if !self.sent.insert(prompt.key.clone()) {
            return None;
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let line = json!({
            "type": "user",
            "message": {"role": "user", "content": prompt.text},
            "parent_tool_use_id": null,
            "session_id": self.session,
            "uuid": uuid,
        });
        self.prompts.insert(uuid, prompt);
        Some(line)
    }

    /// A control request to the agent, such as `initialize` or `interrupt`.
    pub(crate) fn control_request(&mut self, subtype: &'static str) -> Value {
        let id = uuid::Uuid::new_v4().to_string();
        self.requests.insert(id.clone(), subtype);
        json!({"type": "control_request", "request_id": id, "request": {"subtype": subtype}})
    }

    /// Resolve a pending permission, or decline a pending question.
    pub(crate) fn decide(&mut self, id: &str, decision: Decision) -> Result<(Value, Event)> {
        let index = self.pending_index(id)?;
        let (response, event) = match (&self.pending[index].1, decision) {
            (Pending::Permission(permission), Decision::AllowOnce) => (
                json!({"behavior": "allow", "updatedInput": permission.input}),
                Event::PermissionResolved { id: id.into() },
            ),
            (Pending::Permission(_), Decision::Deny) => (
                json!({"behavior": "deny", "message": DENIED}),
                Event::PermissionResolved { id: id.into() },
            ),
            (Pending::Question { .. }, Decision::Deny) => (
                json!({"behavior": "deny", "message": DENIED}),
                Event::QuestionResolved { id: id.into() },
            ),
            (Pending::Question { .. }, Decision::AllowOnce) => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "a question is answered, not allowed",
                ));
            }
        };
        self.pending.remove(index);
        Ok((control_response(id, response), event))
    }

    /// Answer a pending question. Claude Code receives the answers as the question tool's updated input.
    pub(crate) fn answer(
        &mut self,
        id: &str,
        answers: Vec<(String, AnswerValue)>,
    ) -> Result<(Value, Event)> {
        let index = self.pending_index(id)?;
        let Pending::Question { question, input } = &self.pending[index].1 else {
            return Err(Error::new(
                ErrorKind::Invalid,
                "a permission is decided, not answered",
            ));
        };
        if answers.is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "no answers were given"));
        }

        let mut values = Map::new();
        for (key, value) in answers {
            let Some(field) = question.fields.iter().find(|field| field.key == key) else {
                return Err(Error::new(ErrorKind::Invalid, format!("no field {key:?}")));
            };
            let value = match (field.kind, value) {
                (FieldKind::Unsupported, _) => {
                    return Err(Error::new(
                        ErrorKind::Invalid,
                        format!("field {key:?} cannot be answered"),
                    ));
                }
                (_, AnswerValue::Text(text)) => text,
                // Claude Code joins the labels of a multiple selection with a comma.
                (_, AnswerValue::Choices(choices)) => choices.join(", "),
            };
            values.insert(key, Value::String(value));
        }

        let mut input = input.clone();
        if let Some(object) = input.as_object_mut() {
            object.insert("answers".into(), Value::Object(values));
        }
        let response = json!({"behavior": "allow", "updatedInput": input});
        self.pending.remove(index);
        Ok((
            control_response(id, response),
            Event::QuestionResolved { id: id.into() },
        ))
    }

    pub(crate) fn handle_line(&mut self, line: &str) -> Effects {
        let mut effects = Effects::default();
        // Claude Code writes only JSON to standard output in stream-json mode; anything else is not protocol.
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            self.handle(&value, &mut effects);
        }
        effects
    }

    fn handle(&mut self, value: &Value, effects: &mut Effects) {
        match str_field(value, "type") {
            Some("system") => self.system(value, effects),
            Some("stream_event") => {
                if is_top_level(value) {
                    self.stream_event(&value["event"], effects);
                }
            }
            Some("assistant") => self.assistant(value, effects),
            Some("user") => self.user(value, effects),
            Some("command_lifecycle") => {
                if str_field(value, "state") == Some("started")
                    && let Some(uuid) = str_field(value, "command_uuid")
                {
                    self.delivered(uuid, effects);
                }
            }
            Some("result") => {
                self.close_message(effects);
                // An interrupted turn also ends in an error result, with an `aborted_*` terminal reason.
                let aborted = str_field(value, "terminal_reason")
                    .is_some_and(|reason| reason.starts_with("aborted"));
                if value["is_error"].as_bool() == Some(true) && !aborted {
                    let message = str_field(value, "result")
                        .filter(|result| !result.is_empty())
                        .or_else(|| str_field(value, "subtype"))
                        .unwrap_or("the turn failed");
                    effects.events.push(Event::Error {
                        message: message.into(),
                    });
                }
                if self.busy {
                    self.busy = false;
                    effects.events.push(Event::Idle);
                }
            }
            Some("control_request") => self.control(value, effects),
            Some("control_response") => {
                let response = &value["response"];
                if let Some(id) = str_field(response, "request_id")
                    && self.requests.remove(id).is_some()
                    && str_field(response, "subtype") == Some("error")
                {
                    let message = str_field(response, "error").unwrap_or("control request failed");
                    effects.events.push(Event::Error {
                        message: message.into(),
                    });
                }
            }
            Some("control_cancel_request") => {
                if let Some(id) = str_field(value, "request_id")
                    && let Ok(index) = self.pending_index(id)
                {
                    let event = match self.pending.remove(index).1 {
                        Pending::Permission(_) => Event::PermissionResolved { id: id.into() },
                        Pending::Question { .. } => Event::QuestionResolved { id: id.into() },
                    };
                    effects.events.push(event);
                }
            }
            _ => {}
        }
    }

    fn system(&mut self, value: &Value, effects: &mut Effects) {
        match str_field(value, "subtype") {
            Some("init") => {
                if let Some(session) = str_field(value, "session_id") {
                    self.session = session.into();
                }
            }
            Some("status") if str_field(value, "status") == Some("requesting") => {
                self.begin_turn(effects);
            }
            Some("task_started") => {
                if value["is_backgrounded"].as_bool() == Some(true)
                    && let Some(id) = str_field(value, "tool_use_id")
                {
                    self.background.insert(id.into());
                }
            }
            Some("task_notification") => {
                if let Some(id) = str_field(value, "tool_use_id") {
                    let state = match str_field(value, "status") {
                        Some("completed") => WorkerState::Completed,
                        _ => WorkerState::Error,
                    };
                    self.set_worker(id, state, effects);
                }
            }
            _ => {}
        }
    }

    fn stream_event(&mut self, event: &Value, effects: &mut Effects) {
        match str_field(event, "type") {
            Some("message_start") => match str_field(&event["message"], "id") {
                Some(id) => self.start_message(id, effects),
                None => {
                    self.begin_turn(effects);
                    self.close_message(effects);
                }
            },
            Some("content_block_start") => {
                let Some(message) = self.current.map(|index| &mut self.messages[index]) else {
                    return;
                };
                let part = match str_field(&event["content_block"], "type") {
                    Some("text") => Part::Text {
                        text: String::new(),
                    },
                    Some("thinking") => Part::Reasoning {
                        text: String::new(),
                    },
                    // Tool use input streams as partial JSON; the tool is reported once the complete block arrives.
                    _ => return,
                };
                if let Some(index) = event["index"].as_u64() {
                    message.parts.push(part);
                    self.blocks.insert(index, message.parts.len() - 1);
                }
            }
            Some("content_block_delta") => {
                let Some(current) = self.current else { return };
                let Some(part) = event["index"]
                    .as_u64()
                    .and_then(|index| self.blocks.get(&index))
                else {
                    return;
                };
                let message = &mut self.messages[current];
                let delta = &event["delta"];
                match (&mut message.parts[*part], str_field(delta, "type")) {
                    (Part::Text { text }, Some("text_delta")) => {
                        let delta = str_field(delta, "text").unwrap_or_default();
                        text.push_str(delta);
                        effects.events.push(Event::TextDelta {
                            message: message.id.clone(),
                            text: delta.into(),
                        });
                    }
                    (Part::Reasoning { text }, Some("thinking_delta")) => {
                        text.push_str(str_field(delta, "thinking").unwrap_or_default());
                    }
                    _ => {}
                }
            }
            Some("message_stop") => self.close_message(effects),
            _ => {}
        }
    }

    /// One or more complete assistant content blocks. With partial messages, each block streams as stream events first
    /// and the record repeats it; without them, as after Claude Code falls back to a non-streaming request, the record is
    /// the only report of the message.
    fn assistant(&mut self, value: &Value, effects: &mut Effects) {
        let parent = str_field(value, "parent_tool_use_id");
        let message = &value["message"];
        let Some(message_id) = str_field(message, "id") else {
            return;
        };
        let Some(content) = message["content"].as_array() else {
            return;
        };
        // Synthetic messages, such as API errors, are not replies; the turn's result reports the failure.
        if str_field(message, "model") == Some(SYNTHETIC_MODEL)
            || value["isApiErrorMessage"] == true
        {
            return;
        }
        if parent.is_none() && !self.messages.iter().any(|m| m.id == message_id) {
            self.start_message(message_id, effects);
        }
        for block in content {
            match str_field(block, "type") {
                Some("text") if parent.is_none() => {
                    let text = str_field(block, "text").unwrap_or_default();
                    self.settle(message_id, Part::Text { text: text.into() }, effects);
                }
                Some("thinking") if parent.is_none() => {
                    let text = str_field(block, "thinking").unwrap_or_default();
                    self.settle(message_id, Part::Reasoning { text: text.into() }, effects);
                }
                Some("tool_use") => self.tool_use(parent, message_id, block, effects),
                _ => {}
            }
        }
    }

    /// Reconcile a complete text or reasoning block with what streamed for it. The block completes the earliest part of
    /// its kind not yet settled, and only text that did not stream is reported; a block that never streamed is added.
    fn settle(&mut self, message_id: &str, block: Part, effects: &mut Effects) {
        let Some(current) = self
            .current
            .filter(|index| self.messages[*index].id == message_id)
        else {
            // The message already completed, so its blocks have streamed.
            return;
        };
        let message = &mut self.messages[current];
        let reasoning = matches!(block, Part::Reasoning { .. });
        let (Part::Text { text: complete } | Part::Reasoning { text: complete }) = block else {
            unreachable!("only text and reasoning blocks are settled");
        };
        let found = message.parts.iter().enumerate().position(|(index, part)| {
            !self.settled.contains(&index)
                && match part {
                    Part::Text { .. } => !reasoning,
                    Part::Reasoning { .. } => reasoning,
                    Part::Tool { .. } => false,
                }
        });
        let (index, missing) = match found {
            Some(index) => {
                let (Part::Text { text } | Part::Reasoning { text }) = &mut message.parts[index]
                else {
                    unreachable!("the part has the block's kind");
                };
                // Streamed text is never retracted, so a block that disagrees with it adds nothing.
                let missing = complete
                    .strip_prefix(text.as_str())
                    .unwrap_or_default()
                    .to_owned();
                text.push_str(&missing);
                (index, missing)
            }
            // Claude Code omits thinking it does not display, leaving an empty block.
            None if complete.is_empty() => return,
            None => {
                message.parts.push(if reasoning {
                    Part::Reasoning {
                        text: complete.clone(),
                    }
                } else {
                    Part::Text {
                        text: complete.clone(),
                    }
                });
                (message.parts.len() - 1, complete)
            }
        };
        self.settled.insert(index);
        // Later deltas for the block are already included.
        self.blocks.retain(|_, part| *part != index);
        if !reasoning && !missing.is_empty() {
            effects.events.push(Event::TextDelta {
                message: message.id.clone(),
                text: missing,
            });
        }
    }

    fn tool_use(
        &mut self,
        parent: Option<&str>,
        message_id: &str,
        block: &Value,
        effects: &mut Effects,
    ) {
        let (Some(id), Some(tool)) = (str_field(block, "id"), str_field(block, "name")) else {
            return;
        };
        let input = block.get("input").cloned().unwrap_or(Value::Null);

        if WORKER_TOOLS.contains(&tool) && !self.workers.iter().any(|worker| worker.id == id) {
            let title = str_field(&input, "description")
                .or_else(|| str_field(&input, "subagent_type"))
                .unwrap_or(tool);
            self.workers.push(Worker {
                id: id.into(),
                parent: Some(parent.unwrap_or(&self.session).into()),
                title: title.into(),
                state: WorkerState::Running,
            });
            effects.events.push(Event::WorkersChanged {
                workers: self.workers.clone(),
            });
        }

        // Only the session's own tool activity is reported; subagent activity is summarized by its worker.
        if parent.is_none() && self.tools.insert(id.into()) {
            if let Some(message) = self.messages.iter_mut().rev().find(|m| m.id == message_id) {
                message.parts.push(Part::Tool {
                    id: id.into(),
                    tool: tool.into(),
                    input: input.clone(),
                    finished: false,
                });
            }
            effects.events.push(Event::ToolStarted {
                message: message_id.into(),
                tool: tool.into(),
                id: id.into(),
                input,
            });
        }
    }

    fn user(&mut self, value: &Value, effects: &mut Effects) {
        if value["isReplay"].as_bool() == Some(true) {
            if let Some(uuid) = str_field(value, "uuid") {
                self.delivered(uuid, effects);
            }
            return;
        }

        let top_level = is_top_level(value);
        let Some(content) = value["message"]["content"].as_array() else {
            return;
        };
        for block in content {
            if str_field(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(id) = str_field(block, "tool_use_id") else {
                continue;
            };
            let failed = block["is_error"].as_bool() == Some(true);

            if self.workers.iter().any(|worker| worker.id == id) {
                if failed {
                    self.set_worker(id, WorkerState::Error, effects);
                } else if !self.background.contains(id) {
                    self.set_worker(id, WorkerState::Completed, effects);
                }
            }

            if top_level && self.tools.contains(id) {
                for message in self.messages.iter_mut().rev() {
                    if let Some(Part::Tool { finished, .. }) = message
                        .parts
                        .iter_mut()
                        .find(|part| matches!(part, Part::Tool { id: tool, .. } if tool == id))
                    {
                        *finished = true;
                        break;
                    }
                }
                effects.events.push(Event::ToolFinished {
                    id: id.into(),
                    error: failed.then(|| result_text(&block["content"])),
                });
            }
        }
    }

    fn control(&mut self, value: &Value, effects: &mut Effects) {
        let Some(id) = str_field(value, "request_id") else {
            return;
        };
        let request = &value["request"];
        if str_field(request, "subtype") != Some("can_use_tool") {
            // Tomori registers no hooks or SDK tools, so no other request is expected; never leave the agent waiting.
            effects.writes.push(json!({
                "type": "control_response",
                "response": {"subtype": "error", "request_id": id, "error": "unsupported control request"},
            }));
            return;
        }

        let tool = str_field(request, "tool_name").unwrap_or_default();
        let input = request.get("input").cloned().unwrap_or(Value::Null);

        if tool == QUESTION_TOOL
            && let Some(question) = question(id, &input)
        {
            effects.events.push(Event::QuestionAsked(question.clone()));
            self.pending
                .push((id.into(), Pending::Question { question, input }));
            return;
        }

        let permission = Permission {
            id: id.into(),
            tool: tool.into(),
            description: str_field(request, "description")
                .or_else(|| str_field(&input, "description"))
                .map(Into::into),
            input,
        };
        effects
            .events
            .push(Event::PermissionRequested(permission.clone()));
        self.pending
            .push((id.into(), Pending::Permission(permission)));
    }

    fn begin_turn(&mut self, effects: &mut Effects) {
        if !self.busy {
            self.busy = true;
            effects.events.push(Event::Busy);
        }
    }

    fn delivered(&mut self, uuid: &str, effects: &mut Effects) {
        let Some(prompt) = self.prompts.remove(uuid) else {
            return;
        };
        effects.events.push(Event::Delivered { key: prompt.key });
        self.messages.push(SnapshotMessage {
            id: uuid.into(),
            role: Role::User,
            parts: vec![Part::Text { text: prompt.text }],
            completed: true,
        });
        self.begin_turn(effects);
    }

    /// Complete the current assistant message and start the next one.
    fn start_message(&mut self, id: &str, effects: &mut Effects) {
        self.begin_turn(effects);
        self.close_message(effects);
        self.messages.push(SnapshotMessage {
            id: id.into(),
            role: Role::Assistant,
            parts: Vec::new(),
            completed: false,
        });
        self.current = Some(self.messages.len() - 1);
        effects.events.push(Event::MessageStarted {
            message: id.into(),
            role: Role::Assistant,
        });
    }

    fn close_message(&mut self, effects: &mut Effects) {
        if let Some(index) = self.current.take() {
            self.blocks.clear();
            self.settled.clear();
            let message = &mut self.messages[index];
            message.completed = true;
            effects.events.push(Event::MessageCompleted {
                message: message.id.clone(),
            });
        }
    }

    fn set_worker(&mut self, id: &str, state: WorkerState, effects: &mut Effects) {
        if let Some(worker) = self.workers.iter_mut().find(|worker| worker.id == id)
            && worker.state != state
        {
            worker.state = state;
            effects.events.push(Event::WorkersChanged {
                workers: self.workers.clone(),
            });
        }
    }

    fn pending_index(&self, id: &str) -> Result<usize> {
        self.pending
            .iter()
            .position(|(pending, _)| pending == id)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no pending request {id:?}")))
    }
}

fn control_response(id: &str, response: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": id, "response": response},
    })
}

/// Map `AskUserQuestion` input to a question. Each field is keyed by its question text, as Claude Code keys answers.
fn question(id: &str, input: &Value) -> Option<Question> {
    let questions = input["questions"].as_array()?;
    let mut fields = Vec::with_capacity(questions.len());
    for question in questions {
        let text = str_field(question, "question")?;
        let options: Vec<FieldOption> = question["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| {
                Some(FieldOption {
                    label: str_field(option, "label")?.into(),
                    description: str_field(option, "description").map(Into::into),
                })
            })
            .collect();
        let kind = match str_field(question, "kind") {
            None | Some("choice") if question["multiSelect"].as_bool() == Some(true) => {
                FieldKind::MultipleChoice
            }
            None | Some("choice") => FieldKind::Choice,
            // Number answers are returned to Claude Code as text, like every other answer.
            Some("text" | "number") => FieldKind::Text,
            Some(_) => FieldKind::Unsupported,
        };
        fields.push(Field {
            key: text.into(),
            title: text.into(),
            description: str_field(question, "description").map(Into::into),
            kind,
            options,
            // Claude Code always lets the user type an answer other than the listed options.
            custom: matches!(kind, FieldKind::Choice | FieldKind::MultipleChoice),
        });
    }
    let title = str_field(input, "title")
        .or_else(|| questions.first().and_then(|q| str_field(q, "header")))
        .unwrap_or_default();
    Some(Question {
        id: id.into(),
        title: title.into(),
        fields,
    })
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| str_field(block, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn is_top_level(value: &Value) -> bool {
    value["parent_tool_use_id"].is_null()
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(state: &mut State, value: Value) -> Vec<Event> {
        state.handle_line(&value.to_string()).events
    }

    fn can_use_tool(id: &str, tool: &str, input: Value) -> Value {
        json!({
            "type": "control_request",
            "request_id": id,
            "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input, "tool_use_id": "toolu_1"},
        })
    }

    #[test]
    fn cancelled_requests_are_resolved() {
        let mut state = State::new("session".into());
        let input = json!({"command": "rm -rf build", "description": "Remove the build directory"});
        let events = handle(&mut state, can_use_tool("perm-1", "Bash", input));
        let Event::PermissionRequested(permission) = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(
            permission.description.as_deref(),
            Some("Remove the build directory")
        );

        let events = handle(
            &mut state,
            json!({"type": "control_cancel_request", "request_id": "perm-1"}),
        );
        assert_eq!(
            events,
            vec![Event::PermissionResolved {
                id: "perm-1".into()
            }]
        );
        assert_eq!(state.snapshot().permission, None);
        assert_eq!(
            state
                .decide("perm-1", Decision::AllowOnce)
                .unwrap_err()
                .kind,
            ErrorKind::NotFound
        );
    }

    #[test]
    fn allowing_returns_the_original_input() {
        let mut state = State::new("session".into());
        let input = json!({"command": "cargo test"});
        handle(&mut state, can_use_tool("perm-1", "Bash", input.clone()));
        let (response, event) = state.decide("perm-1", Decision::AllowOnce).unwrap();
        assert_eq!(
            response["response"]["response"],
            json!({"behavior": "allow", "updatedInput": input})
        );
        assert_eq!(
            event,
            Event::PermissionResolved {
                id: "perm-1".into()
            }
        );
    }

    #[test]
    fn maps_extended_question_kinds() {
        let mut state = State::new("session".into());
        let input = json!({
            "title": "Project setup",
            "questions": [
                {"question": "What should it be called?", "header": "Name", "kind": "text"},
                {"question": "How many workers?", "header": "Workers", "kind": "number", "min": 1, "max": 8},
                {"question": "Pick a colour.", "header": "Colour", "kind": "colour"},
            ],
        });
        let events = handle(
            &mut state,
            can_use_tool("question-1", "AskUserQuestion", input),
        );
        let Event::QuestionAsked(question) = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(question.title, "Project setup");
        let kinds: Vec<_> = question.fields.iter().map(|field| field.kind).collect();
        assert_eq!(
            kinds,
            vec![FieldKind::Text, FieldKind::Text, FieldKind::Unsupported]
        );
        assert!(question.fields.iter().all(|field| !field.custom));

        let error = state
            .answer(
                "question-1",
                vec![("Pick a colour.".into(), AnswerValue::Text("red".into()))],
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid);

        let (response, _) = state
            .answer(
                "question-1",
                vec![(
                    "What should it be called?".into(),
                    AnswerValue::Text("Tomori".into()),
                )],
            )
            .unwrap();
        assert_eq!(
            response["response"]["response"]["updatedInput"]["answers"],
            json!({"What should it be called?": "Tomori"})
        );
    }

    #[test]
    fn malformed_questions_are_permissions() {
        let mut state = State::new("session".into());
        let events = handle(
            &mut state,
            can_use_tool("question-1", "AskUserQuestion", json!({"questions": "?"})),
        );
        assert!(
            matches!(&events[0], Event::PermissionRequested(permission) if permission.tool == "AskUserQuestion")
        );
    }

    #[test]
    fn failed_turns_report_an_error() {
        let mut state = State::new("session".into());
        handle(
            &mut state,
            json!({"type": "system", "subtype": "status", "status": "requesting"}),
        );
        let events = handle(
            &mut state,
            json!({"type": "result", "subtype": "success", "is_error": true, "result": "API Error: 529 Overloaded", "terminal_reason": "completed"}),
        );
        assert_eq!(
            events,
            vec![
                Event::Error {
                    message: "API Error: 529 Overloaded".into()
                },
                Event::Idle,
            ]
        );
    }

    fn stream(event: Value) -> Value {
        json!({"type": "stream_event", "event": event, "parent_tool_use_id": null})
    }

    fn assistant(id: &str, content: Value) -> Value {
        json!({
            "type": "assistant",
            "parent_tool_use_id": null,
            "message": {"model": "claude-haiku-5-5", "id": id, "content": content},
        })
    }

    fn text_delta(index: u64, text: &str) -> Value {
        stream(
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
        )
    }

    fn deltas(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn complete_records_do_not_repeat_streamed_text() {
        let mut state = State::new("session".into());
        let mut events = handle(
            &mut state,
            stream(json!({"type": "message_start", "message": {"id": "msg_1"}})),
        );
        for event in [
            stream(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            text_delta(0, "Hel"),
            // The record completes the block, including text that did not stream.
            assistant("msg_1", json!([{"type": "text", "text": "Hello."}])),
            // A late delta is already included.
            text_delta(0, "lo."),
            stream(json!({"type": "content_block_stop", "index": 0})),
            stream(
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            ),
            text_delta(1, " Bye."),
            // A record that disagrees with streamed text cannot retract it.
            assistant("msg_1", json!([{"type": "text", "text": " Goodbye."}])),
            text_delta(1, " Ignored."),
            // A block that did not stream is reported whole.
            assistant("msg_1", json!([{"type": "text", "text": " Other."}])),
            stream(json!({"type": "message_stop"})),
            // A record for a completed message has streamed.
            assistant("msg_1", json!([{"type": "text", "text": "Again."}])),
        ] {
            events.extend(handle(&mut state, event));
        }
        assert_eq!(deltas(&events), "Hello. Bye. Other.");
        assert_eq!(
            state.snapshot().messages[0].parts,
            vec![
                Part::Text {
                    text: "Hello.".into()
                },
                Part::Text {
                    text: " Bye.".into()
                },
                Part::Text {
                    text: " Other.".into()
                },
            ]
        );
        assert_eq!(
            events.last(),
            Some(&Event::MessageCompleted {
                message: "msg_1".into()
            })
        );
    }

    #[test]
    fn a_fallback_after_a_partial_stream_starts_a_new_message() {
        let mut state = State::new("session".into());
        let mut events = handle(
            &mut state,
            stream(json!({"type": "message_start", "message": {"id": "msg_partial"}})),
        );
        // The stream ended without a complete block, so Claude Code retried without streaming.
        events.extend(handle(
            &mut state,
            assistant(
                "msg_full",
                json!([{"type": "thinking", "thinking": "", "signature": "x"}]),
            ),
        ));
        events.extend(handle(
            &mut state,
            assistant("msg_full", json!([{"type": "text", "text": "Reply."}])),
        ));
        assert_eq!(
            state.snapshot().messages[1].parts,
            vec![Part::Text {
                text: "Reply.".into()
            }]
        );
        // Synthetic messages are not replies.
        events.extend(handle(
            &mut state,
            json!({
                "type": "assistant",
                "parent_tool_use_id": null,
                "message": {"model": "<synthetic>", "id": "msg_error", "content": [{"type": "text", "text": "API Error"}]},
            }),
        ));
        events.extend(handle(
            &mut state,
            json!({"type": "result", "subtype": "success", "is_error": false}),
        ));
        let started = |id: &str| Event::MessageStarted {
            message: id.into(),
            role: Role::Assistant,
        };
        assert_eq!(
            events,
            vec![
                Event::Busy,
                started("msg_partial"),
                Event::MessageCompleted {
                    message: "msg_partial".into()
                },
                started("msg_full"),
                Event::TextDelta {
                    message: "msg_full".into(),
                    text: "Reply.".into()
                },
                Event::MessageCompleted {
                    message: "msg_full".into()
                },
                Event::Idle,
            ]
        );
    }

    #[test]
    fn nested_workers_name_their_parent() {
        let mut state = State::new("session".into());
        let agent = |id: &str, parent: Value| {
            json!({
                "type": "assistant",
                "parent_tool_use_id": parent,
                "message": {"id": "msg_1", "content": [
                    {"type": "tool_use", "id": id, "name": "Agent", "input": {"description": "Explore", "prompt": "..."}},
                ]},
            })
        };
        handle(&mut state, agent("toolu_outer", Value::Null));
        let events = handle(&mut state, agent("toolu_inner", json!("toolu_outer")));
        // The nested tool use is not the session's own activity.
        assert_eq!(events.len(), 1);
        let Event::WorkersChanged { workers } = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(workers[0].parent.as_deref(), Some("session"));
        assert_eq!(workers[1].parent.as_deref(), Some("toolu_outer"));

        let events = handle(
            &mut state,
            json!({
                "type": "user",
                "parent_tool_use_id": "toolu_outer",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_inner", "is_error": true, "content": "failed"}]},
            }),
        );
        let Event::WorkersChanged { workers } = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(workers[1].state, WorkerState::Error);
    }
}
