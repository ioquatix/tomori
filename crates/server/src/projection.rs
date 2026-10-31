// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Text shown to participants, derived from structured backend facts. Backends never produce UI text; see `design/agent-backends.md`.

use std::fmt::Write;

use backend::{AnswerValue, Field, FieldKind, Permission, Question};

/// Longest tool input summary shown in an approval.
const INPUT_SUMMARY_LIMIT: usize = 600;

const NUMBERS: [&str; 11] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// The title and text of a pending tool permission.
pub fn approval(permission: &Permission) -> protocol::Approval {
    let mut text = String::new();
    if let Some(description) = permission
        .description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        text.push_str(description.trim());
        text.push('\n');
    }
    let summary = input_summary(&permission.input);
    if !summary.is_empty() {
        text.push_str(&summary);
        text.push('\n');
    }
    text.push_str("\nA: allow once. B: deny.");
    protocol::Approval {
        id: permission.id.clone(),
        title: permission.tool.clone(),
        text,
    }
}

/// A short, readable rendering of a tool's input: well-known fields first, otherwise compact JSON.
fn input_summary(input: &serde_json::Value) -> String {
    const KEYS: [&str; 7] = [
        "command",
        "file_path",
        "path",
        "url",
        "pattern",
        "query",
        "description",
    ];
    let summary = match input {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(map) => {
            let known: Vec<&str> = KEYS
                .iter()
                .filter_map(|key| map.get(*key).and_then(|value| value.as_str()))
                .collect();
            if known.is_empty() {
                input.to_string()
            } else {
                known.join("\n")
            }
        }
        other => other.to_string(),
    };
    truncate(&summary, INPUT_SUMMARY_LIMIT)
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_owned(),
    }
}

/// Whether every field of a question can be answered in Tomori.
pub fn supported(question: &Question) -> bool {
    !question.fields.is_empty()
        && question
            .fields
            .iter()
            .all(|field| field.kind != FieldKind::Unsupported)
}

/// The first field without an answer.
pub fn current_field<'a>(
    question: &'a Question,
    answers: &[(String, AnswerValue)],
) -> Option<&'a Field> {
    question
        .fields
        .iter()
        .find(|field| !answers.iter().any(|(key, _)| *key == field.key))
}

/// The prompt for the current field of a partially answered question.
pub fn question(question: &Question, answers: &[(String, AnswerValue)]) -> protocol::Question {
    let supported = supported(question);
    let field = current_field(question, answers);
    let text = match field {
        Some(field) if supported => {
            let mut text = format!(
                "{}/{}: {}\n",
                answers.len() + 1,
                question.fields.len(),
                if field.title.is_empty() {
                    &field.key
                } else {
                    &field.title
                }
            );
            if let Some(description) = &field.description {
                text.push_str(description);
                text.push('\n');
            }
            for (index, option) in field.options.iter().enumerate() {
                let _ = write!(text, "{}. {}", index + 1, option.label);
                if let Some(description) = &option.description {
                    let _ = write!(text, ": {description}");
                }
                text.push('\n');
            }
            text.push_str("Hold X and say your answer");
            if !field.options.is_empty() {
                text.push_str(" or its number");
            }
            text.push('.');
            text
        }
        _ => "Complete this question outside Tomori; it has fields that cannot be answered here."
            .to_owned(),
    };
    protocol::Question {
        id: question.id.clone(),
        title: question.title.clone(),
        text,
        field: field.map(|field| field.key.clone()).unwrap_or_default(),
        supported,
    }
}

/// Lowercase, with runs of anything other than letters and digits collapsed to single spaces.
fn normal(text: &str) -> String {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn without_final_punctuation(text: &str) -> &str {
    text.trim().trim_end_matches(['.', '!', '?'])
}

/// Interpret spoken or typed text as the answer to one field.
pub fn answer_value(field: &Field, text: &str) -> Result<AnswerValue, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The answer was empty. No answer was submitted.".to_owned());
    }
    match field.kind {
        FieldKind::Text => Ok(AnswerValue::Text(text.to_owned())),
        FieldKind::Choice => match option_label(field, text)? {
            Some(label) => Ok(AnswerValue::Choices(vec![label])),
            None => Ok(AnswerValue::Text(text.to_owned())),
        },
        FieldKind::MultipleChoice => {
            let mut labels = Vec::new();
            for part in split_list(without_final_punctuation(text)) {
                match option_label(field, part)? {
                    Some(label) if !labels.contains(&label) => labels.push(label),
                    Some(_) => {}
                    None => return Err(OPTION_ERROR.to_owned()),
                }
            }
            if labels.is_empty() {
                return Err(OPTION_ERROR.to_owned());
            }
            Ok(AnswerValue::Choices(labels))
        }
        FieldKind::Unsupported => {
            Err("Complete this question outside Tomori. No answer was submitted.".to_owned())
        }
    }
}

const OPTION_ERROR: &str = "Say an option's label or number. No answer was submitted.";

/// Split "one, two and three" into its parts.
fn split_list(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for part in text.split(',') {
        let mut rest = part;
        while let Some(index) = find_word(rest, "and") {
            parts.push(&rest[..index]);
            rest = &rest[index + 3..];
        }
        parts.push(rest);
    }
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// The byte index of `word` as a whole word in `text`, ignoring ASCII case.
fn find_word(text: &str, word: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut start = 0;
    while let Some(offset) = lower[start..].find(word) {
        let index = start + offset;
        let before = lower[..index].chars().next_back();
        let after = lower[index + word.len()..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return Some(index);
        }
        start = index + word.len();
    }
    None
}

/// Match text to an option by label or by number. `None` means free text, allowed only for custom fields.
fn option_label(field: &Field, text: &str) -> Result<Option<String>, String> {
    let input = normal(text);
    let exact: Vec<&str> = field
        .options
        .iter()
        .filter(|option| normal(&option.label) == input)
        .map(|option| option.label.as_str())
        .collect();
    if let [label] = exact[..] {
        return Ok(Some(label.to_owned()));
    }
    let index = input
        .parse::<usize>()
        .ok()
        .or_else(|| NUMBERS.iter().position(|number| *number == input));
    if let Some(index) = index.filter(|index| (1..=field.options.len()).contains(index)) {
        return Ok(Some(field.options[index - 1].label.clone()));
    }
    if field.custom {
        return Ok(None);
    }
    Err(OPTION_ERROR.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend::FieldOption;

    fn field(kind: FieldKind, custom: bool) -> Field {
        Field {
            key: "colour".into(),
            title: "Colour".into(),
            description: Some("Pick one.".into()),
            kind,
            options: ["Red", "Light blue"]
                .into_iter()
                .map(|label| FieldOption {
                    label: label.into(),
                    description: None,
                })
                .collect(),
            custom,
        }
    }

    #[test]
    fn choices_match_labels_and_numbers() {
        let choice = field(FieldKind::Choice, false);
        assert_eq!(
            answer_value(&choice, "light Blue."),
            Ok(AnswerValue::Choices(vec!["Light blue".into()]))
        );
        assert_eq!(
            answer_value(&choice, "two"),
            Ok(AnswerValue::Choices(vec!["Light blue".into()]))
        );
        assert_eq!(
            answer_value(&choice, "1"),
            Ok(AnswerValue::Choices(vec!["Red".into()]))
        );
        assert!(answer_value(&choice, "green").is_err());
        assert!(answer_value(&choice, "3").is_err());
        assert_eq!(
            answer_value(&field(FieldKind::Choice, true), "green"),
            Ok(AnswerValue::Text("green".into()))
        );
    }

    #[test]
    fn multiple_choices_split_on_commas_and_and() {
        let multiple = field(FieldKind::MultipleChoice, false);
        assert_eq!(
            answer_value(&multiple, "red and light blue, one."),
            Ok(AnswerValue::Choices(vec![
                "Red".into(),
                "Light blue".into()
            ]))
        );
        assert!(answer_value(&multiple, "red and green").is_err());
    }

    #[test]
    fn unsupported_and_empty_answers_are_rejected() {
        assert!(answer_value(&field(FieldKind::Unsupported, false), "red").is_err());
        assert!(answer_value(&field(FieldKind::Text, false), "  ").is_err());
    }

    #[test]
    fn questions_prompt_for_the_next_unanswered_field() {
        let mut second = field(FieldKind::Text, false);
        second.key = "name".into();
        second.title = "Name".into();
        second.description = None;
        second.options.clear();
        let question_value = Question {
            id: "q".into(),
            title: "Setup".into(),
            fields: vec![field(FieldKind::Choice, false), second],
        };
        let first = question(&question_value, &[]);
        assert_eq!(first.field, "colour");
        assert!(first.supported);
        assert_eq!(
            first.text,
            "1/2: Colour\nPick one.\n1. Red\n2. Light blue\nHold X and say your answer or its number."
        );
        let next = question(
            &question_value,
            &[("colour".into(), AnswerValue::Choices(vec!["Red".into()]))],
        );
        assert_eq!(next.field, "name");
        assert_eq!(next.text, "2/2: Name\nHold X and say your answer.");
    }

    #[test]
    fn approvals_summarize_known_tool_inputs() {
        let permission = Permission {
            id: "p".into(),
            tool: "Bash".into(),
            input: serde_json::json!({"command": "ls -la", "timeout": 5}),
            description: Some("List files".into()),
        };
        let approval = approval(&permission);
        assert_eq!(approval.title, "Bash");
        assert_eq!(
            approval.text,
            "List files\nls -la\n\nA: allow once. B: deny."
        );
    }
}
