// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::HashSet;

use protocol::{QueueEntry, QueueState};

/// Finished entries kept visible after delivery or failure.
const FINISHED_LIMIT: usize = 8;

/// A space's input queue: entries in finalization order, delivered one at a time. See `design/spaces.md`.
#[derive(Debug, Default)]
pub struct InputQueue {
    entries: Vec<QueueEntry>,
    /// Every key ever queued, so a repeated finalization is not delivered twice.
    keys: HashSet<String>,
}

impl InputQueue {
    pub fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }

    /// Add an entry. Returns false if an entry with this key was already queued, in which case nothing changes.
    pub fn push(&mut self, key: String, speaker: String, text: String) -> bool {
        if !self.keys.insert(key.clone()) {
            return false;
        }
        self.entries.push(QueueEntry {
            id: key,
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
        assert!(queue.push("a".into(), "Ada".into(), "first".into()));
        assert!(queue.push("b".into(), "Bob".into(), "second".into()));
        assert!(!queue.push("a".into(), "Ada".into(), "again".into()));
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
            queue.push(key.clone(), "Ada".into(), "text".into());
            queue.begin_next();
            queue.finish(&key, QueueState::Delivered);
        }
        assert_eq!(queue.entries().len(), FINISHED_LIMIT);
        assert_eq!(queue.entries()[0].id, "k12");
        assert!(!queue.push("k0".into(), "Ada".into(), "text".into()));
    }
}
