// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use serde::{Deserialize, Serialize};

use crate::{Gate, SpaceState, SpaceSummary};

/// A tracked pose in the space's coordinate system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    pub position: [f32; 3],
    /// A unit quaternion `[x, y, z, w]`.
    pub orientation: [f32; 4],
}

/// A participant's decision on a pending tool permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    AllowOnce,
    /// Allow, and grant the approval's [`crate::ApprovalOption`] named by the decision's `option`, with the same effect
    /// as choosing it in the agent's own interface.
    Grant,
    Deny,
}

/// Control messages sent by a client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// The first message on every connection.
    Hello {
        protocol: u32,
        /// A stable identity chosen by the client, so a reconnect is recognized as the same participant.
        participant: String,
        /// The display name used for speaker attribution.
        name: String,
        /// Reserved for future authentication. Ignored by servers that do not authenticate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials: Option<String>,
    },
    ListSpaces,
    /// Create a space bound to a new session, or to an existing `session` where the backend supports binding.
    CreateSpace {
        title: String,
        backend: String,
        directory: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// Join a space, leaving any other space.
    Join {
        space: String,
    },
    Leave,
    /// Change where this client's microphone audio goes. Applies to subsequent audio frames.
    Gate {
        gate: Gate,
    },
    /// Mark the start of push-to-talk audio for the agent. Audio frames with [`Gate::Agent`] belong to the open utterance.
    UtteranceBegin {
        utterance: String,
    },
    /// Finalize the utterance; the server finishes recognition and queues the transcript.
    UtteranceEnd {
        utterance: String,
    },
    UtteranceCancel {
        utterance: String,
    },
    /// Cancel a queued entry before it is sent to the agent. Any participant may cancel any entry.
    CancelQueued {
        entry: String,
    },
    Decide {
        request: String,
        decision: Decision,
        /// The id of the approval option to grant. Required with [`Decision::Grant`] and refused with any other decision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option: Option<String>,
    },
    /// Answer the current field of a pending question with spoken or typed text.
    Answer {
        question: String,
        field: String,
        text: String,
    },
    Presence {
        head: Pose,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        left: Option<Pose>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        right: Option<Pose>,
    },
    /// Turn speaker attribution on or off for the current space.
    SetAttribution {
        enabled: bool,
    },
    /// Sent when nothing has been received for the keepalive interval. Valid before Hello. See [`crate::Keepalive`].
    Ping {
        nonce: u64,
    },
    /// The immediate answer to a [`ServerMessage::Ping`], echoing its nonce.
    Pong {
        nonce: u64,
    },
}

/// Control messages sent by the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        protocol: u32,
        server: String,
        /// The participant number used as [`crate::AudioFrame::source`] for this client's forwarded audio.
        source: u32,
    },
    Spaces {
        spaces: Vec<SpaceSummary>,
    },
    /// The complete state of the joined space. Sent on join and whenever it changes.
    Space {
        state: Box<SpaceState>,
    },
    Left,
    Transcript {
        utterance: String,
        text: String,
        /// True once the utterance is finalized and queued.
        #[serde(rename = "final")]
        finished: bool,
    },
    /// Recognition could not produce a transcript; nothing was queued.
    TranscriptFailed {
        utterance: String,
        error: String,
    },
    /// Announces the agent speech audio frames that follow, which use [`crate::AGENT_SOURCE`].
    Speech {
        segment: u32,
        message: String,
        text: String,
        sample_rate: u32,
    },
    Presence {
        source: u32,
        head: Pose,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        left: Option<Pose>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        right: Option<Pose>,
    },
    /// A request could not be completed. The connection stays open.
    Error {
        error: String,
        /// The space a refused `Join` named, so that a client can tell its join was refused rather than some other request. Absent for other errors, and from older servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<String>,
    },
    /// Sent when nothing has been received for the keepalive interval. See [`crate::Keepalive`].
    Ping {
        nonce: u64,
    },
    /// The immediate answer to a [`ClientMessage::Ping`], echoing its nonce.
    Pong {
        nonce: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_use_a_snake_case_type_tag() {
        let json = serde_json::to_value(ClientMessage::Decide {
            request: "per_1".into(),
            decision: Decision::AllowOnce,
            option: None,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "decide", "request": "per_1", "decision": "allow_once"})
        );
        let grant = serde_json::json!({"type": "decide", "request": "per_1", "decision": "grant", "option": "suggestions"});
        assert_eq!(
            serde_json::from_value::<ClientMessage>(grant).unwrap(),
            ClientMessage::Decide {
                request: "per_1".into(),
                decision: Decision::Grant,
                option: Some("suggestions".into()),
            }
        );

        let json = serde_json::to_value(ServerMessage::Transcript {
            utterance: "u".into(),
            text: "hello".into(),
            finished: true,
        })
        .unwrap();
        assert_eq!(json["final"], true);
    }

    #[test]
    fn keepalive_messages_are_symmetric() {
        let ping = serde_json::json!({"type": "ping", "nonce": 7});
        let pong = serde_json::json!({"type": "pong", "nonce": 7});
        assert_eq!(
            serde_json::to_value(ClientMessage::Ping { nonce: 7 }).unwrap(),
            ping
        );
        assert_eq!(
            serde_json::to_value(ServerMessage::Ping { nonce: 7 }).unwrap(),
            ping
        );
        assert_eq!(
            serde_json::to_value(ClientMessage::Pong { nonce: 7 }).unwrap(),
            pong
        );
        assert_eq!(
            serde_json::to_value(ServerMessage::Pong { nonce: 7 }).unwrap(),
            pong
        );
        assert_eq!(
            serde_json::from_value::<ClientMessage>(ping.clone()).unwrap(),
            ClientMessage::Ping { nonce: 7 }
        );
        assert_eq!(
            serde_json::from_value::<ServerMessage>(pong).unwrap(),
            ServerMessage::Pong { nonce: 7 }
        );
        // The largest nonce survives a round trip through the frame encoding.
        let frame = crate::Frame::server(&ServerMessage::Ping { nonce: u64::MAX });
        assert_eq!(
            frame.to_server_message().unwrap(),
            ServerMessage::Ping { nonce: u64::MAX }
        );
    }

    #[test]
    fn hello_credentials_are_optional() {
        let message: ClientMessage = serde_json::from_str(
            r#"{"type": "hello", "protocol": 1, "participant": "p", "name": "Ada"}"#,
        )
        .unwrap();
        assert!(matches!(
            message,
            ClientMessage::Hello {
                credentials: None,
                ..
            }
        ));
    }
}
