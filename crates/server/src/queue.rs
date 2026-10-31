// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;

use protocol::{QueueEntry, QueueState};

/// Finished entries kept visible after delivery or failure.
const FINISHED_LIMIT: usize = 8;

/// Keys remembered after queueing, so that a repeated finalization is recognized. More than the queue holds (`limits::QUEUED_ENTRIES` waiting plus [`FINISHED_LIMIT`] finished), so a key is remembered at least as long as its entry is visible, and a client retrying after a reconnect has many utterances' grace.
const RECENT_KEYS: usize = 32;

/// A space's input queue: entries in finalization order, delivered one at a time. See `design/spaces.md`.
#[derive(Debug, Default)]
pub struct InputQueue {
    entries: Vec<QueueEntry>,
    /// The most recently queued keys with their text, oldest first, so a repeated finalization is not delivered twice. Bounded by [`RECENT_KEYS`].
    recent: VecDeque<(String, String)>,
}

impl InputQueue {
    pub fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }

    /// Entries that are queued or sending.
    pub fn waiting(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.state, QueueState::Queued | QueueState::Sending))
            .count()
    }

    /// The text queued under `key`, if it was queued recently.
    pub fn queued_text(&self, key: &str) -> Option<&str> {
        self.recent
            .iter()
            .find(|(queued, _)| queued == key)
            .map(|(_, text)| text.as_str())
    }

    /// Add an entry for the participant `participant`, shown as spoken by `speaker`. Returns false if an entry with this key was queued recently, in which case nothing changes.
    pub fn push(
        &mut self,
        key: String,
        participant: String,
        speaker: String,
        text: String,
    ) -> bool {
        if self.queued_text(&key).is_some() {
            return false;
        }
        if self.recent.len() == RECENT_KEYS {
            self.recent.pop_front();
        }
        self.recent.push_back((key.clone(), text.clone()));
        self.entries.push(QueueEntry {
            id: key,
            participant,
            speaker,
            text,
            state: QueueState::Queued,
        });
        true
    }

    /// Remove a queued entry. Entries that are sending or finished cannot be cancelled.
    pub fn cancel(&mut self, key: &str) -> Result<(), &'static str> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == key)
            .ok_or("That queued input no longer exists.")?;
        if self.entries[index].state != QueueState::Queued {
            return Err("That input was already sent to the agent.");
        }
        self.entries.remove(index);
        Ok(())
    }

    pub fn sending(&self) -> Option<&QueueEntry> {
        self.entries
            .iter()
            .find(|entry| entry.state == QueueState::Sending)
    }

    /// Mark the oldest queued entry as sending and return it, unless an entry is already sending.
    pub fn begin_next(&mut self) -> Option<&QueueEntry> {
        if self.sending().is_some() {
            return None;
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.state == QueueState::Queued)?;
        entry.state = QueueState::Sending;
        Some(entry)
    }

    /// Fail every entry that is still queued, such as when the session has closed.
    pub fn fail_waiting(&mut self) {
        for entry in &mut self.entries {
            if entry.state == QueueState::Queued {
                entry.state = QueueState::Failed;
            }
        }
        self.trim();
    }

    /// Finish the sending entry with `key`. Returns the entry if it was sending.
    pub fn finish(&mut self, key: &str, state: QueueState) -> Option<QueueEntry> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == key && entry.state == QueueState::Sending)?;
        entry.state = state;
        let entry = entry.clone();
        self.trim();
        Some(entry)
    }

    fn trim(&mut self) {
        let finished =
            |entry: &QueueEntry| matches!(entry.state, QueueState::Delivered | QueueState::Failed);
        let mut excess = self
            .entries
            .iter()
            .filter(|entry| finished(entry))
            .count()
            .saturating_sub(FINISHED_LIMIT);
        self.entries.retain(|entry| {
            if excess > 0 && finished(entry) {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_delivered_one_at_a_time_in_order() {
        let mut queue = InputQueue::default();
        assert!(queue.push("a".into(), "ada".into(), "Ada".into(), "first".into()));
        assert!(queue.push("b".into(), "bob".into(), "Bob".into(), "second".into()));
        assert!(!queue.push("a".into(), "ada".into(), "Ada".into(), "again".into()));
        assert_eq!(queue.begin_next().unwrap().id, "a");
        assert!(queue.begin_next().is_none());
        assert_eq!(
            queue.cancel("a"),
            Err("That input was already sent to the agent.")
        );
        assert!(queue.finish("a", QueueState::Delivered).is_some());
        assert!(queue.finish("a", QueueState::Delivered).is_none());
        queue.cancel("b").unwrap();
        assert!(queue.begin_next().is_none());
        assert_eq!(queue.entries().len(), 1);
    }

    #[test]
    fn finished_entries_are_trimmed() {
        let mut queue = InputQueue::default();
        for index in 0..20 {
            let key = format!("k{index}");
            queue.push(key.clone(), "ada".into(), "Ada".into(), "text".into());
            queue.begin_next();
            queue.finish(&key, QueueState::Delivered);
        }
        assert_eq!(queue.entries().len(), FINISHED_LIMIT);
        assert_eq!(queue.entries()[0].id, "k12");
        assert!(!queue.push("k19".into(), "ada".into(), "Ada".into(), "text".into()));
    }

    #[test]
    fn remembered_keys_are_bounded() {
        let mut queue = InputQueue::default();
        for index in 0..RECENT_KEYS * 4 {
            let key = format!("k{index}");
            assert!(queue.push(
                key.clone(),
                "ada".into(),
                "Ada".into(),
                format!("text {index}")
            ));
            queue.begin_next();
            queue.finish(&key, QueueState::Delivered);
        }
        assert_eq!(queue.recent.len(), RECENT_KEYS);
        let last = RECENT_KEYS * 4 - 1;
        assert_eq!(
            queue.queued_text(&format!("k{last}")),
            Some(format!("text {last}").as_str())
        );
        // Every visible entry is still recognized.
        for entry in queue.entries() {
            assert!(queue.queued_text(&entry.id).is_some());
        }
        assert_eq!(queue.queued_text("k0"), None);
    }
}
