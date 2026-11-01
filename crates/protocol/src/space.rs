// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use serde::{Deserialize, Serialize};

/// A space in the lobby listing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpaceSummary {
    pub id: String,
    pub title: String,
    pub backend: String,
    pub directory: String,
    pub activity: Activity,
    pub participants: usize,
}

/// An existing agent session a space can be bound to, reported by a backend that supports attaching.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub directory: String,
}

/// A backend the server can bind spaces to, as offered to a client so it can show only working actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub name: String,
    /// Existing sessions can be listed and attached through [`crate::ClientMessage::CreateSpace`]'s `session`.
    pub attach: bool,
}

/// A directory a space may run in, as offered to a client. A client cannot type a path, so it chooses from what the server sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRoot {
    /// The canonical path, ready to send in a `CreateSpace`.
    pub path: String,
    /// A short name for display, such as the path's final component.
    pub label: String,
}

/// The agent's observed state. `Unknown` is never treated as idle or complete, and stands for any state a newer server sends that this build does not know.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Idle,
    Running,
    /// A permission or question needs a participant's input.
    Waiting,
    /// The agent session has ended. The space keeps its state, but input is no longer accepted; `SpaceState::error` says why.
    Closed,
    #[default]
    #[serde(other)]
    Unknown,
}

/// The complete shared state of a space, identical for every participant. See `design/spaces.md`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpaceState {
    pub id: String,
    pub title: String,
    pub backend: String,
    pub directory: String,
    pub activity: Activity,
    /// Recent conversation, oldest first, projected to displayable text.
    pub messages: Vec<Message>,
    pub queue: Vec<QueueEntry>,
    pub approval: Option<Approval>,
    pub question: Option<Question>,
    pub workers: Vec<Worker>,
    pub presentations: Vec<Presentation>,
    pub participants: Vec<Participant>,
    pub attribution: bool,
    /// A space-level problem, such as a lost backend connection. Shared state is retained.
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub text: String,
    /// False while an assistant message is still streaming.
    pub completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    /// Waiting for the agent's turn to end. Can be cancelled.
    Queued,
    /// Being delivered. Can no longer be cancelled.
    Sending,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: String,
    /// The speaker's participant identity, so a client can recognize its own entries. Absent from older servers.
    #[serde(default)]
    pub participant: String,
    /// The speaker's display name.
    pub speaker: String,
    pub text: String,
    pub state: QueueState,
}

/// A pending tool permission, with prompt text prepared by the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub title: String,
    pub text: String,
    /// Choices beside allowing once and denying that the agent's own interface offers, such as "don't ask again".
    /// Chosen with [`crate::Decision::Grant`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ApprovalOption>,
}

/// A standing permission granted together with allowing the request, with its label prepared by the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOption {
    /// Passed back as the option of a [`crate::Decision::Grant`] decision.
    pub id: String,
    /// What choosing it allows and for how long, such as "Yes, and don't ask again for Bash(make) in this project".
    pub label: String,
}

/// The current field of a pending question, with prompt text prepared by the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub title: String,
    pub text: String,
    /// The field the next answer applies to.
    pub field: String,
    /// False when the question must be completed outside Tomori.
    pub supported: bool,
}

/// A worker's observed state. `Unknown` stands for any state a newer server sends that this build does not know.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Running,
    Waiting,
    Idle,
    Completed,
    Error,
    #[default]
    #[serde(other)]
    Unknown,
}

/// A delegated worker session, displayed as a station.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worker {
    pub id: String,
    /// The worker that delegated to this one, or `None` for a worker the space's own agent session started directly.
    pub parent: Option<String>,
    pub title: String,
    pub state: WorkerState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationKind {
    #[default]
    Markdown,
    Table,
}

/// A result card published by the coordinator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presentation {
    pub id: String,
    pub source_session: String,
    pub source_message: String,
    pub title: String,
    pub kind: PresentationKind,
    pub text: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub id: String,
    pub name: String,
    /// The number used as the audio frame source for this participant.
    pub source: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_list_options_only_when_offered() {
        let mut approval = Approval {
            id: "p".into(),
            title: "Bash".into(),
            text: "make".into(),
            options: Vec::new(),
        };
        let json = serde_json::to_value(&approval).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"id": "p", "title": "Bash", "text": "make"})
        );
        assert_eq!(serde_json::from_value::<Approval>(json).unwrap(), approval);

        approval.options.push(ApprovalOption {
            id: "suggestions".into(),
            label: "Yes, and don't ask again for Bash(make) in this project".into(),
        });
        let json = serde_json::to_value(&approval).unwrap();
        assert_eq!(json["options"][0]["id"], "suggestions");
        assert_eq!(serde_json::from_value::<Approval>(json).unwrap(), approval);
    }

    #[test]
    fn queue_entries_from_older_servers_have_no_participant() {
        let entry: QueueEntry = serde_json::from_value(serde_json::json!({
            "id": "e1",
            "speaker": "Ada",
            "text": "run the tests",
            "state": "queued",
        }))
        .unwrap();
        assert_eq!(entry.participant, "");
        assert_eq!(entry.speaker, "Ada");
    }

    #[test]
    fn states_a_newer_server_adds_are_read_as_unknown() {
        assert_eq!(
            serde_json::from_str::<Activity>("\"compacting\"").unwrap(),
            Activity::Unknown
        );
        assert_eq!(
            serde_json::from_str::<Activity>("\"closed\"").unwrap(),
            Activity::Closed
        );
        assert_eq!(
            serde_json::from_str::<WorkerState>("\"paused\"").unwrap(),
            WorkerState::Unknown
        );
        let summary: SpaceSummary = serde_json::from_value(serde_json::json!({
            "id": "space-1",
            "title": "Tomori",
            "backend": "fake",
            "directory": "/tmp",
            "activity": "compacting",
            "participants": 1,
        }))
        .unwrap();
        assert_eq!(summary.activity, Activity::Unknown);
    }
}
