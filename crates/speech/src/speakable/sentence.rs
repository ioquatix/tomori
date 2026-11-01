// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Sentence boundaries within the speakable text of one complete leaf block.
//!
//! Every decision depends only on the text before the boundary and the first non-space character after it.

use std::ops::Range;

/// Words that end with a full stop without ending a sentence.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "e.g", "i.e", "cf", "approx", "no",
    "fig", "inc", "ltd", "co", "mt", "dept", "est", "al", "eg", "ie", "u.s", "u.k",
];

/// A sentence within a block's text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Sentence {
    /// The sentence spans `start..end`.
    pub start: usize,
    pub end: usize,
    /// Where the following sentence, if any, begins.
    pub resume: usize,
}

fn is_terminal(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}

fn is_closer(character: char) -> bool {
    matches!(character, '"' | '\'' | '”' | '’' | ')' | ']' | '»' | '」')
}

/// Find the next sentence in `text[from..]`, the complete text of a block, or `None` when only whitespace remains.
///
/// The remainder of the text is a sentence even without terminal punctuation. Sentences longer than `maximum` bytes are split at a clause or word break. Punctuation inside a `protected` range, such as inline code, never ends a sentence.
pub(crate) fn next(
    text: &str,
    from: usize,
    maximum: usize,
    protected: &[Range<usize>],
) -> Option<Sentence> {
    let start = from + skip_whitespace(&text[from..]);
    if start == text.len() {
        return None;
    }

    let mut characters = text[start..].char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        let position = start + index;
        if position - start >= maximum {
            return Some(split_long(text, start, position));
        }
        if !is_terminal(character) || protected.iter().any(|range| range.contains(&position)) {
            continue;
        }
        // Extend over repeated terminal punctuation and closing quotes or brackets.
        let mut end = position + character.len_utf8();
        let mut ellipsis = character == '…';
        let mut dots = usize::from(character == '.');
        while let Some(&(_, following)) = characters.peek() {
            if is_terminal(following) || is_closer(following) {
                if following == '.' {
                    dots += 1;
                }
                if following == '…' {
                    ellipsis = true;
                }
                end += following.len_utf8();
                characters.next();
            } else {
                break;
            }
        }
        ellipsis |= dots >= 3;

        let rest = &text[end..];
        let gap = skip_whitespace(rest);
        if gap == rest.len() {
            // Nothing but whitespace (or nothing) follows.
            return Some(Sentence {
                start,
                end,
                resume: text.len(),
            });
        }
        if gap == 0 {
            // "3.14", "e.g.x", "end.)" followed directly by text: not a boundary.
            continue;
        }
        let following = rest[gap..].chars().next().unwrap();
        if is_boundary(&text[start..position], character, ellipsis, following) {
            return Some(Sentence {
                start,
                end,
                resume: end + gap,
            });
        }
    }

    Some(Sentence {
        start,
        end: text.len(),
        resume: text.len(),
    })
}

fn is_boundary(before: &str, terminal: char, ellipsis: bool, following: char) -> bool {
    if following.is_lowercase() {
        return false;
    }
    if terminal != '.' || ellipsis {
        return true;
    }
    let word = before
        .rsplit(|character: char| character.is_whitespace() || "(\"'“‘[".contains(character))
        .next()
        .unwrap_or("");
    let lower = word.to_lowercase();
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return false;
    }
    // A single capital letter is an initial: "J. Smith".
    let mut letters = word.chars();
    if let (Some(first), None) = (letters.next(), letters.next())
        && first.is_uppercase()
    {
        return false;
    }
    true
}

/// Split an over-long sentence at the last clause or word break before `limit`.
fn split_long(text: &str, start: usize, limit: usize) -> Sentence {
    let window = &text[start..limit];
    let cut = window
        .rfind([',', ';', ':', '—'])
        .map(|index| index + window[index..].chars().next().unwrap().len_utf8())
        .filter(|&index| index > window.len() / 2)
        .or_else(|| window.rfind(char::is_whitespace).filter(|&index| index > 0))
        .unwrap_or(window.len());
    let end = start + cut;
    Sentence {
        start,
        end,
        resume: end + skip_whitespace(&text[end..]),
    }
}

fn skip_whitespace(text: &str) -> usize {
    text.len() - text.trim_start().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentences(text: &str) -> Vec<&str> {
        let mut result = Vec::new();
        let mut from = 0;
        while let Some(Sentence { start, end, resume }) = next(text, from, 400, &[]) {
            result.push(&text[start..end]);
            from = resume;
        }
        result
    }

    #[test]
    fn splits_simple_sentences() {
        assert_eq!(
            sentences("Hello there. How are you? Fine!"),
            ["Hello there.", "How are you?", "Fine!"]
        );
    }

    #[test]
    fn keeps_abbreviations_and_decimals() {
        assert_eq!(
            sentences("Dr. Smith paid 3.50 dollars, e.g. for tea. Then J. R. left."),
            [
                "Dr. Smith paid 3.50 dollars, e.g. for tea.",
                "Then J. R. left."
            ]
        );
    }

    #[test]
    fn handles_ellipses_and_quotes() {
        assert_eq!(
            sentences("Well... maybe. He said \"stop.\" Then... Nothing."),
            ["Well... maybe.", "He said \"stop.\"", "Then...", "Nothing."]
        );
    }

    #[test]
    fn decides_by_the_following_character() {
        assert_eq!(sentences("Hello. W"), ["Hello.", "W"]);
        assert_eq!(sentences("Hello. w"), ["Hello. w"]);
        assert_eq!(sentences("  \n "), Vec::<&str>::new());
    }

    #[test]
    fn keeps_protected_punctuation() {
        assert_eq!(
            next("Run a. B now. Done.", 0, 400, std::slice::from_ref(&(4..8))),
            Some(Sentence {
                start: 0,
                end: 13,
                resume: 14
            })
        );
    }

    #[test]
    fn splits_long_sentences_at_clauses() {
        let text = "one two three, four five six seven eight nine ten";
        let Sentence { start, end, .. } = next(text, 0, 20, &[]).unwrap();
        assert_eq!(&text[start..end], "one two three,");
    }
}
