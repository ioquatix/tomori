// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use super::*;

/// Push `delta` and return the sentences of the blocks that closed.
fn push(speakable: &mut Speakable, delta: &str) -> Vec<String> {
    sentences(speakable.push(delta)).collect()
}

fn finish(speakable: &mut Speakable) -> Vec<String> {
    sentences(speakable.finish()).collect()
}

fn one_shot_blocks(source: &str, options: &SpeakableOptions) -> Vec<Block> {
    let mut speakable = Speakable::with_options(options.clone());
    let mut blocks = speakable.push(source);
    blocks.extend(speakable.finish());
    blocks
}

fn one_shot(source: &str, options: &SpeakableOptions) -> Vec<String> {
    sentences(one_shot_blocks(source, options)).collect()
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

/// Feed `source` in random chunks and return the blocks.
fn chunked(source: &str, options: &SpeakableOptions, seed: u64) -> Vec<Block> {
    let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut speakable = Speakable::with_options(options.clone());
    let mut blocks = Vec::new();
    let mut offset = 0;
    while offset < source.len() {
        let mut end = (offset + 1 + (random.next() % 12) as usize).min(source.len());
        while !source.is_char_boundary(end) {
            end += 1;
        }
        blocks.extend(speakable.push(&source[offset..end]));
        offset = end;
    }
    blocks.extend(speakable.finish());
    blocks
}

/// Feed `source` one character at a time and return the blocks.
fn by_character(source: &str, options: &SpeakableOptions) -> Vec<Block> {
    let mut speakable = Speakable::with_options(options.clone());
    let mut blocks = Vec::new();
    for character in source.chars() {
        blocks.extend(speakable.push(character.encode_utf8(&mut [0; 4])));
    }
    blocks.extend(speakable.finish());
    blocks
}

/// Every division of `source` yields the same blocks, events and sentences as one push.
fn assert_stream_matches(source: &str, options: &SpeakableOptions) {
    let expected = one_shot_blocks(source, options);
    for seed in 1..200 {
        assert_eq!(
            chunked(source, options, seed),
            expected,
            "seed {seed} for {source:?}"
        );
    }
    // Character by character is the most demanding feed.
    assert_eq!(
        by_character(source, options),
        expected,
        "character feed for {source:?}"
    );
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
fn returns_each_document_level_block_with_its_structure() {
    let blocks = one_shot_blocks(DOCUMENT, &SpeakableOptions::default());
    let kinds: Vec<_> = blocks.iter().map(|block| block.kind).collect();
    assert_eq!(
        kinds,
        [
            BlockKind::Heading(1),
            BlockKind::Paragraph,
            BlockKind::Code,
            BlockKind::Table,
            BlockKind::List,
            BlockKind::BlockQuote,
            BlockKind::Heading(2),
            BlockKind::Html,
            BlockKind::Paragraph,
            BlockKind::FootnoteDefinition,
        ]
    );
    let table = &blocks[3];
    assert_eq!(
        table.source(DOCUMENT),
        "| Name | Value |\n|------|-------|\n| a    | 1     |"
    );
    assert!(table.sentences.is_empty());
    let cells = table
        .events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Start(Tag::TableCell { .. })))
        .count();
    assert_eq!(cells, 4);
    assert!(matches!(
        table.events.last().unwrap().kind,
        EventKind::End(base_markdown::stream::TagKind::Table)
    ));
    let code = &blocks[2];
    assert!(code.source(DOCUMENT).starts_with("```rust\n"));
    assert!(matches!(
        &code.events[..],
        [Event {
            kind: EventKind::CodeBlock {
                unclosed: false,
                ..
            },
            ..
        }]
    ));
    // Every event of a block lies within its span, and blocks do not overlap.
    let mut end = 0;
    for block in &blocks {
        assert!(block.span.start >= end, "{block:?}");
        for event in &block.events {
            assert!(event.span.start >= block.span.start && event.span.end <= block.span.end);
        }
        end = block.span.end;
    }
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
        "Prose before. Name | Value\n--- | ---\nx | y\n",
        "Mail Someone@Example.com today.\n\nDone.",
        "Line one\r\nline two.\r\n\r\nNext.",
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
fn speaks_a_paragraph_once_it_closes_then_sentence_by_sentence() {
    let mut speakable = Speakable::new();
    assert!(push(&mut speakable, "Hello there. How").is_empty());
    assert!(push(&mut speakable, " are you?\nI am").is_empty());
    assert!(push(&mut speakable, " fine.\n").is_empty());
    // The blank line closes the paragraph; its sentences are returned separately, so that synthesis of the first can start at once.
    assert_eq!(
        push(&mut speakable, "\n"),
        ["Hello there.", "How are you?", "I am fine."]
    );
    assert!(push(&mut speakable, "Second paragraph").is_empty());
    assert_eq!(finish(&mut speakable), ["Second paragraph"]);
}

#[test]
fn a_heading_is_spoken_at_the_end_of_its_line() {
    let mut speakable = Speakable::new();
    assert_eq!(push(&mut speakable, "Intro.\n\n# Head"), ["Intro."]);
    assert_eq!(push(&mut speakable, "ing\n"), ["Heading"]);
    assert!(push(&mut speakable, "Body.").is_empty());
    assert_eq!(finish(&mut speakable), ["Body."]);
}

#[test]
fn a_block_that_interrupts_a_paragraph_releases_it_once_another_block_starts() {
    let mut speakable = Speakable::new();
    // A heading that follows a paragraph without a blank line does not yet prove that the paragraph closed.
    assert!(push(&mut speakable, "Intro.\n# Heading\n").is_empty());
    assert_eq!(push(&mut speakable, "Body.\n"), ["Intro.", "Heading"]);
    assert_eq!(finish(&mut speakable), ["Body."]);
}

#[test]
fn a_paragraph_that_becomes_a_table_header_is_never_spoken() {
    let mut speakable = Speakable::new();
    let mut spoken = Vec::new();
    for delta in [
        "Name. Here",
        " | Value. There\n",
        "|---|",
        "---|\n",
        "| a | b |\n",
        "\n",
    ] {
        spoken.extend(speakable.push(delta));
    }
    assert_eq!(spoken.len(), 1);
    assert_eq!(spoken[0].kind, BlockKind::Table);
    assert!(spoken[0].sentences.is_empty());
    assert!(push(&mut speakable, "After. ").is_empty());
    assert_eq!(finish(&mut speakable), ["After."]);
}

#[test]
fn holds_a_line_that_may_become_a_setext_heading() {
    let mut speakable = Speakable::new();
    assert!(push(&mut speakable, "Title\n").is_empty());
    assert!(push(&mut speakable, "====").is_empty());
    let blocks = speakable.push("=\n\n");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, BlockKind::Heading(1));
    assert_eq!(blocks[0].sentences, ["Title"]);
    assert!(push(&mut speakable, "Body.").is_empty());
    assert_eq!(finish(&mut speakable), ["Body."]);
}

#[test]
fn releases_a_list_as_one_block() {
    let mut speakable = Speakable::new();
    assert!(push(&mut speakable, "- First. Item.\n").is_empty());
    assert!(push(&mut speakable, "- Second item.\n\n").is_empty());
    assert!(push(&mut speakable, "  Still the second.\n").is_empty());
    assert!(push(&mut speakable, "- Third.\n\n").is_empty());
    let blocks = speakable.push("After.\n");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, BlockKind::List);
    assert_eq!(
        blocks[0].sentences,
        [
            "First.",
            "Item.",
            "Second item.",
            "Still the second.",
            "Third."
        ]
    );
    let items = blocks[0]
        .events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Start(Tag::Item { .. })))
        .count();
    assert_eq!(items, 3);
    assert_eq!(finish(&mut speakable), ["After."]);
}

#[test]
fn holds_unclosed_code_spans_and_links_within_their_paragraph() {
    let mut speakable = Speakable::new();
    assert!(push(&mut speakable, "One. Run `cargo. Test").is_empty());
    assert!(push(&mut speakable, "` now. Next [the guide").is_empty());
    assert!(push(&mut speakable, "](https://x.y) here.\n\n").len() == 3);
    assert!(push(&mut speakable, "Done").is_empty());
    assert_eq!(finish(&mut speakable), ["Done"]);
    let mut speakable = Speakable::new();
    speakable.push("One. Run `cargo. Test` now. Next [the guide](https://x.y) here.");
    assert_eq!(
        finish(&mut speakable),
        ["One.", "Run cargo. Test now.", "Next the guide here."]
    );
}

#[test]
fn skips_unclosed_code_blocks_until_finished() {
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        ..SpeakableOptions::default()
    };
    let mut speakable = Speakable::with_options(options);
    assert_eq!(
        push(&mut speakable, "Look.\n\n```\nprint('Hi. There')\n"),
        ["Look."]
    );
    assert!(push(&mut speakable, "more()\n").is_empty());
    let blocks = speakable.push("```\n");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, BlockKind::Code);
    assert_eq!(blocks[0].sentences, ["Code omitted."]);
    assert!(finish(&mut speakable).is_empty());
}

#[test]
fn speaks_a_notice_for_a_code_block_left_open() {
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        ..SpeakableOptions::default()
    };
    let mut speakable = Speakable::with_options(options);
    assert!(push(&mut speakable, "```\nnever closed\n").is_empty());
    let blocks = speakable.finish();
    assert_eq!(blocks.len(), 1);
    assert!(matches!(
        blocks[0].events[0].kind,
        EventKind::CodeBlock { unclosed: true, .. }
    ));
    assert_eq!(blocks[0].sentences, ["Code omitted."]);
}

#[test]
fn finish_is_idempotent() {
    let mut speakable = Speakable::new();
    speakable.push("Only text");
    assert_eq!(finish(&mut speakable), ["Only text"]);
    assert!(speakable.finish().is_empty());
}

#[test]
#[should_panic(expected = "already finished")]
fn push_after_finish_panics() {
    let mut speakable = Speakable::new();
    speakable.finish();
    speakable.push("Late.");
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
        "| a | b |\n",
        "|---|---|\n",
        "| 1 | 2 |\n",
        "---\n",
        "===\n",
        "# Heading\n",
        "- item. ",
        "  continued item. ",
        "1. step ",
        "> quote. ",
        "```\nfenced. code\n",
        "```\n",
        "~~~\n",
        "<b>bold</b> ",
        "<div>\n",
        "<!-- comment\n",
        "-->\n",
        "&amp; ",
        "Dr. Who? ",
        "Wait... ",
        "3.14 is pi. ",
        "    indented code\n",
        "\\* escaped ",
        "[^1] ",
        "[^1]: A note.\n",
        "[^2]: Long note.\n\n    continued note.\n",
        "[text][r] ",
        "[r]: http://r.example\n",
        "  [r]: http://indented.example \"Title\"\n",
        "> [q]: http://q.example\n",
        "_under_ ",
        "Name | Value\n",
        "> ",
        "  ",
        "* ",
        "café… ",
        "\r\n",
    ];
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        maximum_segment: 60,
    };
    // Set SPEAKABLE_FUZZ=<documents>[:<seed>] for a longer run.
    let fuzz = std::env::var("SPEAKABLE_FUZZ").unwrap_or_default();
    let mut parts = fuzz.split(':').map(|part| part.parse::<u64>().ok());
    let documents = parts.next().flatten().unwrap_or(400);
    let mut random = Random(parts.next().flatten().unwrap_or(0x5EED));
    for _ in 0..documents {
        let count = 3 + random.next() % 16;
        let source: String = (0..count)
            .map(|_| fragments[(random.next() % fragments.len() as u64) as usize])
            .collect();
        let expected = one_shot_blocks(&source, &options);
        for seed in 1..12 {
            assert_eq!(
                chunked(&source, &options, seed),
                expected,
                "seed {seed} for {source:?}"
            );
        }
        assert_eq!(
            by_character(&source, &options),
            expected,
            "character feed for {source:?}"
        );
    }
}

/// base-markdown 0.1.1 treated a setext underline followed by a one-column delimiter row as a table header; 0.1.4 gives the underline precedence.
#[test]
fn setext_underline_before_delimiter_row() {
    assert_eq!(speak("a\n===\n|-"), ["a"]);
    assert_stream_matches(
        "- a b\n===\n  |---|---|\n\nAfter.",
        &SpeakableOptions::default(),
    );
}

#[test]
fn definitions_apply_to_later_blocks() {
    let source = "[r]: http://r.example\n\n[^1]: A note.\n\nFirst. Second.\n\n    indented code\n\nThird [text][r] and a note[^1]. Fourth.\n\n> Fifth.\n";
    let options = SpeakableOptions {
        code_notice: Some("Code omitted.".into()),
        ..SpeakableOptions::default()
    };
    assert_stream_matches(source, &options);
    let blocks = one_shot_blocks(source, &options);
    let third = blocks
        .iter()
        .find(|block| block.source(source).starts_with("Third"))
        .unwrap();
    assert!(
        third
            .events
            .iter()
            .any(|event| matches!(event.kind, EventKind::Start(Tag::Link { .. })))
    );
    assert_eq!(
        sentences(blocks).collect::<Vec<_>>(),
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
fn a_use_before_its_definition_reads_the_same() {
    assert_eq!(
        speak("See [the docs][d] and a note[^n]. Done.\n\n[d]: http://d.example\n\n[^n]: Note.\n"),
        ["See the docs and a note.", "Done."]
    );
}

#[test]
fn streams_a_long_message_block_by_block() {
    let mut source = String::new();
    for index in 0..2_000 {
        source.push_str(&format!(
            "Sentence {index} has a [link](https://example.com/{index}) and `code`."
        ));
        source.push_str(if index % 5 == 4 { "\n\n" } else { " " });
    }
    let mut speakable = Speakable::new();
    let mut blocks = 0;
    let mut segments = 0;
    for chunk in source.as_bytes().chunks(4) {
        let closed = speakable.push(std::str::from_utf8(chunk).unwrap());
        blocks += closed.len();
        segments += closed
            .iter()
            .map(|block| block.sentences.len())
            .sum::<usize>();
    }
    // The message ends with a blank line, so every paragraph closes while streaming.
    assert_eq!(blocks, 400);
    segments += finish(&mut speakable).len();
    assert_eq!(segments, 2_000);
}

#[test]
fn identifies_rules_definitions_and_html_blocks() {
    let source = "[r]: http://r.example\n\n---\n\n<div>\nhidden\n</div>\n\nText.\n";
    let blocks = one_shot_blocks(source, &SpeakableOptions::default());
    let kinds: Vec<_> = blocks.iter().map(|block| block.kind).collect();
    assert_eq!(
        kinds,
        [
            BlockKind::Definition,
            BlockKind::Rule,
            BlockKind::Html,
            BlockKind::Paragraph
        ]
    );
    assert_eq!(sentences(blocks).collect::<Vec<_>>(), ["Text."]);
}

#[test]
fn a_paragraph_that_leads_into_a_list_waits_for_the_list() {
    let mut speakable = Speakable::new();
    assert!(push(&mut speakable, "Here is how:\n- First.\n- Second.\n\n").is_empty());
    let blocks = speakable.push("Done.\n");
    let kinds: Vec<_> = blocks.iter().map(|block| block.kind).collect();
    assert_eq!(kinds, [BlockKind::Paragraph, BlockKind::List]);
    assert_eq!(
        sentences(blocks).collect::<Vec<_>>(),
        ["Here is how:", "First.", "Second."]
    );
    assert_eq!(finish(&mut speakable), ["Done."]);
}
