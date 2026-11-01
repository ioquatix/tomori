// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Mapping between the OpenCode V2 event and resource shapes and the backend's
//! neutral vocabulary.
//!
//! The OpenCode server reports everything on one global event stream,
//! addressed by `sessionID`; these functions turn one event into the backend
//! events a single session should see, `None` when the event is not for it.
//! Resource listings are turned into permissions, questions and workers the
//! same way, so attaching to a session recovers what live events would have
//! reported.

use backend::{
    AnswerValue, Event, Field, FieldKind, FieldOption, Grant, GrantScope, Permission,
    PermissionOption, Question, Role, Worker, WorkerState,
};
use serde_json::Value;

/// The session a server event names, if it names one.
pub fn session_id_of(value: &Value) -> Option<String> {
    let data = value.get("data")?;
    data.get("sessionID")
        .or_else(|| data.get("form").and_then(|form| form.get("sessionID")))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Turn one OpenCode event into the backend events to emit for `session_id`,
/// or `None` when the event names another session. `seen_messages` records the
/// messages a session has already reported starting, so each message gets a
/// single [`Event::MessageStarted`].
pub fn event_for(
    value: &Value,
    session_id: &str,
    seen_messages: &mut std::collections::HashSet<String>,
) -> Option<Event> {
    let data = value.get("data")?;
    match value.get("type")?.as_str()? {
        "session.text.started" => first_seen(data, session_id, seen_messages),
        "session.text.delta" => text_delta(data, session_id, "delta"),
        "session.reasoning.delta" => text_delta(data, session_id, "delta"),
        // `session.text.ended` names the message that just finished. It fires
        // once, and only after the `session.text.started` that already
        // recorded this id in `seen_messages`, so it must not be gated on
        // `first_seen` (which would return `None` and swallow the completion).
        // Without `MessageCompleted` the reply's final unclosed sentence is
        // never flushed to speech.
        "session.text.ended" => {
            if !for_session(data, session_id) {
                return None;
            }
            Some(Event::MessageCompleted {
                message: message_id(data)?,
            })
        }
        "session.tool.called" => {
            first_seen(data, session_id, seen_messages)?;
            tool_started(data, session_id)
        }
        "session.tool.success" => tool_finished(data, session_id, None),
        "session.tool.failed" => tool_finished(data, session_id, Some(data)),
        "permission.asked" => permission_asked(data, session_id),
        "permission.replied" => permission_replied(data, session_id),
        "form.created" => form_created(data, session_id),
        "form.replied" | "form.cancelled" => form_resolved(data, session_id),
        "session.inbox.delivered" => inbox_delivered(data, session_id),
        "session.status" => status(data, session_id),
        "session.idle" => idle(data, session_id),
        "session.execution.started" => busy_like(data, session_id, Event::Busy),
        "session.execution.interrupted" => busy_like(data, session_id, Event::Idle),
        "session.execution.succeeded" => busy_like(data, session_id, Event::Idle),
        "session.execution.failed" => execution_failed(data, session_id),
        // Children are tracked as workers; the caller refreshes the list.
        "session.created" | "session.deleted" | "session.forked" => Some(Event::WorkersChanged {
            workers: Vec::new(),
        }),
        _ => None,
    }
}

fn for_session(data: &Value, session_id: &str) -> bool {
    data.get("sessionID").and_then(Value::as_str) == Some(session_id)
}

/// Report the message before anything that names it, at most once.
fn first_seen(
    data: &Value,
    session_id: &str,
    seen_messages: &mut std::collections::HashSet<String>,
) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    let message = message_id(data)?;
    if !seen_messages.insert(message.clone()) {
        return None;
    }
    Some(Event::MessageStarted {
        message,
        role: Role::Assistant,
    })
}

fn text_delta(data: &Value, session_id: &str, field: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    Some(Event::TextDelta {
        message: message_id(data)?,
        text: data.get(field)?.as_str()?.to_owned(),
    })
}

fn tool_started(data: &Value, session_id: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    Some(Event::ToolStarted {
        message: message_id(data)?,
        // The V2 stream does not name the tool on the call event; the history
        // does, and the id is what the completion events are matched by.
        tool: String::new(),
        id: data.get("id")?.as_str()?.to_owned(),
        input: data.get("input").cloned().unwrap_or(Value::Null),
    })
}

fn tool_finished(data: &Value, session_id: &str, failed: Option<&Value>) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    let error = failed.and_then(|data| {
        data.get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    Some(Event::ToolFinished {
        id: data.get("id")?.as_str()?.to_owned(),
        error,
    })
}

/// A permission request object (from the list endpoint or the event stream's
/// `data`) as a [`Permission`].
pub fn permission_from_request(request: &Value) -> Option<Permission> {
    Some(Permission {
        id: request.get("id")?.as_str()?.to_owned(),
        tool: request.get("action")?.as_str()?.to_owned(),
        input: Value::Object(
            request
                .get("resources")
                .and_then(Value::as_array)
                .map(|resources| {
                    resources
                        .iter()
                        .enumerate()
                        .map(|(index, resource)| (index.to_string(), resource.clone()))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        description: request
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned),
        options: request
            .get("save")
            .and_then(Value::as_array)
            .map(|save| {
                save.iter()
                    .enumerate()
                    .filter_map(|(index, rule)| {
                        let rule = rule.as_str()?;
                        let (tool, content) = rule.split_once(' ')?;
                        Some(PermissionOption {
                            id: index.to_string(),
                            grants: vec![Grant::Rule {
                                tool: tool.to_owned(),
                                content: Some(content.to_owned()),
                                scope: GrantScope::Session,
                            }],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn permission_asked(data: &Value, session_id: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    permission_from_request(data).map(Event::PermissionRequested)
}

fn permission_replied(data: &Value, session_id: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    Some(Event::PermissionResolved {
        id: data.get("requestID")?.as_str()?.to_owned(),
    })
}

fn form_created(data: &Value, session_id: &str) -> Option<Event> {
    let form = data.get("form")?;
    if form.get("sessionID").and_then(Value::as_str) != Some(session_id) {
        return None;
    }
    Some(Event::QuestionAsked(question_from_form(form)))
}

fn form_resolved(data: &Value, session_id: &str) -> Option<Event> {
    let (id, session) = match data.get("id").and_then(Value::as_str) {
        Some(id) => (
            id.to_owned(),
            data.get("sessionID").and_then(Value::as_str)?.to_owned(),
        ),
        None => {
            let form = data.get("form")?;
            (
                form.get("id")?.as_str()?.to_owned(),
                form.get("sessionID")?.as_str()?.to_owned(),
            )
        }
    };
    if session != session_id {
        return None;
    }
    Some(Event::QuestionResolved { id })
}

fn inbox_delivered(data: &Value, session_id: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    // The `inboxID` is the `msg_`-prefixed id we sent in `send_prompt`.
    // Strip the prefix to recover the server's delivery key.
    let inbox_id = data.get("inboxID")?.as_str()?;
    let key = inbox_id.strip_prefix("msg_").unwrap_or(inbox_id);
    Some(Event::Delivered {
        key: key.to_owned(),
    })
}

fn status(data: &Value, session_id: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    if data.get("status")?.get("type")?.as_str()? == "busy" {
        Some(Event::Busy)
    } else {
        None
    }
}

fn idle(data: &Value, session_id: &str) -> Option<Event> {
    for_session(data, session_id).then_some(Event::Idle)
}

fn busy_like(data: &Value, session_id: &str, event: Event) -> Option<Event> {
    for_session(data, session_id).then_some(event)
}

fn execution_failed(data: &Value, session_id: &str) -> Option<Event> {
    if !for_session(data, session_id) {
        return None;
    }
    Some(Event::Error {
        message: data
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("the agent execution failed")
            .to_owned(),
    })
}

fn message_id(data: &Value) -> Option<String> {
    data.get("assistantMessageID")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A form as a structured question.
pub fn question_from_form(form: &Value) -> Question {
    let fields = form
        .get("fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(field_from_form_field)
                .collect::<Vec<Field>>()
        })
        .unwrap_or_default();
    Question {
        id: form
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        title: form
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        fields,
    }
}

fn field_from_form_field(field: &Value) -> Option<Field> {
    let key = field.get("key")?.as_str()?.to_owned();
    let (kind, options) = match field.get("type")?.as_str()? {
        "string" => {
            let options = field
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if options.is_empty() {
                (FieldKind::Text, Vec::new())
            } else {
                (FieldKind::Choice, options)
            }
        }
        "number" | "integer" => (FieldKind::Text, Vec::new()),
        "boolean" => (
            FieldKind::Choice,
            vec![Value::String("true".into()), Value::String("false".into())],
        ),
        "multiselect" => (
            FieldKind::MultipleChoice,
            field
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        ),
        // Completed outside Tomori, in the linked tool.
        "external" => (FieldKind::Unsupported, Vec::new()),
        _ => (FieldKind::Unsupported, Vec::new()),
    };
    let options = field_options(&options);
    Some(Field {
        key: key.clone(),
        title: field
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|title| !title.is_empty())
            .unwrap_or(key),
        description: field
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        kind,
        options,
        custom: field
            .get("custom")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn field_options(values: &[Value]) -> Vec<FieldOption> {
    values
        .iter()
        .map(|option| {
            let label = if let Some(label) = option.as_str() {
                label.to_owned()
            } else {
                option
                    .get("label")
                    .and_then(Value::as_str)
                    .map_or_else(|| option.to_string(), str::to_owned)
            };
            let description = option
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            FieldOption { label, description }
        })
        .collect()
}

/// A child session as a delegated worker.
pub fn worker_from_session(info: &Value) -> Option<Worker> {
    Some(Worker {
        id: info.get("id")?.as_str()?.to_owned(),
        parent: info
            .get("parentID")
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: info
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("worker")
            .to_owned(),
        state: match info.get("outcome").and_then(Value::as_str) {
            Some("succeeded") => WorkerState::Completed,
            Some("failed") => WorkerState::Error,
            Some("interrupted") => WorkerState::Unknown,
            _ => WorkerState::Running,
        },
    })
}

/// The answers to a question, in the OpenCode reply's shape: an object
/// mapping field keys to strings, arrays of strings, numbers or booleans.
pub fn form_answer(answers: &[(String, AnswerValue)]) -> Value {
    let mut object = serde_json::Map::new();
    for (key, value) in answers {
        let value = match value {
            AnswerValue::Text(text) => Value::String(text.clone()),
            AnswerValue::Choices(choices) => Value::Array(
                choices
                    .iter()
                    .map(|choice| Value::String(choice.clone()))
                    .collect(),
            ),
        };
        object.insert(key.clone(), value);
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        value: &Value,
        session_id: &str,
        seen: &mut std::collections::HashSet<String>,
    ) -> Option<Event> {
        event_for(value, session_id, seen)
    }

    /// Regression: `session.text.ended` must emit `MessageCompleted` even
    /// though the matching `session.text.started` already recorded the message
    /// in `seen_messages`. Previously the arm was gated on `first_seen`, which
    /// returned `None` for an already-seen id and swallowed the completion — so
    /// the reply's final unclosed sentence was never flushed to speech.
    #[test]
    fn text_ended_emits_message_completed_after_text_started() {
        let session = "ses_test";
        let mut seen = std::collections::HashSet::new();

        let started = serde_json::json!({
            "type": "session.text.started",
            "data": {
                "sessionID": session,
                "assistantMessageID": "msg_1",
                "ordinal": 0
            }
        });
        assert!(
            matches!(event(&started, session, &mut seen), Some(Event::MessageStarted { message, role }) if message == "msg_1" && role == Role::Assistant)
        );

        let delta = serde_json::json!({
            "type": "session.text.delta",
            "data": {
                "sessionID": session,
                "assistantMessageID": "msg_1",
                "delta": "pong"
            }
        });
        assert!(
            matches!(event(&delta, session, &mut seen), Some(Event::TextDelta { message, text }) if message == "msg_1" && text == "pong")
        );

        let ended = serde_json::json!({
            "type": "session.text.ended",
            "data": {
                "sessionID": session,
                "assistantMessageID": "msg_1",
                "delta": "pong"
            }
        });
        assert!(
            matches!(event(&ended, session, &mut seen), Some(Event::MessageCompleted { message }) if message == "msg_1"),
            "text.ended must complete the message it names"
        );
    }
}
