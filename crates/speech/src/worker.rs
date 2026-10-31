// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Supervision of an engine worker process that speaks line-delimited JSON over its standard input and output.
//!
//! The worker prints `{"ready": true, ...}` once its model is loaded, then answers each request line, which carries an `id`, with one or more response lines carrying the same `id`. A response with an `error` field fails that request; otherwise the caller decides which response completes it.
//!
//! The process is started on first use and again after any failure: exit, malformed output (including output that is not UTF-8), a write error or a timeout. A failure is returned to the request that observed it and is never retried silently. A failed process is killed and reaped within a bound, so no failure can block the caller indefinitely. Pipes are read by standard threads; this can move to the scheduler's pipe adoption when it is available.

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
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
    /// How long to wait for the ready line after starting, however much other output arrives first.
    pub startup_timeout: Duration,
    /// How long to wait for each response line. It always applies: a worker that stops writing without exiting is killed.
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
        reap(&mut self.child, REAP_LIMIT);
    }
}

/// How long a worker that closed its output may take to exit by itself before it is killed.
const EXIT_GRACE: Duration = Duration::from_millis(200);

/// How long to wait for a killed worker to be reaped. A process that cannot be reaped in this time is left to the operating system rather than blocking the caller.
const REAP_LIMIT: Duration = Duration::from_secs(2);

/// Wait up to `limit` for `child` to exit, returning its status if it did.
fn reap(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => return None,
        }
    }
}

/// Stop a worker whose output has ended: let it exit by itself briefly, then kill it. Returns its exit status if it could be reaped.
fn stop(child: &mut Child) -> Option<ExitStatus> {
    if let Some(status) = reap(child, EXIT_GRACE) {
        return Some(status);
    }
    let _ = child.kill();
    reap(child, REAP_LIMIT)
}

/// Each line of `input` up to its end, without the line ending. Bytes that are not UTF-8 are replaced rather than ending the stream, so the reader keeps draining the pipe.
fn lines_lossy(input: impl Read, mut line: impl FnMut(String) -> bool) {
    let mut input = BufReader::new(input);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match input.read_until(b'\n', &mut buffer) {
            Ok(0) => return,
            Ok(_) => {
                if buffer.last() == Some(&b'\n') {
                    buffer.pop();
                    if buffer.last() == Some(&b'\r') {
                        buffer.pop();
                    }
                }
                if !line(String::from_utf8_lossy(&buffer).into_owned()) {
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
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
                // Output that is not valid UTF-8 arrives as a line that does not parse, which fails the request rather than ending the stream.
                lines_lossy(stdout, |line| sender.send(Some(line)).is_ok());
                let _ = sender.send(None);
            })
            .expect("spawn worker reader");
        let name = command.name.clone();
        thread::Builder::new()
            .name(format!("{name}-stderr"))
            .spawn(move || {
                // Drain until the end, whatever the bytes, so the worker never blocks writing diagnostics.
                lines_lossy(stderr, |line| {
                    log::warn!("{name} worker: {line}");
                    true
                });
            })
            .expect("spawn worker stderr reader");

        let mut running = Running {
            child,
            stdin,
            lines,
        };
        let deadline = Instant::now() + self.command.startup_timeout;
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
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
            let status = match stop(&mut running.child) {
                Some(status) => format!("exited ({status})"),
                None => "closed its output and could not be stopped".to_owned(),
            };
            Err(Error::Worker(format!(
                "{name} worker {status} while waiting for {waiting}"
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

    /// Call `operation` on a fresh worker running `script`, asserting that it fails as a worker error well before the response timeout.
    fn fails_promptly(script: &str) -> String {
        let mut command = shell(script);
        command.response_timeout = Duration::from_secs(20);
        let mut worker = Worker::new(command);
        let started = Instant::now();
        let result = call(&mut worker, "hello");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        assert!(worker.running.is_none());
        match result {
            Err(Error::Worker(message)) => message,
            other => panic!("expected a worker error, got {other:?}"),
        }
    }

    #[test]
    fn output_that_is_not_utf8_fails_the_request_instead_of_hanging() {
        let message = fails_promptly(
            r#"
echo '{"ready": true}'
read -r line
printf '\377\n'
exec sleep 1000
"#,
        );
        assert!(message.contains("wrote"), "{message}");
    }

    #[test]
    fn a_worker_that_closes_its_output_without_exiting_is_killed() {
        let message = fails_promptly(
            r#"
echo '{"ready": true}'
read -r line
exec 1>&-
exec sleep 1000
"#,
        );
        assert!(
            message.contains("while waiting for a response"),
            "{message}"
        );
    }

    #[test]
    fn a_worker_that_exits_reports_its_status() {
        let message = fails_promptly(
            r#"
echo '{"ready": true}'
read -r line
exit 3
"#,
        );
        assert!(message.contains("exited"), "{message}");
    }

    #[test]
    fn startup_output_that_is_not_utf8_is_ignored() {
        let mut worker = Worker::new(shell(&format!("printf '\\377\\n'\n{ECHO}")));
        assert_eq!(call(&mut worker, "hello"), Ok(1));
    }

    #[test]
    fn startup_is_bounded_however_much_other_output_arrives() {
        let mut command = shell("while true; do echo noise; sleep 0.05; done");
        command.startup_timeout = Duration::from_millis(300);
        let mut worker = Worker::new(command);
        let started = Instant::now();
        assert!(matches!(worker.start(), Err(Error::Worker(_))));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn diagnostics_that_are_not_utf8_are_still_drained() {
        // More than a pipe buffer of diagnostics after an invalid byte: the worker blocks unless stderr is drained to the end.
        let script =
            format!("printf '\\377\\n' >&2\nhead -c 300000 /dev/zero | tr '\\0' 'x' >&2\n{ECHO}");
        let mut command = shell(&script);
        command.startup_timeout = Duration::from_secs(10);
        let mut worker = Worker::new(command);
        assert_eq!(call(&mut worker, "hello"), Ok(1));
    }
}
