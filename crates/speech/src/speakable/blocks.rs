// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Speakable text of the leaf blocks within one complete document-level block.
//!
//! The events come from base-markdown's `IncrementalParser`, which emits a document-level block only once no later text can change it, so everything here works on final text and nothing is held back.

use std::ops::Range;

use base_markdown::stream::{Event, EventKind, Tag, TagKind, Text};

/// The speakable text of one leaf block (a paragraph, heading or code block), before whitespace and marker normalization.
#[derive(Debug, Default)]
pub(crate) struct Leaf {
    pub text: String,
    /// Ranges of `text` within which punctuation does not end a sentence (inline code).
    pub protected: Vec<Range<usize>>,
}

struct Link {
    destination: String,
    text_start: usize,
}

/// The speakable leaves of one document-level block, in order.
///
/// `events` are the block's events, whose text values refer to `source`. Paragraph and heading text is spoken, including within lists and block quotes. Tables, images and footnote definitions are silent, and so is every leaf without speakable text. Each code block outside them is replaced by `code_notice`, when there is one.
pub(crate) fn leaves(source: &str, events: &[Event<'_>], code_notice: Option<&str>) -> Vec<Leaf> {
    let mut leaves = Vec::new();
    let mut current: Option<Leaf> = None;
    // Depth of containers whose content is not spoken (tables, images, footnote definitions).
    let mut silent = 0usize;
    let mut link: Option<Link> = None;

    for event in events {
        match &event.kind {
            EventKind::Start(Tag::Paragraph { .. } | Tag::Heading(_)) => {
                current = Some(Leaf::default());
            }
            EventKind::End(TagKind::Paragraph | TagKind::Heading) => {
                if let Some(leaf) = current.take()
                    && silent == 0
                {
                    leaves.push(leaf);
                }
            }
            EventKind::Start(Tag::Table | Tag::Image { .. } | Tag::FootnoteDefinition(_)) => {
                silent += 1;
            }
            EventKind::End(TagKind::Table | TagKind::Image | TagKind::FootnoteDefinition) => {
                silent = silent.saturating_sub(1);
            }
            EventKind::Start(Tag::Link { destination, .. }) => {
                if let Some(leaf) = current.as_ref() {
                    link = Some(Link {
                        destination: resolve(source, destination).to_owned(),
                        text_start: leaf.text.len(),
                    });
                }
            }
            EventKind::End(TagKind::Link) => {
                if let (Some(leaf), Some(link)) = (current.as_mut(), link.take())
                    && is_raw_url(&leaf.text[link.text_start..], &link.destination)
                {
                    leaf.text.truncate(link.text_start);
                    leaf.protected.retain(|range| range.end <= link.text_start);
                }
            }
            EventKind::Text(value) if silent == 0 => {
                if let Some(leaf) = current.as_mut() {
                    leaf.text.push_str(resolve(source, value));
                }
            }
            EventKind::Code { value, .. } if silent == 0 => {
                if let Some(leaf) = current.as_mut() {
                    let start = leaf.text.len();
                    leaf.text.push_str(resolve(source, value));
                    leaf.protected.push(start..leaf.text.len());
                }
            }
            EventKind::SoftBreak(_) | EventKind::HardBreak if silent == 0 => {
                if let Some(leaf) = current.as_mut() {
                    leaf.text.push(' ');
                }
            }
            EventKind::CodeBlock { .. } if current.is_none() && silent == 0 => {
                if let Some(notice) = code_notice {
                    leaves.push(Leaf {
                        text: notice.to_owned(),
                        protected: Vec::new(),
                    });
                }
            }
            _ => {}
        }
    }
    leaves
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
