// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::process::{Command, ExitStatus};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use backend::{AnswerValue, Decision, Error, ErrorKind, Event, Prompt, Result, Snapshot};
use serde_json::Value;

use crate::process::{Output, Process};
use crate::protocol::State;

/// Everything [`Session::next_event`](backend::Session::next_event) waits on, in arrival order.
enum Input {
    Process(Output),
    /// An event raised by a command on this session, such as a resolved permission.
    Local(Event),
}

#[derive(Default)]
struct Lifecycle {
    eof: bool,
    exit: Option<ExitStatus>,
    /// [`Event::Closed`] has been queued; nothing follows it.
    closed: bool,
}

/// One `claude` process driving one Claude Code session.
pub struct ClaudeCodeSession {
    process: Process,
    input: async_channel::Receiver<Input>,
    local: async_channel::Sender<Input>,
    state: Mutex<State>,
    events: Mutex<VecDeque<Event>>,
    lifecycle: Mutex<Lifecycle>,
    closing: AtomicBool,
}

impl ClaudeCodeSession {
    pub(crate) fn spawn(command: Command, session: String, grace: Duration) -> Result<Self> {
        let (sender, input) = async_channel::unbounded();
        let local = sender.clone();
        let process = Process::spawn(command, grace, move |output| {
            let _ = sender.try_send(Input::Process(output));
        })
        .map_err(|error| {
            Error::new(
                ErrorKind::Unavailable,
                format!("cannot start claude: {error}"),
            )
        })?;

        let mut state = State::new(session);
        process.write(state.control_request("initialize").to_string());

        Ok(Self {
            process,
            input,
            local,
            state: Mutex::new(state),
            events: Mutex::new(VecDeque::new()),
            lifecycle: Mutex::new(Lifecycle::default()),
            closing: AtomicBool::new(false),
        })
    }

    fn write(&self, line: Value) {
        self.process.write(line.to_string());
    }

    fn ensure_open(&self) -> Result<()> {
        let lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.closed || lifecycle.exit.is_some() || self.closing.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::Closed, "the session has closed"));
        }
        Ok(())
    }

    fn resolve(&self, result: Result<(Value, Event)>) -> Result<()> {
        let (response, event) = result?;
        self.write(response);
        let _ = self.local.try_send(Input::Local(event));
        Ok(())
    }

    fn receive(&self, input: Input) {
        let events = match input {
            Input::Local(event) => vec![event],
            Input::Process(Output::Line(line)) => {
                let effects = self.state.lock().unwrap().handle_line(&line);
                for line in effects.writes {
                    self.write(line);
                }
                effects.events
            }
            Input::Process(Output::Eof) => {
                self.lifecycle.lock().unwrap().eof = true;
                self.finish()
            }
            Input::Process(Output::Exited(status)) => {
                self.lifecycle.lock().unwrap().exit = Some(status);
                self.finish()
            }
        };
        self.events.lock().unwrap().extend(events);
    }

    /// The closing event, once the process has exited and all of its output has been handled.
    fn finish(&self) -> Vec<Event> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        let Some(status) = lifecycle.exit else {
            return Vec::new();
        };
        if !lifecycle.eof || lifecycle.closed {
            return Vec::new();
        }
        lifecycle.closed = true;

        let reason = if self.closing.load(Ordering::SeqCst) || status.success() {
            None
        } else {
            let mut reason = format!("claude exited with {status}");
            if let Some(line) = self.process.stderr_tail().last() {
                reason.push_str(": ");
                reason.push_str(line);
            }
            Some(reason)
        };
        vec![Event::Closed { reason }]
    }
}

impl backend::Session for ClaudeCodeSession {
    fn id(&self) -> String {
        self.state.lock().unwrap().session().to_owned()
    }

    async fn next_event(&self) -> Result<Event> {
        loop {
            if let Some(event) = self.events.lock().unwrap().pop_front() {
                return Ok(event);
            }
            if self.lifecycle.lock().unwrap().closed {
                return Err(Error::new(ErrorKind::Closed, "the session has closed"));
            }
            // The session holds a sender, so the channel never closes while it is waited on.
            match self.input.recv().await {
                Ok(input) => self.receive(input),
                Err(_) => return Err(Error::new(ErrorKind::Closed, "the session has closed")),
            }
        }
    }

    async fn snapshot(&self) -> Result<Snapshot> {
        Ok(self.state.lock().unwrap().snapshot())
    }

    async fn send(&self, prompt: Prompt) -> Result<()> {
        self.ensure_open()?;
        if let Some(line) = self.state.lock().unwrap().prompt(prompt) {
            self.write(line);
        }
        Ok(())
    }

    async fn decide(&self, permission: &str, decision: Decision) -> Result<()> {
        self.ensure_open()?;
        let result = self.state.lock().unwrap().decide(permission, decision);
        self.resolve(result)
    }

    async fn answer(&self, question: &str, answers: Vec<(String, AnswerValue)>) -> Result<()> {
        self.ensure_open()?;
        let result = self.state.lock().unwrap().answer(question, answers);
        self.resolve(result)
    }

    async fn interrupt(&self) -> Result<()> {
        self.ensure_open()?;
        let request = self.state.lock().unwrap().control_request("interrupt");
        self.write(request);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closing.store(true, Ordering::SeqCst);
        self.process.terminate();
        self.process.wait_exit().await;
        Ok(())
    }
}
