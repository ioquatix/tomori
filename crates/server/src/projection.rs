// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Text shown to participants, derived from structured backend facts. Backends never produce UI text; see `design/agent-backends.md`.

use std::fmt::Write;

use backend::{AnswerValue, Field, FieldKind, Grant, GrantScope, Permission, Question};

use crate::limits;

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
        options: options(permission),
    }
}

/// The most standing options shown with one approval.
const OPTION_COUNT_LIMIT: usize = 4;

/// Longest option label, in characters. Like Claude Code's terminal, an option too long to describe in full is not
/// offered, rather than granting something its label leaves out.
const OPTION_LABEL_LIMIT: usize = 400;

/// The standing options participants may choose for `permission`. Only these can be granted.
pub fn options(permission: &Permission) -> Vec<protocol::ApprovalOption> {
    permission
        .options
        .iter()
        .filter(|option| !option.grants.is_empty())
        .map(|option| protocol::ApprovalOption {
            id: option.id.clone(),
            label: grant_label(&option.grants),
        })
        .filter(|option| option.label.chars().count() <= OPTION_LABEL_LIMIT)
        .take(OPTION_COUNT_LIMIT)
        .collect()
}

/// A spoken-style label for a standing option, naming everything it grants and where it is kept, in the manner of
/// Claude Code's terminal dialog: "Yes, allow all edits during this session" or "Yes, and don't ask again for
/// Bash(make) in this project".
pub fn grant_label(grants: &[Grant]) -> String {
    // Grants of one kind and scope are named together, in the order they first appear.
    let mut groups: Vec<(&'static str, GrantScope, Vec<String>)> = Vec::new();
    for grant in grants {
        let (kind, scope, name) = match grant {
            Grant::Rule {
                tool,
                content,
                scope,
            } => (
                "rule",
                *scope,
                match content {
                    Some(content) => format!("{tool}({content})"),
                    None => tool.clone(),
                },
            ),
            Grant::Directory { path, scope } => ("directory", *scope, path.clone()),
            Grant::Mode { mode, scope } => ("mode", *scope, mode.clone()),
        };
        match groups
            .iter_mut()
            .find(|(other, within, _)| *other == kind && *within == scope)
        {
            Some((_, _, names)) => names.push(name),
            None => groups.push((kind, scope, vec![name])),
        }
    }

    let parts: Vec<String> = groups
        .iter()
        .map(|(kind, scope, names)| {
            let place = match scope {
                GrantScope::Session => "during this session",
                GrantScope::Local => "in this project",
                GrantScope::Project => "in this project for everyone (shared project settings)",
                GrantScope::User => "in any project (user settings)",
            };
            let names = names.join(" and ");
            match *kind {
                "rule" => format!("don't ask again for {names} {place}"),
                "directory" => format!("allow access to {names} {place}"),
                _ if names == "acceptEdits" => format!("allow all edits {place}"),
                _ => format!("switch to {names} mode {place}"),
            }
        })
        .collect();
    match parts.as_slice() {
        [] => "Yes".to_owned(),
        [only] if only.starts_with("don't") => format!("Yes, and {only}"),
        [only] => format!("Yes, {only}"),
        _ => format!("Yes, and {}", parts.join("; and ")),
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
        field: field
            .map(|field| field_name(question, field))
            .unwrap_or_default(),
        supported,
    }
}

/// How a field is named to clients, who name it again in their answers: its key, or for a key longer than [`limits::FIELD_BYTES`], such as a long question used as its own key, its position, so that snapshots stay small.
pub fn field_name(question: &Question, field: &Field) -> String {
    if field.key.len() <= limits::FIELD_BYTES {
        return field.key.clone();
    }
    let position = question
        .fields
        .iter()
        .position(|candidate| std::ptr::eq(candidate, field))
        .unwrap_or_default();
    format!("field {}", position + 1)
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
            options: Vec::new(),
        };
        let approval = approval(&permission);
        assert_eq!(approval.title, "Bash");
        assert_eq!(
            approval.text,
            "List files\nls -la\n\nA: allow once. B: deny."
        );
        assert!(approval.options.is_empty());
    }

    #[test]
    fn grant_labels_name_what_is_granted_and_where() {
        use backend::{Grant, GrantScope};

        let rule = |content: Option<&str>, scope| Grant::Rule {
            tool: "Bash".into(),
            content: content.map(Into::into),
            scope,
        };
        assert_eq!(
            grant_label(&[Grant::Mode {
                mode: "acceptEdits".into(),
                scope: GrantScope::Session
            }]),
            "Yes, allow all edits during this session"
        );
        assert_eq!(
            grant_label(&[
                rule(Some("touch marker.txt"), GrantScope::Local),
                Grant::Directory {
                    path: "/work".into(),
                    scope: GrantScope::Session
                },
            ]),
            "Yes, and don't ask again for Bash(touch marker.txt) in this project; and allow access to /work during this session"
        );
        assert_eq!(
            grant_label(&[
                rule(Some("make:*"), GrantScope::User),
                rule(None, GrantScope::User)
            ]),
            "Yes, and don't ask again for Bash(make:*) and Bash in any project (user settings)"
        );
        assert_eq!(
            grant_label(&[Grant::Mode {
                mode: "plan".into(),
                scope: GrantScope::Project
            }]),
            "Yes, switch to plan mode in this project for everyone (shared project settings)"
        );
    }

    #[test]
    fn options_that_cannot_be_described_are_not_offered() {
        use backend::{Grant, GrantScope, PermissionOption};

        let option = |id: &str, content: &str| PermissionOption {
            id: id.into(),
            grants: vec![Grant::Rule {
                tool: "Bash".into(),
                content: Some(content.into()),
                scope: GrantScope::Local,
            }],
        };
        let mut permission = Permission {
            id: "p".into(),
            tool: "Bash".into(),
            input: serde_json::Value::Null,
            description: None,
            options: vec![
                PermissionOption {
                    id: "empty".into(),
                    grants: Vec::new(),
                },
                option("long", &"x".repeat(OPTION_LABEL_LIMIT)),
                option("make", "make"),
            ],
        };
        for index in 0..OPTION_COUNT_LIMIT {
            permission
                .options
                .push(option(&format!("extra-{index}"), "ls"));
        }
        let ids: Vec<String> = options(&permission)
            .into_iter()
            .map(|option| option.id)
            .collect();
        assert_eq!(ids, ["make", "extra-0", "extra-1", "extra-2"]);
    }
}
