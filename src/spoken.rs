//! New completed assistant text only; never replay existing session history.
use std::collections::HashSet;

#[derive(Default)]
pub struct Tracker {
    initialized: bool,
    seen: HashSet<String>,
}

impl Tracker {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn collect<'a>(
        &mut self,
        messages: impl IntoIterator<Item = (&'a str, &'a str, &'a str, bool)>,
    ) -> Vec<String> {
        let messages: Vec<_> = messages.into_iter().collect();
        if !self.initialized {
            // Including in-flight messages avoids reading historical partial
            // replies when attaching or switching to an existing session.
            self.seen
                .extend(messages.iter().map(|(id, _, _, _)| (*id).to_owned()));
            self.initialized = true;
            return vec![];
        }
        let mut output = Vec::new();
        for (id, role, text, completed) in messages {
            if role == "assistant" && completed && self.seen.insert(id.to_owned()) {
                let text = clean(text);
                if !text.is_empty() {
                    output.push(text);
                }
            }
        }
        output
    }
}

pub fn clean(text: &str) -> String {
    let mut in_code = false;
    let mut result = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let line = line
            .trim_start()
            .trim_start_matches(['#', '>', '*', '-', ' ']);
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' | '`' | '_' => {}
                '[' => {
                    // Speak a markdown link's label, not its URL.
                    let mut label = String::new();
                    for ch in chars.by_ref() {
                        if ch == ']' {
                            break;
                        }
                        label.push(ch);
                    }
                    result.push_str(&label);
                    if chars.peek() == Some(&'(') {
                        chars.next();
                        for ch in chars.by_ref() {
                            if ch == ')' {
                                break;
                            }
                        }
                    }
                }
                _ => result.push(c),
            }
        }
        result.push(' ');
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Bound each synthesis request. Split at sentence/word boundaries without
/// losing text, including for long unbroken words and multibyte characters.
pub fn chunks(text: &str, max_chars: usize) -> Vec<String> {
    assert!(max_chars > 0);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        for c in word.chars() {
            if current.chars().count() == max_chars {
                chunks.push(std::mem::take(&mut current));
            }
            current.push(c);
        }
        if word.ends_with(['.', '?', '!']) && current.chars().count() >= 40 {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_existing_and_inflight_history_but_speaks_new_completed_replies_once() {
        let mut tracker = Tracker::default();
        assert!(
            tracker
                .collect([
                    ("old", "assistant", "old text", true),
                    ("mid", "assistant", "partial", false)
                ])
                .is_empty()
        );
        assert!(
            tracker
                .collect([
                    ("mid", "assistant", "partial complete", true),
                    ("new", "assistant", "Still", false)
                ])
                .is_empty()
        );
        assert_eq!(
            tracker.collect([("new", "assistant", "New message!", true)]),
            ["New message!"]
        );
        assert!(
            tracker
                .collect([("new", "assistant", "New message!", true)])
                .is_empty()
        );
        assert!(
            tracker
                .collect([
                    ("user", "user", "User text", true),
                    ("tool", "assistant", "", true)
                ])
                .is_empty()
        );
        tracker.reset();
        assert!(
            tracker
                .collect([("new", "assistant", "New message!", true)])
                .is_empty()
        );
    }

    #[test]
    fn cleans_code_and_links_and_splits_large_unicode_text() {
        assert_eq!(
            clean("**Hello** [docs](https://x.test).\n```rs\nsecret();\n```\nDone."),
            "Hello docs. Done."
        );
        let text = format!("{} {}", "é".repeat(500), "Hello world!");
        let result = chunks(&text, 200);
        assert!(result.len() >= 3);
        assert!(result.iter().all(|s| s.chars().count() <= 200));
        assert_eq!(
            result
                .iter()
                .flat_map(|s| s.chars())
                .filter(|c| *c == 'é')
                .count(),
            500
        );
        assert_eq!(chunks("", 200), Vec::<String>::new());
    }
}
