// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Sentence boundaries within the speakable text of one block.
//!
//! Every decision depends only on the text before the boundary and the first non-space character after it, so a boundary found in a stable prefix is the same boundary that a one-shot pass over the complete text would find.

use std::ops::Range;

/// Words that end with a full stop without ending a sentence.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "e.g", "i.e", "cf", "approx", "no",
    "fig", "inc", "ltd", "co", "mt", "dept", "est", "al", "eg", "ie", "u.s", "u.k",
];

/// The outcome of looking for the next sentence in a block's text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// A sentence spans `start..end`; the following sentence (if any) begins at `resume`.
    Sentence {
        start: usize,
        end: usize,
        resume: usize,
    },
    /// More text is needed to decide where the next sentence ends.
    Pending,
    /// Only whitespace remains.
    Empty,
}

fn is_terminal(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}

fn is_closer(character: char) -> bool {
    matches!(character, '"' | '\'' | '”' | '’' | ')' | ']' | '»' | '」')
}

/// Find the next sentence in `text[from..available]`.
///
/// `available` is the end of the stable prefix. When `complete` is true, `available` is the end of the block and the remainder is a sentence even without terminal punctuation. Punctuation inside a `protected` range, such as inline code, never ends a sentence.
pub(crate) fn next(
    text: &str,
    from: usize,
    available: usize,
    complete: bool,
    maximum: usize,
    protected: &[Range<usize>],
) -> Next {
    let stable = &text[..available];
    let start = from + skip_whitespace(&stable[from..]);
    if start == available {
        // Whitespace alone is never a sentence, whether or not more follows.
        return Next::Empty;
    }

    let mut characters = stable[start..].char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        let position = start + index;
        if position - start >= maximum {
            return split_long(stable, start, position);
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

        let rest = &stable[end..];
        let gap = skip_whitespace(rest);
        if gap == rest.len() {
            // Nothing but whitespace (or nothing) follows within the stable text.
            if complete && available == text.len() {
                return Next::Sentence {
                    start,
                    end,
                    resume: available,
                };
            }
            return Next::Pending;
        }
        if gap == 0 {
            // "3.14", "e.g.x", "end.)" followed directly by text: not a boundary.
            continue;
        }
        let following = rest[gap..].chars().next().unwrap();
        if is_boundary(&stable[start..position], character, ellipsis, following) {
            return Next::Sentence {
                start,
                end,
                resume: end + gap,
            };
        }
    }

    if complete && available == text.len() {
        Next::Sentence {
            start,
            end: available,
            resume: available,
        }
    } else {
        Next::Pending
    }
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
fn split_long(text: &str, start: usize, limit: usize) -> Next {
    let window = &text[start..limit];
    let cut = window
        .rfind([',', ';', ':', '—'])
        .map(|index| index + window[index..].chars().next().unwrap().len_utf8())
        .filter(|&index| index > window.len() / 2)
        .or_else(|| window.rfind(char::is_whitespace).filter(|&index| index > 0))
        .unwrap_or(window.len());
    let end = start + cut;
    Next::Sentence {
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
        loop {
            match next(text, from, text.len(), true, 400, &[]) {
                Next::Sentence { start, end, resume } => {
                    result.push(&text[start..end]);
                    from = resume;
                }
                Next::Empty => return result,
                Next::Pending => panic!("a complete block is never pending"),
            }
        }
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
    fn waits_for_the_following_character() {
        assert_eq!(next("Hello. ", 0, 7, false, 400, &[]), Next::Pending);
        assert_eq!(
            next("Hello. W", 0, 8, false, 400, &[]),
            Next::Sentence {
                start: 0,
                end: 6,
                resume: 7
            }
        );
        assert_eq!(next("Hello. w", 0, 8, false, 400, &[]), Next::Pending);
    }

    #[test]
    fn splits_long_sentences_at_clauses() {
        let text = "one two three, four five six seven eight nine ten";
        match next(text, 0, text.len(), false, 20, &[]) {
            Next::Sentence { start, end, .. } => assert_eq!(&text[start..end], "one two three,"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
