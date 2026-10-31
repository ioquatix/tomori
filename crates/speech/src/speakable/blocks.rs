// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Speakable text of each leaf block, and how much of it is stable.
//!
//! This is the only part of the filter that parses Markdown. It currently re-parses the accumulated message on every delta; an incremental parser only needs to replace [`analyse`] and may skip blocks before the caller's resume offset, because the caller never revisits them.

use std::ops::Range;

use base_markdown::{
    EventParser, ParseOptions,
    stream::{EventKind, Tag, TagKind, Text},
};

/// One leaf block (paragraph, heading, code block, table, HTML block, rule or definition).
#[derive(Debug)]
pub(crate) struct Block {
    /// The source offset where the block starts; stable blocks keep the same start.
    pub start: usize,
    /// Speakable text, before whitespace and marker normalization.
    pub text: String,
    /// The length of the prefix of `text` that can no longer change.
    pub stable: usize,
    /// Ranges of `text` within which punctuation does not end a sentence (inline code).
    pub protected: Vec<Range<usize>>,
    /// Whether the whole block can no longer change.
    pub complete: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Heading,
    Code { closed: bool },
    Table,
    Other,
}

struct Leaf {
    kind: Kind,
    start: usize,
    end: usize,
    text: String,
    pieces: Vec<Piece>,
    /// The text offset of the first unresolved inline opener, if any.
    risk: Option<usize>,
    protected: Vec<Range<usize>>,
}

impl Leaf {
    fn new(kind: Kind, start: usize, end: usize) -> Self {
        Self {
            kind,
            start,
            end,
            text: String::new(),
            pieces: Vec::new(),
            risk: None,
            protected: Vec::new(),
        }
    }

    fn append(&mut self, source: usize, value: &str, exact: bool) {
        self.pieces.push(Piece {
            source,
            text: self.text.len(),
            length: value.len(),
            exact,
        });
        self.text.push_str(value);
    }

    fn hold(&mut self, text_offset: usize) {
        self.risk = Some(self.risk.map_or(text_offset, |risk| risk.min(text_offset)));
    }

    /// The text offset corresponding to `source`: exact within verbatim text, otherwise the start of the first piece at or after it.
    fn text_offset(&self, source: usize) -> usize {
        for piece in &self.pieces {
            if piece.exact && (piece.source..piece.source + piece.length).contains(&source) {
                return piece.text + (source - piece.source);
            }
            if piece.source >= source {
                return piece.text;
            }
        }
        self.text.len()
    }
}

/// Where a run of speakable text came from.
struct Piece {
    source: usize,
    text: usize,
    length: usize,
    /// Whether the text is a verbatim copy of the source at `source`.
    exact: bool,
}

struct Link {
    destination: String,
    text_start: usize,
    source_end: usize,
}

/// Analyse `source`, the accumulated Markdown of one message.
///
/// When `finished` is true the message is complete and every block is complete.
pub(crate) fn analyse(source: &str, finished: bool, code_notice: Option<&str>) -> Vec<Block> {
    let options = ParseOptions::gfm();
    let mut leaves: Vec<Leaf> = Vec::new();
    let mut current: Option<Leaf> = None;
    // Depth of containers whose content is not spoken (tables, images, footnote definitions).
    let mut silent = 0usize;
    let mut link: Option<Link> = None;

    for event in EventParser::new(source, &options) {
        let Ok(event) = event else {
            // The parser does not fail on Markdown input; treat a failure as the end of speakable text.
            break;
        };
        let span = event.span.clone();
        match event.kind {
            EventKind::Start(Tag::Paragraph { .. }) => {
                current = Some(Leaf::new(Kind::Paragraph, span.start, span.end));
            }
            EventKind::Start(Tag::Heading(_)) => {
                current = Some(Leaf::new(Kind::Heading, span.start, span.end));
            }
            EventKind::End(TagKind::Paragraph | TagKind::Heading) => {
                if let Some(mut leaf) = current.take() {
                    if silent > 0 {
                        leaf.text.clear();
                        leaf.pieces.clear();
                        leaf.risk = None;
                    }
                    leaves.push(leaf);
                }
            }
            EventKind::Start(Tag::Table) => {
                if silent == 0 {
                    leaves.push(Leaf::new(Kind::Table, span.start, span.end));
                }
                silent += 1;
            }
            EventKind::Start(Tag::Image { .. } | Tag::FootnoteDefinition(_)) => silent += 1,
            EventKind::End(TagKind::Table | TagKind::Image | TagKind::FootnoteDefinition) => {
                silent = silent.saturating_sub(1);
            }
            EventKind::Start(Tag::Link { destination, .. }) => {
                if let Some(leaf) = current.as_ref() {
                    link = Some(Link {
                        destination: resolve(source, &destination).to_owned(),
                        text_start: leaf.text.len(),
                        source_end: span.end,
                    });
                }
            }
            EventKind::End(TagKind::Link) => {
                if let (Some(leaf), Some(link)) = (current.as_mut(), link.take()) {
                    let inner = &leaf.text[link.text_start..];
                    if is_raw_url(inner, &link.destination) {
                        leaf.text.truncate(link.text_start);
                        leaf.pieces.retain(|piece| piece.text < link.text_start);
                        leaf.protected.retain(|range| range.end <= link.text_start);
                        if leaf.risk.is_some_and(|risk| risk > link.text_start) {
                            leaf.risk = Some(link.text_start);
                        }
                        // A literal autolink can still grow until whitespace follows it, and the text after it would then change.
                        if source[link.source_end..]
                            .chars()
                            .next()
                            .is_none_or(|next| !next.is_whitespace())
                        {
                            leaf.hold(link.text_start);
                        }
                    }
                }
            }
            EventKind::Text(value) if silent == 0 => {
                if let Some(leaf) = current.as_mut() {
                    let text = resolve(source, &value);
                    if let Text::Source(range) = &value {
                        if let Some(offset) = unresolved_opener(source, range.start, text) {
                            leaf.hold(leaf.text.len() + offset);
                        }
                        leaf.append(range.start, text, range.len() == text.len());
                    } else {
                        leaf.append(span.start, text, false);
                    }
                }
            }
            EventKind::Code { value, .. } if silent == 0 => {
                if let Some(leaf) = current.as_mut() {
                    let start = leaf.text.len();
                    leaf.append(span.start, resolve(source, &value), false);
                    leaf.protected.push(start..leaf.text.len());
                }
            }
            EventKind::SoftBreak(_) | EventKind::HardBreak if silent == 0 => {
                if let Some(leaf) = current.as_mut() {
                    leaf.append(span.start, " ", false);
                }
            }
            EventKind::CodeBlock { unclosed, .. } => {
                if current.is_none() && silent == 0 {
                    let mut leaf =
                        Leaf::new(Kind::Code { closed: !unclosed }, span.start, span.end);
                    if let Some(notice) = code_notice {
                        leaf.text.push_str(notice);
                    }
                    leaves.push(leaf);
                }
            }
            EventKind::Html { block: true, .. }
            | EventKind::Rule
            | EventKind::Definition { .. }
            | EventKind::Opaque { block: true, .. }
                if current.is_none() && silent == 0 =>
            {
                leaves.push(Leaf::new(Kind::Other, span.start, span.end));
            }
            _ => {}
        }
    }

    let count = leaves.len();
    let mut blocks = Vec::with_capacity(count);
    for index in 0..count {
        let leaf = &leaves[index];
        let complete = finished
            || followed_by_blank_line(source, leaf.end)
            || match leaves.get(index + 1) {
                // A following block is established once its first line is terminated (for a table, its header and delimiter rows); until then it may still turn out to continue this block.
                Some(next) => {
                    let lines = if next.kind == Kind::Table { 2 } else { 1 };
                    source[next.start..].matches('\n').count() >= lines
                }
                None => match leaf.kind {
                    Kind::Heading => source[..leaf.end].ends_with('\n'),
                    Kind::Code { closed } => closed && source[..leaf.end].ends_with('\n'),
                    _ => false,
                },
            };
        let stable = if complete {
            leaf.text.len()
        } else {
            let mut stable = leaf.risk.unwrap_or(leaf.text.len());
            if leaf.kind == Kind::Paragraph
                && let Some(line) = possible_table_header(&source[leaf.start..leaf.end])
            {
                // Hold the soft break before the line too: it disappears if the line becomes a header.
                stable = stable.min(leaf.text_offset(leaf.start + line.saturating_sub(1)));
            }
            // The last word may still grow, for example into a URL that is not spoken. Its first character is kept when it is a capital letter, so that a sentence boundary before it can be confirmed; only a capitalised email address would then change.
            if leaf.end == source.len()
                && let Some(mut word) = source[leaf.start..]
                    .rfind(char::is_whitespace)
                    .map(|index| leaf.start + index + 1)
                    .or(Some(leaf.start))
                && word < source.len()
            {
                if let Some(first) = source[word..].chars().next()
                    && first.is_uppercase()
                {
                    word += first.len_utf8();
                }
                stable = stable.min(leaf.text_offset(word));
            }
            // An unterminated last line that does not start with a letter may still start a new block (a heading, list, fence or HTML block) instead of continuing this paragraph, which would remove the soft break before it.
            if leaf.kind == Kind::Paragraph
                && leaf.end == source.len()
                && let Some(newline) = source[leaf.start..].rfind('\n')
                && newline + leaf.start + 1 < source.len()
            {
                let newline = leaf.start + newline;
                let line = source[newline + 1..].trim_start_matches([' ', '\t', '>']);
                if !line.chars().next().is_some_and(char::is_alphabetic) {
                    stable = stable.min(leaf.text_offset(newline));
                }
            }
            if matches!(leaf.kind, Kind::Code { .. }) {
                stable = 0;
            }
            stable
        };
        blocks.push(Block {
            start: leaf.start,
            text: leaf.text.clone(),
            stable,
            protected: leaf.protected.clone(),
            complete,
        });
    }
    blocks
}

fn resolve<'a>(source: &'a str, value: &'a Text<'_>) -> &'a str {
    value.resolve(source).unwrap_or("")
}

/// Whether a link's visible text is just its address, which is not read aloud.
fn is_raw_url(inner: &str, destination: &str) -> bool {
    let inner = inner.trim();
    if inner.is_empty() {
        return true;
    }
    let bare = |value: &str| {
        value
            .trim_start_matches("mailto:")
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_owned()
    };
    inner == destination
        || bare(inner) == bare(destination)
        || inner.starts_with("http://")
        || inner.starts_with("https://")
        || inner.starts_with("www.")
}

/// The offset within `text` of the first character that may still open an inline construct (code span, link, emphasis, strikethrough, raw HTML, entity or escape) once more text arrives.
///
/// Resolved constructs never appear as text, so these characters only occur literally when their construct is unresolved.
fn unresolved_opener(source: &str, origin: usize, text: &str) -> Option<usize> {
    let mut previous = source[..origin].chars().next_back();
    for (index, character) in text.char_indices() {
        let after = source[origin + index + character.len_utf8()..]
            .chars()
            .next();
        let risky = match character {
            '`' | '[' => true,
            '*' | '~' => after.is_none_or(|after| !after.is_whitespace()),
            '_' => {
                after.is_none_or(|after| !after.is_whitespace())
                    && previous.is_none_or(|previous| !previous.is_alphanumeric())
            }
            '<' => after.is_none_or(|after| after.is_alphabetic() || "/!?".contains(after)),
            '&' => after.is_none_or(|after| after.is_alphanumeric() || after == '#'),
            '\\' => after.is_none(),
            _ => false,
        };
        if risky {
            return Some(index);
        }
        previous = Some(character);
    }
    None
}

/// Whether the source after `end` begins with a blank line.
///
/// Some block spans include their terminating line ending and others do not, so an unterminated span first skips the remainder of its own line.
fn followed_by_blank_line(source: &str, end: usize) -> bool {
    let mut rest = &source[end..];
    if !source[..end].ends_with('\n') {
        match rest.find('\n') {
            Some(index) if rest[..index].trim().is_empty() => rest = &rest[index + 1..],
            _ => return false,
        }
    }
    match rest.find('\n') {
        Some(index) => rest[..index].trim().is_empty(),
        None => false,
    }
}

/// The offset of the first line in a trailing paragraph that could still become a table header.
///
/// Any line becomes a header if the next line is a delimiter row with the same number of cells, so a terminated last line is a candidate, as is an unterminated one containing `|`. When the last line itself looks like the start of a delimiter row, the line before it is the candidate.
///
/// An unterminated line without `|` is not held, so that long single-line paragraphs are spoken while they stream. If such a line later turns out to be the header of a table, sentences already spoken from it are not retracted.
fn possible_table_header(paragraph: &str) -> Option<usize> {
    let body = paragraph.strip_suffix('\n').unwrap_or(paragraph);
    let terminated = body.len() != paragraph.len();
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in body.split('\n') {
        lines.push((offset, line));
        offset += line.len() + 1;
    }
    let (last_offset, last) = *lines.last()?;
    let delimiter_like = !terminated
        && !last.trim().is_empty()
        && last.chars().all(|character| "|:- \t".contains(character));
    if delimiter_like && lines.len() >= 2 {
        return Some(lines[lines.len() - 2].0);
    }
    (terminated || last.contains('|')).then_some(last_offset)
}
