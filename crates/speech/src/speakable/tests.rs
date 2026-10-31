// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

fn one_shot(source: &str, options: &SpeakableOptions) -> Vec<String> {
    let mut speakable = Speakable::with_options(options.clone());
    let mut segments = speakable.push(source);
    segments.extend(speakable.finish());
    segments
}

fn speak(source: &str) -> Vec<String> {
    one_shot(source, &SpeakableOptions::default())
}

/// A small deterministic generator, so that failures are reproducible from the seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Feed `source` in random chunks, recording which segments were available before the message finished.
fn chunked(source: &str, options: &SpeakableOptions, seed: u64) -> (Vec<String>, usize) {
    let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut speakable = Speakable::with_options(options.clone());
    let mut segments = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let mut end = (offset + 1 + (random.next() % 12) as usize).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        segments.extend(speakable.push(&source[offset..end]));
        offset = end;
    }
    let early = segments.len();
    segments.extend(speakable.finish());
    (segments, early)
}

fn assert_stream_matches(source: &str, options: &SpeakableOptions) {
    let expected = one_shot(source, options);
    for seed in 1..200 {
        let (segments, _) = chunked(source, options, seed);
        assert_eq!(segments, expected, "seed {seed} for {source:?}");
    }
    // Character by character is the most demanding feed.
    let mut speakable = Speakable::with_options(options.clone());
    let mut segments = Vec::new();
    for character in source.chars() {
        segments.extend(speakable.push(character.encode_utf8(&mut [0; 4])));
    }
    segments.extend(speakable.finish());
    assert_eq!(segments, expected, "character feed for {source:?}");
}

const DOCUMENT: &str = "# Getting started

Run the tests first. Then, e.g. on Linux, open the *workspace* and read [the guide](https://example.com/guide). It takes 2.5 minutes... maybe less!

```rust
fn main() {
    println!(\"Not spoken.\");
}
```

| Name | Value |
|------|-------|
| a    | 1     |

- First item uses `cargo test`.
- Second item has **bold** and ~~struck~~ text.
  1. Nested ordered item.

> A quoted remark. See https://example.com/raw for details.

Setext heading
--------------

<div>Raw HTML block.</div>

An image ![alt text](image.png) is skipped. Dr. Smith agreed[^1].

[^1]: A footnote that is not read.
";

#[test]
fn speaks_only_speakable_text() {
    assert_eq!(
        speak(DOCUMENT),
        [
            "Getting started",
            "Run the tests first.",
            "Then, e.g. on Linux, open the workspace and read the guide.",
            "It takes 2.5 minutes... maybe less!",
            "First item uses cargo test.",
            "Second item has bold and struck text.",
            "Nested ordered item.",
            "A quoted remark.",
            "See for details.",
            "Setext heading",
            "An image is skipped.",
            "Dr. Smith agreed.",
        ]
    );
}

#[test]
fn says_a_notice_for_code_when_configured() {
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        ..SpeakableOptions::default()
    };
    assert_eq!(
        one_shot("Before.\n\n```sh\nls\n```\n\nAfter.", &options),
        ["Before.", "Code omitted.", "After."]
    );
    assert_stream_matches("Before.\n\n```sh\nls\n```\n\nAfter.", &options);
}

#[test]
fn streamed_document_matches_one_shot() {
    assert_stream_matches(DOCUMENT, &SpeakableOptions::default());
}

#[test]
fn streamed_constructs_match_one_shot() {
    let cases = [
        "Plain sentence one. Plain sentence two. And three",
        "Intro text.\n| a | b |\n|---|---|\n| 1 | 2 |\n\nAfter the table.",
        "Name | Value\n--- | ---\nx | y\n\nDone.",
        "Becomes a heading.\n===\n\nBody text.",
        "Becomes a heading too\n---\nBody.",
        "Use `a. B` here. And `unclosed code. Then more",
        "See [a link. With text](http://x.com/a.b). Next sentence.",
        "Tricky **bold. Still bold** text. End.",
        "Entities &amp; things. Done &copy; here.",
        "Raw <span>inline. html</span> here. Done.",
        "Fence follows.\n```\ncode. More code.\n```\nAfter fence.",
        "Tilde fence.\n~~~\nhidden.\n~~~\n",
        "Intro\n#hashtag continues. Yes.",
        "Intro.\n# Real heading\nText.",
        "- one. two\n- three. Four\n\n  continued paragraph. Yes.\n- five",
        "1986. A great year. Indeed.",
        "Escaped \\*stars\\* and a trailing backslash\\",
        "Visit www.example.com. Or mail a@b.co today. Thanks.",
        "Unicode “quotes.” Ellipsis… Next. 日本語。次。",
        "A very long sentence, with many clauses, that keeps going and going, past any sensible limit, \
		 so that the segmenter must split it somewhere reasonable, rather than waiting forever, for a full stop, \
		 which may never come, because the model is verbose, and likes commas, a great deal, really.",
        "> quote one. Quote two\n> continues here.\n\nAfter.",
        "Snake_case_names and _emphasis_ and __strong__ words. End.",
        "Footnote ref[^n] here. Next.\n\n[^n]: Note.",
        "Trailing pipe | in prose. Then more prose.\n\nEnd.",
    ];
    let options = SpeakableOptions {
        maximum_segment: 120,
        ..SpeakableOptions::default()
    };
    for case in cases {
        assert_stream_matches(case, &options);
    }
}

#[test]
fn speaks_stable_sentences_before_the_message_completes() {
    let mut speakable = Speakable::new();
    assert!(speakable.push("Hello there. How").len() == 1);
    assert_eq!(speakable.push(" are you? I am"), ["How are you?"]);
    assert_eq!(speakable.finish(), ["I am"]);
}

#[test]
fn holds_a_line_that_may_become_a_table_header() {
    let mut speakable = Speakable::new();
    assert!(speakable.push("| Name. Here | Value. There |").is_empty());
    assert!(speakable.push("\n|---|").is_empty());
    assert!(speakable.push("---|\n| a | b |\n\nAfter. ").is_empty());
    assert_eq!(speakable.push("Done."), ["After."]);
    assert_eq!(speakable.finish(), ["Done."]);
}

#[test]
fn holds_a_line_that_may_become_a_setext_heading() {
    let mut speakable = Speakable::new();
    assert!(speakable.push("Title\n").is_empty());
    assert!(speakable.push("====").is_empty());
    assert_eq!(speakable.push("=\n"), ["Title"]);
    assert!(speakable.push("\nBody.").is_empty());
    assert_eq!(speakable.finish(), ["Body."]);
}

#[test]
fn holds_unclosed_code_spans_and_links() {
    let mut speakable = Speakable::new();
    assert_eq!(speakable.push("One. Run `cargo. Test"), ["One."]);
    assert_eq!(speakable.push("` now. Next"), ["Run cargo. Test now."]);
    assert!(speakable.push(" [the guide").is_empty());
    assert_eq!(
        speakable.push("](https://x.y) here. "),
        Vec::<String>::new()
    );
    assert_eq!(speakable.push("Done"), ["Next the guide here."]);
    assert_eq!(speakable.finish(), ["Done"]);
}

#[test]
fn skips_unclosed_code_blocks_until_finished() {
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        ..SpeakableOptions::default()
    };
    let mut speakable = Speakable::with_options(options);
    assert_eq!(
        speakable.push("Look.\n```\nprint('Hi. There')\n"),
        ["Look."]
    );
    assert!(speakable.push("more()\n").is_empty());
    assert_eq!(speakable.push("```\n"), ["Code omitted."]);
    assert!(speakable.finish().is_empty());
}

#[test]
fn finish_is_idempotent() {
    let mut speakable = Speakable::new();
    speakable.push("Only text");
    assert_eq!(speakable.finish(), ["Only text"]);
    assert!(speakable.finish().is_empty());
}

#[test]
fn streamed_random_documents_match_one_shot() {
    let fragments = [
        "Plain words. ",
        "More words",
        "\n",
        "\n\n",
        "*em* ",
        "**strong. text** ",
        "`code. span` ",
        "[link](http://a.b/c) ",
        "https://raw.example/x ",
        // A delimiter row directly after a setext underline triggers an upstream parser bug (see `upstream_setext_underline_before_delimiter_row`), so table rows only appear together.
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "---\n",
        "===\n",
        "# Heading\n",
        "- item. ",
        "1. step ",
        "> quote. ",
        "```\nfenced. code\n```\n",
        "~~~\n",
        "<b>bold</b> ",
        "&amp; ",
        "Dr. Who? ",
        "Wait... ",
        "3.14 is pi. ",
        "    indented code\n",
        "\\* escaped ",
        // Footnote definitions are omitted: a definition arriving after its reference changes earlier text (see the module documentation).
        "[^1] ",
        "_under_ ",
        // A table header line whose prose before the first pipe contains a sentence is a known limitation, so headers start their own line here.
        "\nName | Value\n",
        "> ",
        "  ",
        "* ",
        "<div>\n",
        "café… ",
    ];
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        maximum_segment: 60,
    };
    // Set SPEAKABLE_FUZZ=<documents>[:<seed>] for a longer run.
    let fuzz = std::env::var("SPEAKABLE_FUZZ").unwrap_or_default();
    let mut parts = fuzz.split(':').map(|part| part.parse::<u64>().ok());
    let documents = parts.next().flatten().unwrap_or(300);
    let mut random = Random(parts.next().flatten().unwrap_or(0x5EED));
    for _ in 0..documents {
        let count = 3 + random.next() % 12;
        let source: String = (0..count)
            .map(|_| fragments[(random.next() % fragments.len() as u64) as usize])
            .collect();
        let expected = one_shot(&source, &options);
        for seed in 1..20 {
            let (segments, _) = chunked(&source, &options, seed);
            assert_eq!(segments, expected, "seed {seed} for {source:?}");
        }
    }
}

/// base-markdown 0.1.1 treats a setext underline followed by a one-column delimiter row as a table header, fails to form the table, and leaves the underline as paragraph text: `a\n===\n|-` parses as two paragraphs instead of a heading and a paragraph. Streaming through that intermediate state can then lose text.
#[test]
#[ignore = "upstream base-markdown bug"]
fn upstream_setext_underline_before_delimiter_row() {
    assert_eq!(speak("a\n===\n|-"), ["a"]);
    assert_stream_matches(
        "- a b\n===\n  |---|---|\n\nAfter.",
        &SpeakableOptions::default(),
    );
}

/// Feed `source` in random chunks, either parsing from the last spoken boundary or, as an oracle, parsing the whole message every time.
fn chunked_parse(
    source: &str,
    options: &SpeakableOptions,
    seed: u64,
    whole: bool,
) -> Vec<Vec<String>> {
    let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut speakable = Speakable::with_options(options.clone());
    speakable.whole = whole;
    let mut pushes = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let mut end = (offset + 1 + (random.next() % 9) as usize).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        pushes.push(speakable.push(&source[offset..end]));
        offset = end;
    }
    pushes.push(speakable.finish());
    pushes
}

#[test]
fn parsing_from_a_boundary_matches_parsing_everything() {
    // Unlike the one-shot comparison, this includes constructs whose streamed result differs from the complete message, such as definitions after their use, since both feeds must still agree after every delta.
    let fragments = [
        "Plain words. ",
        "More words",
        "\n",
        "\n\n",
        "*em* ",
        "`code. span` ",
        "[link](http://a.b/c) ",
        "[text][r] ",
        "[r]: http://r.example\n",
        "  [r]: http://indented.example \"Title\"\n",
        "[^1] ",
        "[^1]: A note.\n",
        "[^2]: Long note.\n\n    continued note.\n",
        "    indented code\n",
        "  two spaces. ",
        "# Heading\n",
        "- item. ",
        "  continued item. ",
        "1. step ",
        "> quote. ",
        "> [q]: http://q.example\n",
        "[see][q] ",
        "```\nfenced. code\n",
        "```\n",
        "<div>\n",
        "<!-- comment\n",
        "-->\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "---\n",
        "===\n",
        "Dr. Who? ",
        "https://raw.example/x ",
        "café… ",
    ];
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        maximum_segment: 60,
    };
    // Set SPEAKABLE_FUZZ=<documents>[:<seed>] for a longer run.
    let fuzz = std::env::var("SPEAKABLE_FUZZ").unwrap_or_default();
    let mut parts = fuzz.split(':').map(|part| part.parse::<u64>().ok());
    let documents = parts.next().flatten().unwrap_or(400);
    let mut random = Random(parts.next().flatten().unwrap_or(0xB0DA));
    for _ in 0..documents {
        let count = 3 + random.next() % 20;
        let source: String = (0..count)
            .map(|_| fragments[(random.next() % fragments.len() as u64) as usize])
            .collect();
        for seed in 1..8 {
            assert_eq!(
                chunked_parse(&source, &options, seed, false),
                chunked_parse(&source, &options, seed, true),
                "seed {seed} for {source:?}"
            );
        }
    }
}

#[test]
fn definitions_before_the_parsing_boundary_still_apply() {
    let source = "[r]: http://r.example\n\n[^1]: A note.\n\nFirst. Second.\n\n    indented code\n\nThird [text][r] and a note[^1]. Fourth.\n\n> Fifth.\n";
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        ..SpeakableOptions::default()
    };
    for seed in 1..50 {
        assert_eq!(
            chunked_parse(source, &options, seed, false),
            chunked_parse(source, &options, seed, true),
            "seed {seed}"
        );
    }
    let mut speakable = Speakable::with_options(options.clone());
    let mut segments = Vec::new();
    for character in source.chars() {
        segments.extend(speakable.push(character.encode_utf8(&mut [0; 4])));
    }
    assert!(speakable.origin > source.find("Third").unwrap());
    segments.extend(speakable.finish());
    assert_eq!(
        segments,
        [
            "First.",
            "Second.",
            "Code omitted.",
            "Third text and a note.",
            "Fourth.",
            "Fifth."
        ]
    );
}

#[test]
fn streaming_a_long_message_parses_each_part_a_bounded_number_of_times() {
    let mut source = String::new();
    for index in 0..2_000 {
        source.push_str(&format!(
            "Sentence {index} has a [link](https://example.com/{index}) and `code`."
        ));
        source.push_str(if index % 5 == 4 { "\n\n" } else { " " });
    }
    let mut speakable = Speakable::new();
    let mut segments = 0;
    for chunk in source.as_bytes().chunks(4) {
        segments += speakable.push(std::str::from_utf8(chunk).unwrap()).len();
    }
    segments += speakable.finish().len();
    assert_eq!(segments, 2_000);
    // Each delta re-parses at most the current paragraph and the one before it, so the total is linear in the message rather than quadratic.
    let deltas = source.len() / 4 + 2;
    let paragraph = source.len() / 400;
    assert!(
        speakable.parsed < deltas * 2 * paragraph,
        "parsed {} bytes for a {} byte message",
        speakable.parsed,
        source.len()
    );
}
