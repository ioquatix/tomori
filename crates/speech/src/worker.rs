// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Supervision of an engine worker process that speaks line-delimited JSON over its standard input and output.
//!
//! The worker prints `{"ready": true, ...}` once its model is loaded, then answers each request line, which carries an `id`, with one or more response lines carrying the same `id`. A response with an `error` field fails that request; otherwise the caller decides which response completes it.
//!
//! The process is started on first use and again after any failure: exit, malformed output, a write error or a timeout. A failure is returned to the request that observed it and is never retried silently. Pipes are read by standard threads; this can move to the scheduler's pipe adoption when it is available.

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

use crate::Error;

/// How to start a worker process.
#[derive(Clone, Debug)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub directory: Option<PathBuf>,
    /// How long to wait for the ready line after starting.
    pub startup_timeout: Duration,
    /// How long to wait for each response line.
    pub response_timeout: Duration,
    /// A short name used in log messages and errors.
    pub name: String,
}

impl WorkerCommand {
    pub fn new(name: impl Into<String>, program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            environment: Vec::new(),
            directory: None,
            startup_timeout: Duration::from_secs(120),
            response_timeout: Duration::from_secs(30),
            name: name.into(),
        }
    }

    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }
}

/// What to do after a response line.
pub(crate) enum Flow {
    Continue,
    Done,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Option<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A supervised worker process.
pub(crate) struct Worker {
    command: WorkerCommand,
    running: Option<Running>,
    next: u64,
    /// The most recent ready message, describing the loaded engine.
    ready: Option<Value>,
}

impl Worker {
    pub fn new(command: WorkerCommand) -> Self {
        Self {
            command,
            running: None,
            next: 0,
            ready: None,
        }
    }

    pub fn ready(&self) -> Option<&Value> {
        self.ready.as_ref()
    }

    /// Start the worker if it is not running, and wait until it is ready.
    pub fn start(&mut self) -> Result<(), Error> {
        if self.running.is_some() {
            return Ok(());
        }
        let command = &self.command;
        let mut process = Command::new(&command.program);
        process
            .args(&command.arguments)
            .envs(command.environment.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &command.directory {
            process.current_dir(directory);
        }
        let mut child = process.spawn().map_err(|error| {
            Error::Unavailable(format!(
                "could not start the {} worker {}: {error}",
                command.name,
                command.program.display()
            ))
        })?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (sender, lines) = mpsc::channel();
        let name = command.name.clone();
        thread::Builder::new()
            .name(format!("{name}-stdout"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) => {
                            if sender.send(Some(line)).is_err() {
                                return;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let _ = sender.send(None);
            })
            .expect("spawn worker reader");
        let name = command.name.clone();
        thread::Builder::new()
            .name(format!("{name}-stderr"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::warn!("{name} worker: {line}");
                }
            })
            .expect("spawn worker stderr reader");

        let mut running = Running {
            child,
            stdin,
            lines,
        };
        let timeout = self.command.startup_timeout;
        loop {
            let line = receive(&mut running, timeout, &self.command.name, "startup")?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                log::warn!("{} worker: ignoring output {line:?}", self.command.name);
                continue;
            };
            if message.get("ready").and_then(Value::as_bool) == Some(true) {
                log::info!("{} worker ready: {message}", self.command.name);
                self.ready = Some(message);
                break;
            }
            if let Some(error) = message.get("error").and_then(Value::as_str) {
                return Err(Error::Unavailable(format!(
                    "{} worker failed to start: {error}",
                    self.command.name
                )));
            }
        }
        self.running = Some(running);
        Ok(())
    }

    /// Send `request` (an object, to which an `id` is added) and pass each response line to `response` until it returns [`Flow::Done`].
    pub fn request(
        &mut self,
        mut request: Value,
        mut response: impl FnMut(&Value) -> Result<Flow, Error>,
    ) -> Result<(), Error> {
        self.start()?;
        self.next += 1;
        let id = self.next;
        request["id"] = json!(id);
        let result = self.exchange(id, &request, &mut response);
        if let Err(Error::Worker(message)) = &result {
            log::warn!(
                "{} worker failed and will be restarted: {message}",
                self.command.name
            );
            self.running = None;
        }
        result
    }

    fn exchange(
        &mut self,
        id: u64,
        request: &Value,
        response: &mut impl FnMut(&Value) -> Result<Flow, Error>,
    ) -> Result<(), Error> {
        let name = self.command.name.clone();
        let timeout = self.command.response_timeout;
        let running = self.running.as_mut().expect("worker is running");
        let mut line = serde_json::to_string(request).expect("requests serialize");
        line.push('\n');
        running
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| running.stdin.flush())
            .map_err(|error| Error::Worker(format!("{name} worker input closed: {error}")))?;
        loop {
            let line = receive(running, timeout, &name, "a response")?;
            let message: Value = serde_json::from_str(&line)
                .map_err(|_| Error::Worker(format!("{name} worker wrote {line:?}")))?;
            match message.get("id").and_then(Value::as_u64) {
                Some(received) if received == id => {}
                Some(received) if received < id => continue,
                _ => {
                    return Err(Error::Worker(format!(
                        "{name} worker answered out of order: {line}"
                    )));
                }
            }
            if let Some(error) = message.get("error").and_then(Value::as_str) {
                return Err(Error::Engine(error.to_owned()));
            }
            if let Flow::Done = response(&message)? {
                return Ok(());
            }
        }
    }
}

fn receive(
    running: &mut Running,
    timeout: Duration,
    name: &str,
    waiting: &str,
) -> Result<String, Error> {
    match running.lines.recv_timeout(timeout) {
        Ok(Some(line)) => Ok(line),
        Ok(None) | Err(RecvTimeoutError::Disconnected) => {
            let status = running
                .child
                .wait()
                .map(|status| status.to_string())
                .unwrap_or_else(|error| error.to_string());
            Err(Error::Worker(format!(
                "{name} worker exited ({status}) while waiting for {waiting}"
            )))
        }
        Err(RecvTimeoutError::Timeout) => Err(Error::Worker(format!(
            "{name} worker timed out waiting for {waiting}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell worker standing in for an engine: answers each request with its id, and exits on "crash".
    fn shell(script: &str) -> WorkerCommand {
        let mut command = WorkerCommand::new("test", "/bin/sh")
            .argument("-c")
            .argument(script);
        command.response_timeout = Duration::from_secs(5);
        command.startup_timeout = Duration::from_secs(5);
        command
    }

    const ECHO: &str = r#"
echo '{"ready": true}'
while read -r line; do
	id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
	case "$line" in
		*crash*) exit 3 ;;
		*fail*) echo "{\"id\": $id, \"error\": \"refused\"}" ;;
		*hang*) sleep 10 ;;
		*) echo "{\"id\": $id, \"part\": 1}"; echo "{\"id\": $id, \"done\": true}" ;;
	esac
done
"#;

    fn call(worker: &mut Worker, operation: &str) -> Result<usize, Error> {
        let mut parts = 0;
        worker.request(json!({ "op": operation }), |message| {
            if message.get("done").is_some() {
                Ok(Flow::Done)
            } else {
                parts += 1;
                Ok(Flow::Continue)
            }
        })?;
        Ok(parts)
    }

    #[test]
    fn exchanges_requests_and_responses() {
        let mut worker = Worker::new(shell(ECHO));
        assert_eq!(call(&mut worker, "hello"), Ok(1));
        assert_eq!(call(&mut worker, "again"), Ok(1));
        assert_eq!(worker.ready(), Some(&json!({ "ready": true })));
    }

    #[test]
    fn reports_engine_errors_without_restarting() {
        let mut worker = Worker::new(shell(ECHO));
        assert_eq!(
            call(&mut worker, "fail"),
            Err(Error::Engine("refused".into()))
        );
        assert!(worker.running.is_some());
        assert_eq!(call(&mut worker, "hello"), Ok(1));
    }

    #[test]
    fn restarts_after_a_crash() {
        let mut worker = Worker::new(shell(ECHO));
        assert!(matches!(call(&mut worker, "crash"), Err(Error::Worker(_))));
        assert!(worker.running.is_none());
        assert_eq!(call(&mut worker, "hello"), Ok(1));
    }

    #[test]
    fn restarts_after_a_timeout() {
        let mut command = shell(ECHO);
        command.response_timeout = Duration::from_millis(200);
        let mut worker = Worker::new(command);
        assert!(matches!(call(&mut worker, "hang"), Err(Error::Worker(_))));
        assert_eq!(call(&mut worker, "hello"), Ok(1));
    }

    #[test]
    fn reports_a_missing_program_as_unavailable() {
        let mut worker = Worker::new(WorkerCommand::new("test", "/nonexistent/worker"));
        assert!(matches!(
            call(&mut worker, "hello"),
            Err(Error::Unavailable(_))
        ));
    }
}
