// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Sessions driven against `fake-claude`, which replays scenarios condensed from real Claude Code output.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use backend::{
    AnswerValue, Backend, Decision, ErrorKind, Event, FieldKind, Part, Prompt, Role, Session,
    SessionSpec, WorkerState,
};
use backend_claude_code::{ClaudeCode, ClaudeCodeSession, Config};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(30);

struct Fixture {
    directory: PathBuf,
    log: PathBuf,
    backend: ClaudeCode,
}

impl Fixture {
    fn new(scenario: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "backend-claude-code-{scenario}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        let log = directory.join("stdin.jsonl");
        let scenario = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("{scenario}.scenario"));
        let backend = ClaudeCode::new(Config {
            executable: env!("CARGO_BIN_EXE_fake-claude").into(),
            arguments: vec![
                "--scenario".into(),
                scenario.display().to_string(),
                "--log".into(),
                log.display().to_string(),
            ],
            grace: Duration::from_millis(500),
            projects: Some(directory.join("projects")),
        });
        Self {
            directory,
            log,
            backend,
        }
    }

    async fn create(&self) -> ClaudeCodeSession {
        self.backend
            .create(SessionSpec {
                directory: self.directory.display().to_string(),
                title: "Test".into(),
            })
            .await
            .unwrap()
    }

    /// Process ids of the children the fake started, waiting until there are `count`.
    async fn children(&self, count: usize) -> Vec<u32> {
        loop {
            let written = fs::read_to_string(self.directory.join("children")).unwrap_or_default();
            // Only complete lines; the fake may be part way through writing one.
            let children: Vec<u32> = written
                .split_inclusive('\n')
                .filter_map(|line| line.strip_suffix('\n'))
                .map(|line| line.parse().unwrap())
                .collect();
            if children.len() >= count {
                return children;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Lines Tomori wrote to the agent's standard input.
    fn written(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Run an async test on its own thread, failing it if it does not finish in time.
fn run<F>(test: impl FnOnce() -> F + Send + 'static)
where
    F: Future<Output = ()>,
{
    let (done, finished) = mpsc::channel();
    let handle = thread::spawn(move || {
        pollster::block_on(test());
        let _ = done.send(());
    });
    match finished.recv_timeout(TIMEOUT) {
        Ok(()) => handle.join().unwrap(),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the test timed out"),
    }
}

/// Events up to and including the first one matching `until`.
async fn events_until(session: &ClaudeCodeSession, until: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        let event = session.next_event().await.unwrap();
        let done = until(&event);
        events.push(event);
        if done {
            return events;
        }
    }
}

fn prompt(key: &str, text: &str) -> Prompt {
    Prompt {
        key: key.into(),
        text: text.into(),
    }
}

async fn close(session: &ClaudeCodeSession) {
    session.close().await.unwrap();
    let events = events_until(session, |event| matches!(event, Event::Closed { .. })).await;
    assert_eq!(events.last(), Some(&Event::Closed { reason: None }));
    let error = session.next_event().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Closed);
}

#[test]
fn streams_text() {
    run(|| async {
        let fixture = Fixture::new("text");
        let session = fixture.create().await;
        let id = session.id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());

        session
            .send(prompt("one", "Reply with exactly: hello there"))
            .await
            .unwrap();
        // A key is delivered at most once.
        session
            .send(prompt("one", "Reply with exactly: hello there"))
            .await
            .unwrap();

        let events = events_until(&session, |event| *event == Event::Idle).await;
        let message = "msg_011CgZZNao2aW39N6nvDsFki".to_string();
        assert_eq!(
            events,
            vec![
                Event::Delivered { key: "one".into() },
                Event::Busy,
                Event::MessageStarted {
                    message: message.clone(),
                    role: Role::Assistant
                },
                Event::TextDelta {
                    message: message.clone(),
                    text: "Hello".into()
                },
                Event::TextDelta {
                    message: message.clone(),
                    text: " there".into()
                },
                Event::MessageCompleted {
                    message: message.clone()
                },
                Event::Idle,
            ]
        );

        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot.busy, Some(false));
        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, Role::User);
        assert_eq!(
            snapshot.messages[1].parts,
            vec![
                Part::Reasoning {
                    text: "The user wants a greeting.".into()
                },
                Part::Text {
                    text: "Hello there".into()
                },
            ]
        );

        close(&session).await;

        let written = fixture.written();
        assert_eq!(written[0]["type"], "control_request");
        assert_eq!(written[0]["request"]["subtype"], "initialize");
        let users: Vec<_> = written
            .iter()
            .filter(|line| line["type"] == "user")
            .collect();
        assert_eq!(users.len(), 1);
        assert_eq!(
            users[0]["message"]["content"],
            "Reply with exactly: hello there"
        );
        assert_eq!(users[0]["session_id"], id.as_str());
    });
}

#[test]
fn reports_a_reply_that_did_not_stream() {
    run(|| async {
        let fixture = Fixture::new("fallback");
        let session = fixture.create().await;
        session.send(prompt("one", "Read a.")).await.unwrap();

        let events = events_until(&session, |event| *event == Event::Idle).await;
        let first = "msg_fallback".to_string();
        let second = "msg_fallback_2".to_string();
        assert_eq!(
            events,
            vec![
                Event::Delivered { key: "one".into() },
                Event::Busy,
                Event::MessageStarted {
                    message: first.clone(),
                    role: Role::Assistant
                },
                Event::TextDelta {
                    message: first.clone(),
                    text: "I will read the file.".into()
                },
                Event::ToolStarted {
                    message: first.clone(),
                    tool: "Read".into(),
                    id: "toolu_fallback".into(),
                    input: serde_json::json!({"file_path": "/tmp/work/a.txt"}),
                },
                Event::ToolFinished {
                    id: "toolu_fallback".into(),
                    error: None
                },
                Event::MessageCompleted {
                    message: first.clone()
                },
                Event::MessageStarted {
                    message: second.clone(),
                    role: Role::Assistant
                },
                Event::TextDelta {
                    message: second.clone(),
                    text: "It says a.".into()
                },
                Event::TextDelta {
                    message: second.clone(),
                    text: " Done.".into()
                },
                Event::MessageCompleted {
                    message: second.clone()
                },
                Event::Idle,
            ]
        );

        let snapshot = session.snapshot().await.unwrap();
        assert_eq!(snapshot.messages.len(), 3);
        assert_eq!(
            snapshot.messages[1].parts,
            vec![
                Part::Reasoning {
                    text: "Read it first.".into()
                },
                Part::Text {
                    text: "I will read the file.".into()
                },
                Part::Tool {
                    id: "toolu_fallback".into(),
                    tool: "Read".into(),
                    input: serde_json::json!({"file_path": "/tmp/work/a.txt"}),
                    finished: true,
                },
            ]
        );
        assert!(snapshot.messages[1].completed);
        assert_eq!(
            snapshot.messages[2].parts,
            vec![
                Part::Text {
                    text: "It says a.".into()
                },
                Part::Text {
                    text: " Done.".into()
                },
            ]
        );

        close(&session).await;
    });
}

#[test]
fn denies_a_permission() {
    run(|| async {
        let fixture = Fixture::new("permission");
        let session = fixture.create().await;
        session.send(prompt("one", "Write b.txt")).await.unwrap();

        let events = events_until(&session, |event| {
            matches!(event, Event::PermissionRequested(_))
        })
        .await;
        assert!(events.contains(&Event::ToolStarted {
            message: "msg_011CgZZRqjYgXk1dBcgqnXeh".into(),
            tool: "Write".into(),
            id: "toolu_011Ui67Q2wPbtCsQNBUvsanY".into(),
            input: serde_json::json!({"file_path": "/tmp/work/b.txt", "content": "x\n"}),
        }));
        let Some(Event::PermissionRequested(permission)) = events.last() else {
            unreachable!()
        };
        assert_eq!(permission.id, "7802cc24-5d63-4554-9ff9-15db6eca3fc7");
        assert_eq!(permission.tool, "Write");
        assert_eq!(permission.description.as_deref(), Some("b.txt"));
        assert_eq!(
            session.snapshot().await.unwrap().permission.as_ref(),
            Some(permission)
        );

        session
            .decide(&permission.id, Decision::Deny)
            .await
            .unwrap();
        let error = session
            .decide(&permission.id, Decision::Deny)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);

        let events = events_until(&session, |event| *event == Event::Idle).await;
        assert_eq!(
            events[0],
            Event::PermissionResolved {
                id: permission.id.clone()
            }
        );
        assert!(events.contains(&Event::ToolFinished {
            id: "toolu_011Ui67Q2wPbtCsQNBUvsanY".into(),
            error: Some("The user denied this request.".into()),
        }));
        assert_eq!(session.snapshot().await.unwrap().permission, None);

        close(&session).await;

        let responses: Vec<_> = fixture
            .written()
            .into_iter()
            .filter(|line| line["type"] == "control_response")
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0]["response"],
            serde_json::json!({
                "subtype": "success",
                "request_id": "7802cc24-5d63-4554-9ff9-15db6eca3fc7",
                "response": {"behavior": "deny", "message": "The user denied this request."},
            })
        );
        assert_eq!(responses[1]["response"]["subtype"], "error");
        assert_eq!(responses[1]["response"]["request_id"], "hook-1");
    });
}

#[test]
fn answers_a_question() {
    run(|| async {
        let fixture = Fixture::new("question");
        let session = fixture.create().await;
        session.send(prompt("one", "Ask me.")).await.unwrap();

        let events = events_until(&session, |event| matches!(event, Event::QuestionAsked(_))).await;
        let Some(Event::QuestionAsked(question)) = events.last() else {
            unreachable!()
        };
        assert_eq!(question.id, "2d259efd-d203-472e-946d-dbabb4fde634");
        assert_eq!(question.title, "Colour");
        assert_eq!(question.fields.len(), 2);
        assert_eq!(question.fields[0].key, "Which colour do you prefer?");
        assert_eq!(question.fields[0].kind, FieldKind::Choice);
        assert!(question.fields[0].custom);
        assert_eq!(question.fields[0].options[1].label, "Blue");
        assert_eq!(
            question.fields[0].options[1].description.as_deref(),
            Some("Choose blue.")
        );
        assert_eq!(question.fields[1].kind, FieldKind::MultipleChoice);
        assert_eq!(
            session.snapshot().await.unwrap().question.as_ref(),
            Some(question)
        );

        let error = session
            .decide(&question.id, Decision::AllowOnce)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid);
        let error = session
            .answer(
                &question.id,
                vec![("Unknown?".into(), AnswerValue::Text("x".into()))],
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Invalid);

        session
            .answer(
                &question.id,
                vec![
                    (
                        "Which colour do you prefer?".into(),
                        AnswerValue::Choices(vec!["Blue".into()]),
                    ),
                    (
                        "Which shapes do you like?".into(),
                        AnswerValue::Choices(vec!["Circle".into(), "Triangle".into()]),
                    ),
                ],
            )
            .await
            .unwrap();

        let events = events_until(&session, |event| *event == Event::Idle).await;
        assert_eq!(
            events[0],
            Event::QuestionResolved {
                id: question.id.clone()
            }
        );
        assert!(events.contains(&Event::TextDelta {
            message: "msg_011CgZZQEfcWD4L7LLu6Wt4x".into(),
            text: "Blue".into(),
        }));

        close(&session).await;

        let written = fixture.written();
        let response = written
            .iter()
            .find(|line| line["type"] == "control_response")
            .unwrap();
        let response = &response["response"]["response"];
        assert_eq!(response["behavior"], "allow");
        assert_eq!(
            response["updatedInput"]["answers"],
            serde_json::json!({
                "Which colour do you prefer?": "Blue",
                "Which shapes do you like?": "Circle, Triangle",
            })
        );
        // The original questions are kept so the tool input still validates.
        assert_eq!(
            response["updatedInput"]["questions"][0]["question"],
            "Which colour do you prefer?"
        );
    });
}

#[test]
fn reports_workers() {
    run(|| async {
        let fixture = Fixture::new("worker");
        let session = fixture.create().await;
        let id = session.id();
        session
            .send(prompt("one", "Use a subagent."))
            .await
            .unwrap();

        let events = events_until(&session, |event| *event == Event::Idle).await;
        let workers: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::WorkersChanged { workers } => Some(workers.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(workers.len(), 1);
        assert_eq!(workers[0].len(), 1);
        let worker = &workers[0][0];
        assert_eq!(worker.id, "toolu_01So4iSESo7fNDLbyGJPYyDp");
        assert_eq!(worker.parent.as_deref(), Some(id.as_str()));
        assert_eq!(worker.title, "List files");
        assert_eq!(worker.state, WorkerState::Running);
        assert!(events.contains(&Event::ToolFinished {
            id: "toolu_01So4iSESo7fNDLbyGJPYyDp".into(),
            error: None,
        }));

        // The background subagent finishes after the turn, and its activity is not reported as the session's.
        let events = events_until(&session, |event| *event == Event::Idle).await;
        let Event::WorkersChanged { workers } = &events[0] else {
            panic!("expected the worker to complete first: {events:?}");
        };
        assert_eq!(workers[0].state, WorkerState::Completed);
        assert_eq!(events[1], Event::Busy);
        assert_eq!(
            events[2],
            Event::MessageStarted {
                message: "msg_report".into(),
                role: Role::Assistant
            }
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::ToolFinished { id, .. } if id == "toolu_01K7TBv3cxb7aKw5yggAUkU2"
        )));
        assert_eq!(
            session.snapshot().await.unwrap().workers[0].state,
            WorkerState::Completed
        );

        close(&session).await;
    });
}

#[test]
fn interrupts_a_turn() {
    run(|| async {
        let fixture = Fixture::new("interrupt");
        let session = fixture.create().await;
        session.send(prompt("one", "Count.")).await.unwrap();

        events_until(&session, |event| {
            matches!(event, Event::MessageStarted { .. })
        })
        .await;
        session.interrupt().await.unwrap();
        let events = events_until(&session, |event| *event == Event::Idle).await;
        assert_eq!(
            events,
            vec![
                Event::MessageCompleted {
                    message: "msg_011CgZZUXqqJeN1ScECoQXve".into()
                },
                Event::Idle,
            ]
        );

        close(&session).await;

        let interrupt = fixture
            .written()
            .into_iter()
            .find(|line| line["request"]["subtype"] == "interrupt")
            .unwrap();
        assert_eq!(interrupt["type"], "control_request");
    });
}

#[test]
fn closes_when_the_process_exits() {
    run(|| async {
        let fixture = Fixture::new("exit");
        let session = fixture.create().await;
        session.send(prompt("one", "Hello.")).await.unwrap();

        let events = events_until(&session, |event| matches!(event, Event::Closed { .. })).await;
        assert_eq!(events[0], Event::Delivered { key: "one".into() });
        let Some(Event::Closed {
            reason: Some(reason),
        }) = events.last()
        else {
            panic!("expected a reason: {events:?}");
        };
        assert!(reason.contains('3'), "{reason}");
        assert!(reason.contains("authentication failed"), "{reason}");

        assert_eq!(
            session.next_event().await.unwrap_err().kind,
            ErrorKind::Closed
        );
        let error = session.send(prompt("two", "Again.")).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Closed);
    });
}

#[test]
fn kills_a_process_that_does_not_exit() {
    run(|| async {
        let fixture = Fixture::new("hang");
        let session = fixture.create().await;
        close(&session).await;
    });
}

/// Wait for a process to disappear, returning whether it did within a few seconds.
#[cfg(unix)]
fn ends(pid: u32) -> bool {
    for _ in 0..500 {
        // A killed child stays a zombie until its parent is reaped, so this waits until the id is gone entirely.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(unix)]
#[test]
fn closing_stops_the_agents_tools() {
    run(|| async {
        let fixture = Fixture::new("orphan");
        let session = fixture.create().await;
        let children = fixture.children(1).await;
        close(&session).await;
        assert!(ends(children[0]), "the tool process survived");
    });
}

#[cfg(unix)]
#[test]
fn an_exiting_agent_leaves_no_tools_running() {
    run(|| async {
        let fixture = Fixture::new("leftover");
        let session = fixture.create().await;
        let children = fixture.children(1).await;
        close(&session).await;
        assert!(ends(children[0]), "the tool process survived");
    });
}

#[cfg(unix)]
#[test]
fn dropping_a_session_stops_the_agents_tools() {
    run(|| async {
        let fixture = Fixture::new("orphan");
        let session = fixture.create().await;
        let children = fixture.children(1).await;
        drop(session);
        assert!(ends(children[0]), "the tool process survived");
    });
}

#[cfg(unix)]
#[test]
fn the_closing_reason_includes_the_last_error_line() {
    run(|| async {
        let fixture = Fixture::new("late-error");
        let session = fixture.create().await;
        let events = events_until(&session, |event| matches!(event, Event::Closed { .. })).await;
        let Some(Event::Closed {
            reason: Some(reason),
        }) = events.last()
        else {
            panic!("expected a reason: {events:?}");
        };
        assert!(reason.ends_with("Error: authentication failed"), "{reason}");
    });
}

#[cfg(unix)]
#[test]
fn closes_promptly_when_a_stray_process_holds_the_output() {
    run(|| async {
        let fixture = Fixture::new("stray");
        let started = std::time::Instant::now();
        let session = fixture.create().await;
        let events = events_until(&session, |event| matches!(event, Event::Closed { .. })).await;
        let elapsed = started.elapsed();
        let children = fixture.children(1).await;
        unsafe { libc::kill(children[0] as libc::pid_t, libc::SIGKILL) };

        // Output written before the exit is still reported.
        assert_eq!(events, vec![Event::Busy, Event::Closed { reason: None }]);
        assert!(elapsed < Duration::from_secs(4), "closing took {elapsed:?}");
    });
}

#[test]
fn lists_and_attaches_sessions() {
    run(|| async {
        let fixture = Fixture::new("text");
        let directory = fixture.directory.display().to_string();
        assert!(
            fixture
                .backend
                .sessions(&directory)
                .await
                .unwrap()
                .is_empty()
        );

        // Claude Code names the transcript directory after the canonical path.
        let canonical = fs::canonicalize(&fixture.directory).unwrap();
        let name: String = canonical
            .display()
            .to_string()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let transcripts = fixture.directory.join("projects").join(name);
        fs::create_dir_all(&transcripts).unwrap();
        let titled = "0e22000d-8912-4fe7-9712-92b74537a3cc";
        fs::write(
            transcripts.join(format!("{titled}.jsonl")),
            concat!(
                r#"{"type":"queue-operation","operation":"enqueue","content":"Hello"}"#,
                "\n",
                r#"{"parentUuid":null,"type":"user","message":{"role":"user","content":"Ask me which colour I prefer.\nThanks."},"uuid":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Colour preference","sessionId":"0e22000d-8912-4fe7-9712-92b74537a3cc"}"#,
                "\n",
            ),
        )
        .unwrap();
        let untitled = "8a7e6b70-a4e3-4d4d-8df9-8b654500cc5f";
        fs::write(
            transcripts.join(format!("{untitled}.jsonl")),
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"Run the tests."},"uuid":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeef"}"#,
                "\n"
            ),
        )
        .unwrap();
        // Transcripts without a conversation are not offered.
        fs::write(
            transcripts.join("2e85a37c-e460-45c1-b2e1-be4aa4c1ef28.jsonl"),
            r#"{"type":"queue-operation","operation":"dequeue"}"#,
        )
        .unwrap();

        let mut sessions = fixture.backend.sessions(&directory).await.unwrap();
        sessions.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, titled);
        assert_eq!(sessions[0].title, "Colour preference");
        assert_eq!(sessions[1].id, untitled);
        assert_eq!(sessions[1].title, "Run the tests.");

        let error = match fixture
            .backend
            .attach("2e85a37c-0000-4000-8000-be4aa4c1ef28", &directory)
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("attached to a missing session"),
        };
        assert_eq!(error.kind, ErrorKind::NotFound);

        let session = fixture.backend.attach(titled, &directory).await.unwrap();
        assert_eq!(session.id(), titled);
        session.send(prompt("one", "Hello again.")).await.unwrap();
        events_until(&session, |event| *event == Event::Idle).await;
        close(&session).await;
    });
}

/// Drives the installed `claude` with one trivial prompt. Run with `cargo test -p backend-claude-code -- --ignored`.
#[test]
#[ignore = "runs the real claude CLI and uses the API"]
fn live_claude() {
    run(|| async {
        let directory =
            std::env::temp_dir().join(format!("backend-claude-code-live-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let backend = ClaudeCode::new(Config {
            arguments: vec!["--model".into(), "haiku".into()],
            ..Config::default()
        });
        let session = backend
            .create(SessionSpec {
                directory: directory.display().to_string(),
                title: String::new(),
            })
            .await
            .unwrap();
        session
            .send(prompt("one", "Reply with exactly: hello there"))
            .await
            .unwrap();
        let events = events_until(&session, |event| {
            matches!(event, Event::Idle | Event::Closed { .. })
        })
        .await;
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                Event::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            events.contains(&Event::Delivered { key: "one".into() }),
            "{events:?}"
        );
        assert!(text.to_lowercase().contains("hello there"), "{events:?}");
        close(&session).await;

        // The finished session is listed and can be resumed.
        let directory = directory.display().to_string();
        let sessions = backend.sessions(&directory).await.unwrap();
        assert!(
            sessions.iter().any(|summary| summary.id == session.id()),
            "{sessions:?}"
        );
        let resumed = backend.attach(&session.id(), &directory).await.unwrap();
        resumed
            .send(prompt("two", "Reply with exactly: again"))
            .await
            .unwrap();
        let events = events_until(&resumed, |event| {
            matches!(event, Event::Idle | Event::Closed { .. })
        })
        .await;
        assert!(
            events.contains(&Event::Delivered { key: "two".into() }),
            "{events:?}"
        );
        close(&resumed).await;
        let _ = fs::remove_dir_all(&directory);
    });
}
