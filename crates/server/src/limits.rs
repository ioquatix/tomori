// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Size limits that keep every server frame within [`protocol::MAX_FRAME_BYTES`]. See `design/server.md`.
//!
//! Strings supplied by clients are bounded when they arrive, and rejected with an error that does not repeat them. Space snapshots and the lobby listing are then trimmed to a byte budget when they are rendered, because they also carry text produced by the agent.

use protocol::{SpaceState, SpaceSummary};

/// Longest participant identity in `Hello`, in bytes.
pub const PARTICIPANT_BYTES: usize = 128;

/// Longest display name in `Hello`, in bytes.
pub const NAME_BYTES: usize = 64;

/// Longest space title, in bytes.
pub const TITLE_BYTES: usize = 200;

/// Longest working directory for a new space, in bytes.
pub const DIRECTORY_BYTES: usize = 4096;

/// Longest session identifier to bind, in bytes.
pub const SESSION_BYTES: usize = 256;

/// Longest client utterance identifier, in bytes.
pub const UTTERANCE_BYTES: usize = 128;

/// Longest answer text, in bytes.
pub const ANSWER_BYTES: usize = 4096;

/// Longest transcript that can be queued, in bytes. Partial transcripts are cut to this length.
pub const QUEUED_TEXT_BYTES: usize = 4096;

/// The most entries waiting in a space's queue, not counting finished ones.
pub const QUEUED_ENTRIES: usize = 16;

/// Longest error text sent to a client, in bytes. Longer errors are cut.
pub const ERROR_BYTES: usize = 512;

/// The most bytes of JSON in one space snapshot or lobby listing: a quarter of [`protocol::MAX_FRAME_BYTES`].
///
/// The oldest messages are dropped first, then the newest message text is cut, then finished queue entries, approval and question text and workers. What remains is bounded by the input limits above and the connection limit, and stays below the frame limit even in the worst case.
pub const SNAPSHOT_BUDGET: usize = protocol::MAX_FRAME_BYTES / 4;

/// Appended to text that was cut to fit.
pub const TRUNCATED: &str = "… [truncated]";

/// Check a client-supplied string against its limit. The error names the field, never the value.
pub fn check(value: &str, limit: usize, what: &str) -> Result<(), String> {
    if value.len() > limit {
        Err(format!(
            "The {what} is too long; the limit is {limit} bytes."
        ))
    } else {
        Ok(())
    }
}

/// Cut `text` to at most `limit` bytes on a character boundary, marking the cut.
pub fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub(TRUNCATED.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATED}", &text[..end])
}

/// The length of `value` as JSON.
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |json| json.len())
}

/// The bytes `character` takes inside a JSON string.
fn escaped_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        character if (character as u32) < 0x20 => 6,
        character => character.len_utf8(),
    }
}

/// Cut `text`, marking the cut, so that its JSON encoding shrinks by at least `excess` bytes. Returns the bytes saved.
fn shrink(text: &mut String, excess: usize) -> usize {
    let before = json_len(text.as_str());
    let marker = json_len(TRUNCATED) - 2;
    let allowed = (before - 2).saturating_sub(excess).saturating_sub(marker);
    let mut used = 0;
    let mut end = 0;
    for (index, character) in text.char_indices() {
        used += escaped_len(character);
        if used > allowed {
            break;
        }
        end = index + character.len_utf8();
    }
    text.truncate(end);
    text.push_str(TRUNCATED);
    before.saturating_sub(json_len(text.as_str()))
}

/// Trim a space snapshot to [`SNAPSHOT_BUDGET`].
pub fn fit_state(state: &mut SpaceState) {
    let mut size = json_len(state);
    if size <= SNAPSHOT_BUDGET {
        return;
    }
    // The oldest messages first, keeping the newest.
    while size > SNAPSHOT_BUDGET && state.messages.len() > 1 {
        let message = state.messages.remove(0);
        size -= json_len(&message) + 1;
    }
    for message in &mut state.messages {
        if size <= SNAPSHOT_BUDGET {
            break;
        }
        size -= shrink(&mut message.text, size - SNAPSHOT_BUDGET);
    }
    // Then finished queue entries, oldest first.
    while size > SNAPSHOT_BUDGET
        && let Some(index) = state.queue.iter().position(|entry| {
            matches!(
                entry.state,
                protocol::QueueState::Delivered | protocol::QueueState::Failed
            )
        })
    {
        let entry = state.queue.remove(index);
        size -= json_len(&entry) + 1;
    }
    if size > SNAPSHOT_BUDGET
        && let Some(approval) = &mut state.approval
    {
        size -= shrink(&mut approval.text, size - SNAPSHOT_BUDGET);
    }
    if size > SNAPSHOT_BUDGET
        && let Some(question) = &mut state.question
    {
        size -= shrink(&mut question.text, size - SNAPSHOT_BUDGET);
    }
    while size > SNAPSHOT_BUDGET && !state.workers.is_empty() {
        let worker = state.workers.remove(0);
        size -= json_len(&worker) + 1;
    }
}

/// Longest directory shown in the lobby listing once the listing is over budget, in bytes.
const LISTED_DIRECTORY_BYTES: usize = 120;

/// Trim the lobby listing to [`SNAPSHOT_BUDGET`]: shorten directories to their final components, then leave out closed spaces, then the oldest spaces.
pub fn fit_spaces(spaces: &mut Vec<SpaceSummary>) {
    let mut size = json_len(spaces);
    if size <= SNAPSHOT_BUDGET {
        return;
    }
    for space in spaces.iter_mut() {
        if size <= SNAPSHOT_BUDGET {
            break;
        }
        if space.directory.len() > LISTED_DIRECTORY_BYTES {
            let before = json_len(&space.directory);
            let mut start = space.directory.len() - LISTED_DIRECTORY_BYTES;
            while !space.directory.is_char_boundary(start) {
                start += 1;
            }
            space.directory = format!("…{}", &space.directory[start..]);
            size -= before.saturating_sub(json_len(&space.directory));
        }
    }
    while size > SNAPSHOT_BUDGET
        && let Some(index) = spaces
            .iter()
            .position(|space| space.activity == protocol::Activity::Closed)
    {
        let space = spaces.remove(index);
        size -= json_len(&space) + 1;
    }
    while size > SNAPSHOT_BUDGET && !spaces.is_empty() {
        let space = spaces.remove(0);
        size -= json_len(&space) + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Message, Role};

    fn message(id: usize, text: String) -> Message {
        Message {
            id: format!("m{id}"),
            role: Role::Assistant,
            text,
            completed: true,
        }
    }

    #[test]
    fn long_conversations_drop_the_oldest_messages_first() {
        let mut state = SpaceState {
            messages: (0..50).map(|id| message(id, "x".repeat(20_000))).collect(),
            ..SpaceState::default()
        };
        fit_state(&mut state);
        assert!(json_len(&state) <= SNAPSHOT_BUDGET);
        assert_eq!(state.messages.last().unwrap().id, "m49");
        assert!(state.messages.len() > 1 && state.messages.len() < 50);
        assert!(
            state
                .messages
                .iter()
                .all(|message| message.text.len() == 20_000)
        );
    }

    #[test]
    fn one_long_message_is_cut_with_a_marker() {
        // Control characters take six bytes each in JSON.
        let mut state = SpaceState {
            messages: vec![
                message(0, "old".into()),
                message(1, "\u{1}".repeat(100_000)),
            ],
            ..SpaceState::default()
        };
        fit_state(&mut state);
        let size = json_len(&state);
        assert!(size <= SNAPSHOT_BUDGET, "{size}");
        assert!(size > SNAPSHOT_BUDGET - 1024, "{size}");
        assert_eq!(state.messages.len(), 1);
        assert!(state.messages[0].text.ends_with(TRUNCATED));
    }

    fn listing(count: usize) -> Vec<SpaceSummary> {
        (0..count)
            .map(|index| SpaceSummary {
                id: format!("space-{index}"),
                title: "t".repeat(TITLE_BYTES),
                backend: "fake".into(),
                directory: "/d".repeat(DIRECTORY_BYTES / 2),
                activity: protocol::Activity::Idle,
                participants: 0,
            })
            .collect()
    }

    #[test]
    fn the_listing_stays_within_budget() {
        // Shortening directories is enough for a few hundred spaces.
        let mut spaces = listing(200);
        fit_spaces(&mut spaces);
        assert!(json_len(&spaces) <= SNAPSHOT_BUDGET);
        assert_eq!(spaces.len(), 200);
        assert!(spaces[0].directory.starts_with('…'));

        // Beyond that, closed spaces are left out, then the oldest spaces.
        let mut spaces = listing(2000);
        for space in spaces.iter_mut().skip(1000).step_by(2) {
            space.activity = protocol::Activity::Closed;
        }
        fit_spaces(&mut spaces);
        assert!(json_len(&spaces) <= SNAPSHOT_BUDGET);
        assert!(spaces.len() < 2000);
        assert_eq!(spaces.last().unwrap().id, "space-1999");
        assert!(spaces.iter().any(|space| space.id == "space-1001"));
        assert!(!spaces.iter().any(|space| space.id == "space-1000"));
    }

    #[test]
    fn checks_and_cuts_never_repeat_the_value() {
        let error = check(&"secret".repeat(100), 10, "title").unwrap_err();
        assert!(!error.contains("secret"));
        let cut = truncate(&"é".repeat(1000), ERROR_BYTES);
        assert!(cut.len() <= ERROR_BYTES && cut.ends_with(TRUNCATED));
    }
}
