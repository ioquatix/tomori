// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Select speakable sentences from one assistant message as it streams.
//!
//! Feed the message's Markdown deltas to [`Speakable::push`] and call [`Speakable::finish`] when the message completes. Each returns the sentences that became stable, in order; every sentence is returned exactly once. See `design/speech.md`.
//!
//! Paragraph, heading and list item text is spoken, with emphasis markers removed and links read by their text. Code blocks, tables, raw HTML, images, footnotes and bare URLs are not. Text whose Markdown structure may still change, such as a line that could become a table header or an unclosed code span or link, is held back until it is stable.
//!
//! Streaming produces the same sentences as feeding the complete message at once, except in these cases, where already spoken text cannot be retracted:
//!
//! - A reference or footnote definition that arrives after its use: the use is spoken before it resolves. Leftover `[label]` and `[^note]` syntax is removed, so this usually reads the same.
//! - A line without `|` that is later followed by a table delimiter row: sentences already spoken from it are not withdrawn when it becomes the table header.
//! - A capitalised email address that is still arriving, which can confirm a sentence boundary before it is removed as a raw address.

mod blocks;
mod sentence;

use std::borrow::Cow;

use sentence::Next;

/// Separates the carried definitions from the text being parsed. A line at the left margin closes any footnote definition, so indented text after it is not taken as the definition's continuation; the comment itself is an HTML block that is never spoken.
const PRELUDE_END: &str = "<!-- -->\n\n";

/// Options for [`Speakable`].
#[derive(Clone, Debug)]
pub struct SpeakableOptions {
    /// Spoken in place of each code block, for example "Code omitted.". Nothing is said when `None`.
    pub code_notice: Option<String>,
    /// Sentences longer than this many bytes are split at a clause or word break, so that synthesis can start sooner and stays within engine limits.
    pub maximum_segment: usize,
}

impl Default for SpeakableOptions {
    fn default() -> Self {
        Self {
            code_notice: None,
            maximum_segment: 400,
        }
    }
}

/// Where speaking has reached: a block, identified by its source start, and a byte offset within its text.
#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    block: usize,
    offset: usize,
}

/// The speakable filter and sentence segmenter for one assistant message.
///
/// Each delta re-parses only the text after the last top-level boundary that has been spoken, so streaming a message costs time proportional to its length, apart from top-level blocks that span blank lines (see `blocks`).
#[derive(Debug, Default)]
pub struct Speakable {
    source: String,
    options: SpeakableOptions,
    cursor: Option<Cursor>,
    finished: bool,
    /// The source offset parsing starts from. Everything before it has been spoken and can no longer change.
    origin: usize,
    /// Reference and footnote definitions before `origin`, each followed by a blank line, parsed ahead of the text so that later uses still resolve.
    definitions: String,
    /// Parse the whole message on every delta, as an oracle for tests.
    #[cfg(test)]
    whole: bool,
    /// The number of bytes parsed so far.
    #[cfg(test)]
    parsed: usize,
}

impl Speakable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: SpeakableOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// The Markdown received so far.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Append a delta of the message's Markdown and return the sentences that became stable.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        assert!(!self.finished, "the message has already finished");
        self.source.push_str(delta);
        self.collect()
    }

    /// Mark the message complete and return all remaining sentences.
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        self.collect()
    }

    fn collect(&mut self) -> Vec<String> {
        let origin = self.origin;
        let text: Cow<str> = if origin == 0 {
            Cow::Borrowed(&self.source)
        } else {
            let mut text = String::with_capacity(
                self.definitions.len() + PRELUDE_END.len() + self.source.len() - origin,
            );
            if !self.definitions.is_empty() {
                text.push_str(&self.definitions);
                text.push_str(PRELUDE_END);
            }
            text.push_str(&self.source[origin..]);
            Cow::Owned(text)
        };
        // Offsets in `text` from `prelude` on correspond to source offsets from `origin` on.
        let prelude = text.len() - (self.source.len() - origin);
        let to_source = |offset: usize| offset - prelude + origin;
        #[cfg(test)]
        {
            self.parsed += text.len();
        }
        let analysis = blocks::analyse(&text, self.finished, self.options.code_notice.as_deref());
        let mut segments = Vec::new();
        for block in analysis
            .blocks
            .iter()
            .filter(|block| block.start >= prelude)
        {
            let start = to_source(block.start);
            let mut offset = match self.cursor {
                Some(cursor) if start < cursor.block => continue,
                Some(cursor) if start == cursor.block => cursor.offset.min(block.text.len()),
                _ => 0,
            };
            // Clamp to a character boundary in case the block's text changed shape.
            while !block.text.is_char_boundary(offset) {
                offset -= 1;
            }
            let available = if block.complete {
                block.text.len()
            } else {
                block.stable.max(offset)
            };
            while let Next::Sentence { start, end, resume } = sentence::next(
                &block.text,
                offset,
                available,
                block.complete,
                self.options.maximum_segment,
                &block.protected,
            ) {
                if let Some(segment) = normalize(&block.text[start..end]) {
                    segments.push(segment);
                }
                offset = resume;
            }
            // A block's text can shrink temporarily, for example while a trailing line looks like a table header; never move back over text that was already spoken.
            let offset = match self.cursor {
                Some(cursor) if cursor.block == start => offset.max(cursor.offset),
                _ => offset,
            };
            self.cursor = Some(Cursor {
                block: start,
                offset,
            });
            if !block.complete {
                break;
            }
        }

        // Move the origin to the last boundary at or before the block being spoken: every block before it is complete and has been spoken.
        #[cfg(test)]
        let whole = self.whole;
        #[cfg(not(test))]
        let whole = false;
        if let Some(cursor) = self.cursor
            && !self.finished
            && !whole
            && let Some(boundary) = analysis
                .boundaries
                .iter()
                .filter(|&&boundary| boundary >= prelude)
                .map(|&boundary| to_source(boundary))
                .rfind(|&boundary| boundary > origin && boundary <= cursor.block)
        {
            for range in &analysis.definitions {
                if range.start >= prelude && to_source(range.start) < boundary {
                    self.definitions.push_str(text[range.clone()].trim_end());
                    self.definitions.push_str("\n\n");
                }
            }
            self.origin = boundary;
        }
        segments
    }
}

/// Collapse whitespace and remove leftover Markdown markers. Returns `None` when nothing pronounceable remains.
///
/// Footnote and reference links whose definitions have not arrived yet remain literal while streaming but resolve in a complete message, so `[^note]` and the label of `[text][label]` are removed and other brackets dropped; both forms then read the same.
fn normalize(text: &str) -> Option<String> {
    let characters: Vec<char> = strip_references(text).chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut space = false;
    for (index, &character) in characters.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| characters[index]);
        let next = characters.get(index + 1).copied();
        let keep = match character {
            '*' | '~' | '`' => false,
            '_' => {
                previous.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && next.is_some_and(|c| c.is_alphanumeric() || c == '_')
            }
            _ => true,
        };
        if !keep {
            continue;
        }
        if character.is_whitespace() {
            space = !output.is_empty();
            continue;
        }
        if space {
            output.push(' ');
            space = false;
        }
        output.push(character);
    }
    output.chars().any(char::is_alphanumeric).then_some(output)
}

fn strip_references(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('[') {
        let label = &rest[index..];
        let footnote = label.starts_with("[^");
        let reference = rest[..index].ends_with(']');
        output.push_str(&rest[..index]);
        match label.find(']') {
            Some(close) if footnote || reference => rest = &label[close + 1..],
            _ => rest = &label[1..],
        }
    }
    output.push_str(rest);
    output.replace(']', "")
}

#[cfg(test)]
mod tests;
