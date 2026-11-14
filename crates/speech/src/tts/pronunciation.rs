// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Pronunciations of whole words that the phonemizer gets wrong, such as "README", which it spells letter by letter: Tomori's own, in `assets/pronunciations.toml`, and a server's, in `pronunciations.toml` in its state directory.
//!
//! A word is given as a respelling, words the phonemizer pronounces correctly, or as phonemes between slashes. The phonemizer is told each word's phonemes with misaki's markup, `[README](/ɹˈidmi/)`, which `loqui-g2p` honours.
use std::{collections::HashMap, path::Path, sync::LazyLock};

use regex::Regex;

/// Tomori's own pronunciations.
pub(crate) const BUILT_IN: &str = include_str!("../../assets/pronunciations.toml");

/// The characters that stand in for marked words while the rest of the text is normalized: Unicode's private use area, which nothing pronounces, so they are taken out of the text first.
const PLACEHOLDERS: std::ops::RangeInclusive<u32> = 0xE000..=0xF8FF;

/// Whether `symbol` stands in for a marked word.
pub(crate) fn is_placeholder(symbol: char) -> bool {
    PLACEHOLDERS.contains(&u32::from(symbol))
}

/// Words and their phonemes, by the word in lowercase.
#[derive(Debug, Default)]
pub(crate) struct Pronunciations {
    words: HashMap<String, String>,
}

/// A run of letters and digits, which may be joined by `-`, `.` or `_`, such as "sherpa-onnx" or "README.md".
static RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+(?:[-._][\p{L}\p{N}]+)*").expect("a valid pattern"));

/// A possessive `'s` that follows a run.
static POSSESSIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^['’]s\b").expect("a valid pattern"));

/// What may follow a word joined to it in a run: `-`, `.` or `_`, then a letter or digit.
static JOINED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-._][\p{L}\p{N}]").expect("a valid pattern"));

/// A word of a run.
static PART: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").expect("a valid pattern"));

impl Pronunciations {
    /// The pronunciations in `source`, a TOML table of words and their pronunciations, from `origin`, which is named in warnings. A respelling is phonemized by `respell`. An entry that cannot be used, as its key is not a word or its value not a pronunciation, is left out with a warning, so that one mistake does not lose the rest; a file that is not TOML gives none, and says why.
    pub(crate) fn parse(
        source: &str,
        origin: &str,
        respell: impl Fn(&str) -> String,
    ) -> Result<Self, String> {
        let document: toml_edit::DocumentMut = source
            .parse()
            .map_err(|error| format!("{origin} is not TOML: {error}"))?;
        let mut words = HashMap::new();
        for (key, item) in document.iter() {
            match Self::entry(key, item, &respell) {
                Ok(phonemes) => {
                    words.insert(key.to_lowercase(), phonemes);
                }
                Err(reason) => {
                    log::warn!("The pronunciation of {key:?} in {origin} is left out: {reason}.");
                }
            }
        }
        Ok(Self { words })
    }

    /// The phonemes for `key`, given `item`, or why they cannot be used.
    fn entry(
        key: &str,
        item: &toml_edit::Item,
        respell: impl Fn(&str) -> String,
    ) -> Result<String, &'static str> {
        if RUN.find(key).map(|run| run.as_str()) != Some(key) {
            return Err("it is not a word, or words joined by -, . or _");
        }
        let value = item
            .as_str()
            .ok_or("its pronunciation is not a string")?
            .trim();
        let phonemes = match value
            .strip_prefix('/')
            .and_then(|value| value.strip_suffix('/'))
        {
            Some(phonemes) => phonemes.trim().to_owned(),
            None => respell(value),
        };
        if phonemes.is_empty() {
            return Err("it has no phonemes");
        }
        // The markup that gives the phonemizer the word's phonemes ends at these.
        if phonemes.contains(['[', ']', '(', ')', '/']) {
            return Err("its phonemes contain a bracket or a slash");
        }
        Ok(phonemes)
    }

    /// Tomori's own pronunciations, and then those in `file`, if it is given and exists, which add to and replace Tomori's. A file that cannot be read, or is not TOML, is logged and left out.
    pub(crate) fn load(file: Option<&Path>, respell: impl Fn(&str) -> String) -> Self {
        let mut pronunciations =
            Self::parse(BUILT_IN, "Tomori's pronunciations", &respell).expect("valid TOML");
        let Some(file) = file else {
            return pronunciations;
        };
        let added = match std::fs::read_to_string(file) {
            Ok(source) => Self::parse(&source, &file.display().to_string(), &respell),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return pronunciations,
            Err(error) => Err(format!("{} could not be read: {error}", file.display())),
        };
        match added {
            Ok(added) => {
                log::info!(
                    "Pronouncing {} words as {} says.",
                    added.words.len(),
                    file.display()
                );
                pronunciations.words.extend(added.words);
            }
            Err(reason) => log::warn!("{reason}; its pronunciations are not used."),
        }
        pronunciations
    }

    /// The number of words.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.words.len()
    }

    /// `text` with each word that has a pronunciation replaced by a placeholder, which passes through normalization unchanged, and the markup that [`restore`] puts in its place. A run of words, such as "sherpa-onnx", is replaced whole if it has a pronunciation, and otherwise each of its words that has one. A possessive `'s` after a word that has one is marked with it, as the phonemizer would otherwise say it as "s" whatever comes before.
    pub(crate) fn protect(&self, text: &str) -> (String, Vec<String>) {
        let text: String = text
            .chars()
            .filter(|&symbol| !is_placeholder(symbol))
            .collect();
        let mut marks = Vec::new();
        let mut protected = String::with_capacity(text.len());
        // Marks `word`, with `possessive` after it, if it has a pronunciation, and says whether it did.
        let mut mark = |protected: &mut String, word: &str, possessive: &str| match (
            self.words.get(&word.to_lowercase()),
            char::from_u32(PLACEHOLDERS.start() + marks.len() as u32)
                .filter(|&symbol| is_placeholder(symbol)),
        ) {
            (Some(phonemes), Some(placeholder)) => {
                protected.push(placeholder);
                let suffix = if possessive.is_empty() {
                    ""
                } else {
                    possessive_suffix(phonemes)
                };
                let word = markup_text(word);
                marks.push(format!("[{word}{possessive}](/{phonemes}{suffix}/)"));
                true
            }
            _ => {
                protected.push_str(word);
                false
            }
        };
        let mut end = 0;
        for run in RUN.find_iter(&text) {
            // The "s" of a possessive marked with the word before it.
            if run.start() < end {
                continue;
            }
            protected.push_str(&text[end..run.start()]);
            end = run.end();
            // Not "'s" that a further word is joined to, as in "API's-based", whose "s-based" is a run of its own.
            let possessive = POSSESSIVE
                .find(&text[run.end()..])
                .filter(|possessive| !JOINED.is_match(&text[run.end() + possessive.end()..]))
                .map_or("", |possessive| possessive.as_str());
            if self.words.contains_key(&run.as_str().to_lowercase()) {
                if mark(&mut protected, run.as_str(), possessive) {
                    end += possessive.len();
                }
                continue;
            }
            let mut within = run.start();
            for part in PART.find_iter(run.as_str()) {
                let (start, finish) = (run.start() + part.start(), run.start() + part.end());
                protected.push_str(&text[within..start]);
                let last = finish == run.end();
                let possessive = if last { possessive } else { "" };
                let word = part.as_str();
                // A word in camel case, such as "panelId", is read as its words, as normalization divides it, so each of them that has a pronunciation is marked, unless the whole word has one.
                let pieces = camel_pieces(word);
                let marked = if self.words.contains_key(&word.to_lowercase())
                    || !pieces
                        .iter()
                        .any(|piece| self.words.contains_key(&piece.to_lowercase()))
                {
                    mark(&mut protected, word, possessive)
                } else {
                    let mut marked = false;
                    for (index, piece) in pieces.iter().enumerate() {
                        if index > 0 {
                            protected.push(' ');
                        }
                        let final_piece = index + 1 == pieces.len();
                        marked = mark(
                            &mut protected,
                            piece,
                            if final_piece { possessive } else { "" },
                        );
                    }
                    marked
                };
                if marked && last {
                    end += possessive.len();
                }
                within = finish;
            }
            protected.push_str(&text[within..run.end()]);
        }
        protected.push_str(&text[end..]);
        (protected, marks)
    }
}

/// The words of `word` in camel case, divided where normalization divides them, before a capital that follows a lowercase letter and precedes another, so that "panelId" is "panel" and "Id", but "macOS" and "OpenXR" are whole.
fn camel_pieces(word: &str) -> Vec<&str> {
    let characters: Vec<(usize, char)> = word.char_indices().collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    for (position, &(index, capital)) in characters.iter().enumerate().skip(1) {
        let lower = |at: usize| {
            characters
                .get(at)
                .is_some_and(|(_, c)| c.is_ascii_lowercase())
        };
        if capital.is_ascii_uppercase() && lower(position - 1) && lower(position + 1) {
            pieces.push(&word[start..index]);
            start = index;
        }
    }
    pieces.push(&word[start..]);
    pieces
}

/// `word` as it is written in its markup, which the phonemizer tokenizes as it tokenizes text: as it is, except "id", which its tokenizer, following spaCy's, reads as "I'd", and "Id." as an abbreviation, losing the full stop that ends a sentence, so it is written "ID", which it reads as one word.
fn markup_text(word: &str) -> &str {
    if word.eq_ignore_ascii_case("id") {
        "ID"
    } else {
        word
    }
}

/// The phonemes of a possessive `'s` after `phonemes`, as misaki gives them after a word it knows: "s" after a voiceless consonant, "ᵻz" after a sibilant, and "z" otherwise.
fn possessive_suffix(phonemes: &str) -> &'static str {
    match phonemes.chars().last() {
        Some(last) if "ptkfθ".contains(last) => "s",
        Some(last) if "szʃʒʧʤ".contains(last) => "ᵻz",
        _ => "z",
    }
}

/// `text` with each placeholder of [`Pronunciations::protect`] replaced by its markup, from `marks`.
pub(crate) fn restore(text: &str, marks: &[String]) -> String {
    let mut restored = String::with_capacity(text.len());
    for symbol in text.chars() {
        // Only a placeholder's index is within `marks`, which has at most as many as there are placeholders.
        match marks.get(u32::from(symbol).wrapping_sub(*PLACEHOLDERS.start()) as usize) {
            Some(mark) => restored.push_str(mark),
            None => restored.push(symbol),
        }
    }
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A respelling's phonemes, as the words in capitals, so that tests need no phonemizer.
    fn shout(respelling: &str) -> String {
        respelling.to_uppercase()
    }

    fn parse(source: &str) -> Pronunciations {
        Pronunciations::parse(source, "test", shout).unwrap()
    }

    /// `text` as the phonemizer is given it, with markup in place of each word that has a pronunciation.
    fn marked(pronunciations: &Pronunciations, text: &str) -> String {
        let (protected, marks) = pronunciations.protect(text);
        restore(&protected, &marks)
    }

    #[test]
    fn reads_respellings_and_phonemes() {
        let pronunciations = parse(
            "readme = \"/ɹˈidmi/\"\ngithub = \" git hub \"\n\"sherpa-onnx\" = \"/ ʃˈɜɹpə ˈɑnɪks /\"\n",
        );
        assert_eq!(pronunciations.len(), 3);
        assert_eq!(pronunciations.words["readme"], "ɹˈidmi");
        assert_eq!(pronunciations.words["github"], "GIT HUB");
        assert_eq!(pronunciations.words["sherpa-onnx"], "ʃˈɜɹpə ˈɑnɪks");
    }

    #[test]
    fn leaves_out_entries_it_cannot_use() {
        // Each entry left out is logged.
        log::set_max_level(log::LevelFilter::Trace);
        let pronunciations = parse(
            "good = \"/ɡʊd/\"\n\"two words\" = \"tu\"\n\"-dash\" = \"dash\"\nnumber = 3\nempty = \"//\"\nblank = \"\"\nbracket = \"/a(b/\"\n[table]\nkey = \"value\"\n",
        );
        assert_eq!(pronunciations.len(), 1, "{pronunciations:?}");
        assert_eq!(pronunciations.words["good"], "ɡʊd");
        assert!(
            Pronunciations::parse("= nonsense", "test", shout)
                .unwrap_err()
                .starts_with("test is not TOML")
        );
    }

    #[test]
    fn marks_whole_words_in_any_case() {
        let pronunciations = parse(
            "readme = \"/ɹˈidmi/\"\nonnx = \"/ˈɑnɪks/\"\n\"sherpa-onnx\" = \"/ʃˈɜɹpə ˈɑnɪks/\"\n",
        );
        for (text, expected) in [
            (
                "Read the README, Readme and readme.",
                "Read the [README](/ɹˈidmi/), [Readme](/ɹˈidmi/) and [readme](/ɹˈidmi/).",
            ),
            // In punctuation and code.
            (
                "See `README.md` (README) and \"README\".",
                "See `[README](/ɹˈidmi/).md` ([README](/ɹˈidmi/)) and \"[README](/ɹˈidmi/)\".",
            ),
            // A run of words is marked whole, or else by its words.
            (
                "sherpa-onnx, sherpa_onnx and ONNX.",
                "[sherpa-onnx](/ʃˈɜɹpə ˈɑnɪks/), sherpa_[onnx](/ˈɑnɪks/) and [ONNX](/ˈɑnɪks/).",
            ),
            // Not within a word.
            ("READMEs and onnxruntime.", "READMEs and onnxruntime."),
            // With a possessive, said as misaki says one after a word it knows.
            (
                "The README's, ONNX’s and sherpa_onnx's, but not README'sx.",
                "The [README's](/ɹˈidmiz/), [ONNX’s](/ˈɑnɪksᵻz/) and sherpa_[onnx's](/ˈɑnɪksᵻz/), but not [README](/ɹˈidmi/)'sx.",
            ),
            ("ONNX_rt's and Bob's.", "[ONNX](/ˈɑnɪks/)_rt's and Bob's."),
            // Not "'s" with a further word joined to it.
            ("README's-based.", "[README](/ɹˈidmi/)'s-based."),
            // Words in camel case, as normalization divides them, but not a whole word with a pronunciation, or one with no words that have one.
            (
                "getReadmeOnnx's, ReadmeFile, ReadMe, macONNX and OpenXR.",
                "get [Readme](/ɹˈidmi/) [Onnx's](/ˈɑnɪksᵻz/), [Readme](/ɹˈidmi/) File, [ReadMe](/ɹˈidmi/), macONNX and OpenXR.",
            ),
            ("Nothing to mark.", "Nothing to mark."),
        ] {
            assert_eq!(marked(&pronunciations, text), expected, "{text}");
        }
    }

    #[test]
    fn says_a_possessive_as_misaki_does() {
        assert_eq!(possessive_suffix("ˌApˌikˈA"), "z");
        assert_eq!(possessive_suffix("ɡˈɪt hˈʌb"), "z");
        assert_eq!(possessive_suffix("ˌIdˈi"), "z");
        assert_eq!(possessive_suffix("wˈɑzəm"), "z");
        assert_eq!(possessive_suffix("ɹˈʌst sˈi"), "z");
        assert_eq!(possessive_suffix("ˈɪmpk"), "s");
        assert_eq!(possessive_suffix("ˈimæks"), "ᵻz");
        assert_eq!(possessive_suffix(""), "z");
    }

    #[test]
    fn writes_id_in_capitals_in_its_markup() {
        let pronunciations = parse("id = \"/ˌIdˈi/\"\n");
        assert_eq!(
            marked(&pronunciations, "Its id. Id? ID's id_x."),
            "Its [ID](/ˌIdˈi/). [ID](/ˌIdˈi/)? [ID's](/ˌIdˈiz/) [ID](/ˌIdˈi/)_x."
        );
    }

    #[test]
    fn placeholders_in_the_text_are_left_out() {
        let pronunciations = parse("readme = \"/ɹˈidmi/\"\n");
        let (protected, marks) = pronunciations.protect("a\u{E000}b README \u{F8FF}");
        assert_eq!(protected, "ab \u{E000} ");
        assert_eq!(restore(&protected, &marks), "ab [README](/ɹˈidmi/) ");
        // A placeholder without a mark, and other text, are left as they are.
        assert_eq!(restore("\u{E001} x", &marks), "\u{E001} x");
    }

    #[test]
    fn marks_no_more_words_than_it_has_placeholders() {
        let pronunciations = parse("a = \"/ɐ/\"\n");
        let count = (PLACEHOLDERS.end() - PLACEHOLDERS.start() + 1) as usize;
        let text = vec!["a"; count + 2].join(" ");
        let (protected, marks) = pronunciations.protect(&text);
        assert_eq!(marks.len(), count);
        assert!(protected.ends_with(" a a"), "the rest are left as they are");
    }

    #[test]
    fn loads_tomoris_and_a_servers_own() {
        log::set_max_level(log::LevelFilter::Trace);
        let directory =
            std::env::temp_dir().join(format!("tomori-pronunciations-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let built_in = Pronunciations::load(None, shout);
        assert!(built_in.words.contains_key("readme"));
        // A missing file adds nothing.
        let missing = Pronunciations::load(Some(&directory.join("missing.toml")), shout);
        assert_eq!(missing.len(), built_in.len());
        // A server's own add to and replace Tomori's.
        let file = directory.join("pronunciations.toml");
        std::fs::write(&file, "readme = \"read it\"\nhinoki = \"/hɪnˈOki/\"\n").unwrap();
        let own = Pronunciations::load(Some(&file), shout);
        assert_eq!(own.len(), built_in.len() + 1);
        assert_eq!(own.words["readme"], "READ IT");
        assert_eq!(own.words["hinoki"], "hɪnˈOki");
        // A file that is not TOML, or cannot be read, such as a directory, is left out.
        std::fs::write(&file, "readme = ").unwrap();
        assert_eq!(
            Pronunciations::load(Some(&file), shout).words["readme"],
            "ɹˈidmi"
        );
        assert_eq!(
            Pronunciations::load(Some(&directory), shout).len(),
            built_in.len()
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
