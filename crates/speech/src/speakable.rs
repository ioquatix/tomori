// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Select speakable sentences from one assistant message as it streams.
//!
//! Feed the message's Markdown deltas to [`Speakable::push`] and call [`Speakable::finish`] when the message completes. Each returns the document-level [`Block`]s that closed, in order, each with its sentences to speak; every block is returned exactly once. See `design/speech.md`.
//!
//! A block is returned only once no later text can change it: base-markdown's [`IncrementalParser`] emits a document-level block when it and every block before it are closed. A paragraph is therefore spoken once a blank line or the next block follows it, and a list or block quote once it ends, all at once. Tables, code blocks and lists arrive whole, with their events, so that a later step can show or run them as well as speak them.
//!
//! Paragraph, heading and list item text is spoken, with emphasis markers removed and links read by their text. Code blocks, tables, raw HTML, images, footnotes and bare URLs are not.
//!
//! How the message is divided into deltas never changes the result. A reference or footnote definition applies only to uses in its own or a later document-level block: an earlier use remains literal, and its leftover `[label]` or `[^note]` syntax is removed, so it usually reads the same.

mod blocks;
mod sentence;

use std::{ops::Range, sync::LazyLock};

use base_markdown::{
    IncrementalParser, ParseOptions,
    stream::{Event, EventKind, Tag},
};

/// GitHub Flavored Markdown, as assistant replies are written.
static PARSE_OPTIONS: LazyLock<ParseOptions> = LazyLock::new(ParseOptions::gfm);

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

/// What kind of document-level block a [`Block`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockKind {
    Paragraph,
    /// An ATX or setext heading, with its level.
    Heading(u8),
    BlockQuote,
    /// A list with all of its items.
    List,
    Table,
    /// A fenced or indented code block, or a math block.
    Code,
    /// A raw HTML block.
    Html,
    /// A thematic break.
    Rule,
    /// A link reference definition.
    Definition,
    FootnoteDefinition,
    /// Anything else, such as frontmatter.
    Other,
}

/// One closed document-level block of a message: its complete structure and what is spoken for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    /// The block's byte range in [`Speakable::source`].
    pub span: Range<usize>,
    /// The block's events, from its start to its end, including nested blocks. Text values refer to [`Speakable::source`].
    pub events: Vec<Event<'static>>,
    /// The sentences to speak for the block, in order; empty when nothing in it is spoken.
    pub sentences: Vec<String>,
}

impl Block {
    /// The block's Markdown, given [`Speakable::source`].
    pub fn source<'a>(&self, source: &'a str) -> &'a str {
        &source[self.span.clone()]
    }
}

/// The sentences of `blocks`, in order.
pub fn sentences(blocks: impl IntoIterator<Item = Block>) -> impl Iterator<Item = String> {
    blocks.into_iter().flat_map(|block| block.sentences)
}

/// The speakable filter and sentence segmenter for one assistant message.
#[derive(Debug)]
pub struct Speakable {
    parser: IncrementalParser<'static>,
    options: SpeakableOptions,
    finished: bool,
    /// Events of a document-level block that has started but not yet ended.
    events: Vec<Event<'static>>,
    /// Depth of open containers within `events`.
    depth: usize,
}

impl Default for Speakable {
    fn default() -> Self {
        Self::with_options(SpeakableOptions::default())
    }
}

impl Speakable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(options: SpeakableOptions) -> Self {
        Self {
            parser: IncrementalParser::new(&PARSE_OPTIONS),
            options,
            finished: false,
            events: Vec::new(),
            depth: 0,
        }
    }

    /// The Markdown received so far, which the spans and events of returned blocks refer to.
    pub fn source(&self) -> &str {
        self.parser.source()
    }

    /// Append a delta of the message's Markdown and return the blocks that closed.
    ///
    /// # Panics
    ///
    /// Panics if called after [`Speakable::finish`].
    pub fn push(&mut self, delta: &str) -> Vec<Block> {
        assert!(!self.finished, "the message has already finished");
        let events: Vec<_> = self.parser.push(delta).collect();
        self.collect(events)
    }

    /// Mark the message complete and return all remaining blocks. Later calls return nothing.
    pub fn finish(&mut self) -> Vec<Block> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let events: Vec<_> = self.parser.finish().collect();
        self.collect(events)
    }

    fn collect(&mut self, events: Vec<Event<'static>>) -> Vec<Block> {
        let mut blocks = Vec::new();
        for event in events {
            // Separators between document-level blocks belong to neither.
            if self.depth == 0 && matches!(event.kind, EventKind::Separator { .. }) {
                continue;
            }
            match event.kind {
                EventKind::Start(_) => self.depth += 1,
                EventKind::End(_) => self.depth -= 1,
                _ => {}
            }
            self.events.push(event);
            if self.depth == 0 {
                let events = std::mem::take(&mut self.events);
                blocks.push(self.block(events));
            }
        }
        blocks
    }

    fn block(&self, events: Vec<Event<'static>>) -> Block {
        let first = &events[0];
        let kind = match &first.kind {
            EventKind::Start(Tag::Paragraph { .. }) => BlockKind::Paragraph,
            EventKind::Start(Tag::Heading(level)) => BlockKind::Heading(*level),
            EventKind::Start(Tag::BlockQuote) => BlockKind::BlockQuote,
            EventKind::Start(Tag::List { .. }) => BlockKind::List,
            EventKind::Start(Tag::Table) => BlockKind::Table,
            EventKind::Start(Tag::FootnoteDefinition(_)) => BlockKind::FootnoteDefinition,
            EventKind::CodeBlock { .. } => BlockKind::Code,
            EventKind::Html { .. } => BlockKind::Html,
            EventKind::Rule => BlockKind::Rule,
            EventKind::Definition { .. } => BlockKind::Definition,
            _ => BlockKind::Other,
        };
        let span = first.span.clone();
        let mut sentences = Vec::new();
        for leaf in blocks::leaves(self.source(), &events, self.options.code_notice.as_deref()) {
            let mut offset = 0;
            while let Some(sentence) = sentence::next(
                &leaf.text,
                offset,
                self.options.maximum_segment,
                &leaf.protected,
            ) {
                if let Some(text) = normalize(&leaf.text[sentence.start..sentence.end]) {
                    sentences.push(text);
                }
                offset = sentence.resume;
            }
        }
        Block {
            kind,
            span,
            events,
            sentences,
        }
    }
}

/// Collapse whitespace and remove leftover Markdown markers. Returns `None` when nothing pronounceable remains.
///
/// Footnote and reference links whose definitions come later remain literal, so `[^note]` and the label of `[text][label]` are removed and other brackets dropped; both then read as they would if the definition had come first.
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
