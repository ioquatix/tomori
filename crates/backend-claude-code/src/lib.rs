// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The Claude Code backend for the Tomori server.
//!
//! Each session owns one long-lived `claude -p` process speaking the stream-json protocol on standard input and output.
//! Tool permissions and questions arrive as `can_use_tool` control requests and are answered only when a participant
//! acts. See `design/agent-backends.md`.

mod process;
mod protocol;
mod session;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use backend::{Capabilities, Error, ErrorKind, Result, SessionSpec, SessionSummary};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

pub use session::ClaudeCodeSession;

/// How to run `claude`.
#[derive(Clone, Debug)]
pub struct Config {
    /// The `claude` executable.
    pub executable: PathBuf,
    /// Extra arguments passed before the arguments the backend requires.
    pub arguments: Vec<String>,
    /// How long a closing session may take to exit. Its process group is terminated after half of it and killed after all of it.
    pub grace: Duration,
    /// Where Claude Code keeps session transcripts, normally `~/.claude/projects`.
    pub projects: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        let configuration = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")));
        Self {
            executable: PathBuf::from("claude"),
            arguments: Vec::new(),
            grace: Duration::from_secs(5),
            projects: configuration.map(|directory| directory.join("projects")),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClaudeCode {
    config: Config,
}

impl ClaudeCode {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    fn command(&self, directory: &str) -> Command {
        let mut command = Command::new(&self.config.executable);
        command.args(&self.config.arguments).args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            // Echo accepted user messages with their uuid, which reports delivery.
            "--replay-user-messages",
            // Send every permission prompt to this process as a `can_use_tool` control request.
            "--permission-prompt-tool",
            "stdio",
        ]);
        command.current_dir(directory);
        command
    }

    /// The directory holding transcripts for sessions started in `directory`. Claude Code names it after the canonical
    /// path, so symbolic links such as macOS's `/var` resolve first.
    fn transcripts(&self, directory: &str) -> Option<PathBuf> {
        let canonical = fs::canonicalize(directory)
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned))
            .unwrap_or_else(|| directory.to_owned());
        Some(
            self.config
                .projects
                .as_ref()?
                .join(project_name(&canonical)),
        )
    }
}

impl backend::Backend for ClaudeCode {
    type Session = ClaudeCodeSession;

    fn name(&self) -> &str {
        "claude-code"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            attach: self.config.projects.is_some(),
            interrupt: true,
            questions: true,
            workers: true,
        }
    }

    async fn sessions(&self, directory: &str) -> Result<Vec<SessionSummary>> {
        let Some(transcripts) = self.transcripts(directory) else {
            return Ok(Vec::new());
        };
        let Ok(entries) = fs::read_dir(&transcripts) else {
            return Ok(Vec::new());
        };

        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if uuid::Uuid::parse_str(id).is_err() {
                continue;
            }
            let Some(title) = transcript_title(&path) else {
                continue;
            };
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            sessions.push((
                modified,
                SessionSummary {
                    id: id.into(),
                    title,
                    directory: directory.into(),
                },
            ));
        }
        sessions.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        Ok(sessions.into_iter().map(|(_, summary)| summary).collect())
    }

    async fn create(&self, spec: SessionSpec) -> Result<ClaudeCodeSession> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut command = self.command(&spec.directory);
        command.args(["--session-id", &id]);
        if !spec.title.is_empty() {
            command.args(["--name", &spec.title]);
        }
        ClaudeCodeSession::spawn(command, id, self.config.grace)
    }

    async fn attach(&self, id: &str, directory: &str) -> Result<ClaudeCodeSession> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("no session {id:?}"),
            ));
        }
        if let Some(transcripts) = self.transcripts(directory)
            && !transcripts.join(format!("{id}.jsonl")).is_file()
        {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("no session {id:?} in {directory}"),
            ));
        }
        let mut command = self.command(directory);
        command.args(["--resume", id]);
        ClaudeCodeSession::spawn(command, id.into(), self.config.grace)
    }
}

/// The longest project directory name Claude Code uses before shortening it with a hash.
const PROJECT_NAME_LENGTH_LIMIT: usize = 200;

/// The name Claude Code gives a project's transcript directory, as Claude Code 2.1 computes it from the canonical path:
/// the NFC normalized path with every UTF-16 code unit other than an ASCII letter or digit replaced by `-`, and a name
/// longer than 200 units cut to 200 and followed by `-` and a hash of the path.
fn project_name(directory: &str) -> String {
    let directory: String = directory.nfc().collect();
    let name: String = directory
        .encode_utf16()
        .map(|unit| match char::from_u32(unit.into()) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    if name.len() <= PROJECT_NAME_LENGTH_LIMIT {
        return name;
    }
    format!(
        "{}-{}",
        &name[..PROJECT_NAME_LENGTH_LIMIT],
        base36(path_hash(&directory).unsigned_abs())
    )
}

/// Claude Code's path hash: the 32-bit Java string hash of the path's UTF-16 code units.
fn path_hash(path: &str) -> i32 {
    path.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(unit.into())
    })
}

fn base36(mut value: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit(value % 36, 36).expect("a base 36 digit"));
        value /= 36;
        if value == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// The session's title: one set by the user, else the generated one, else its first prompt. `None` for transcripts
/// without a conversation.
fn transcript_title(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut custom = None;
    let mut generated = None;
    let mut prompt = None;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        // Only parse the few record types that matter; transcripts can be large.
        if line.starts_with(r#"{"type":"custom-title""#) {
            custom = record_field(&line, "customTitle");
        } else if line.starts_with(r#"{"type":"ai-title""#) {
            generated = record_field(&line, "aiTitle");
        } else if prompt.is_none() && line.contains(r#""type":"user""#) {
            prompt = first_prompt(&line);
        }
    }
    custom.or(generated).or(prompt)
}

fn record_field(line: &str, key: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    Some(value.get(key)?.as_str()?.to_owned())
}

fn first_prompt(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "user" || value["isMeta"].as_bool() == Some(true) {
        return None;
    }
    let text = match &value["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .find(|block| block["type"] == "text")?
            .get("text")?
            .as_str()?
            .to_owned(),
        _ => return None,
    };
    let text = text.lines().next()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_replace_punctuation() {
        assert_eq!(
            project_name("/Users/samuel/Developer/tomori/.claude/worktrees/a_b"),
            "-Users-samuel-Developer-tomori--claude-worktrees-a-b"
        );
    }

    #[test]
    fn project_names_replace_each_utf16_code_unit() {
        // A decomposed accent composes to one unit; a character outside the Basic Multilingual Plane is two.
        assert_eq!(project_name("/tmp/cafe\u{301}/\u{1F600}"), "-tmp-caf----");
        assert_eq!(project_name("/tmp/日本"), "-tmp---");
    }

    #[test]
    fn long_project_names_end_with_a_hash() {
        // Values computed with Claude Code 2.1.311's own functions.
        assert_eq!(path_hash(""), 0);
        assert_eq!(path_hash("hello"), 99162322);
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");

        let directory = format!("/tmp/{}", "a".repeat(200));
        assert_eq!(path_hash(&directory), -628621273);
        assert_eq!(
            project_name(&directory),
            format!("-tmp-{}-ae9ja1", "a".repeat(195))
        );
        let directory = format!(
            "/Users/samuel/{}cafe\u{301}-\u{1F600}",
            "projects/".repeat(25)
        );
        assert_eq!(
            project_name(&directory),
            format!("-Users-samuel-{}projec-se2com", "projects-".repeat(20))
        );

        let exactly = format!("/{}", "a".repeat(199));
        assert_eq!(project_name(&exactly).len(), 200);
    }
}
