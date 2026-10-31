// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The `claude` child process and its pipes.
//!
//! Each pipe is serviced by one std thread and output is delivered through a sink, so the session can wait with a plain
//! async receive. This is the only module that touches threads and pipes; replacing it with scheduler-owned pipes and
//! `ProcessWait` does not change the session.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

/// How many trailing lines of standard error are kept to explain an unexpected exit.
const STDERR_LINES: usize = 8;

/// Output from the child process, in order per pipe.
#[derive(Debug)]
pub(crate) enum Output {
    /// One line of standard output, without the line terminator.
    Line(String),
    /// Standard output reached end of file.
    Eof,
    /// The process exited and has been reaped.
    Exited(ExitStatus),
}

struct Status {
    child: Child,
    exited: Option<ExitStatus>,
}

struct Shared {
    status: Mutex<Status>,
    changed: Condvar,
}

impl Shared {
    fn record(&self, status: &mut Status) -> Option<ExitStatus> {
        if status.exited.is_none() {
            // The child has exited, so this reaps it without blocking. Reaping under the lock means `kill` can never
            // signal a recycled process id.
            if let Ok(Some(exit)) = status.child.try_wait() {
                status.exited = Some(exit);
                self.changed.notify_all();
            }
        }
        status.exited
    }
}

pub(crate) struct Process {
    stdin: Mutex<Option<mpsc::Sender<String>>>,
    shared: Arc<Shared>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    exited: async_channel::Receiver<()>,
}

impl Process {
    /// Spawn `command` with piped standard streams and deliver its output to `sink`.
    pub(crate) fn spawn(
        mut command: Command,
        sink: impl Fn(Output) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let pid = child.id();

        let sink = Arc::new(sink);
        let shared = Arc::new(Shared {
            status: Mutex::new(Status {
                child,
                exited: None,
            }),
            changed: Condvar::new(),
        });

        let (writer, lines) = mpsc::channel();
        thread::Builder::new()
            .name("claude-stdin".into())
            .spawn(move || write_lines(stdin, lines))?;

        let output = sink.clone();
        thread::Builder::new()
            .name("claude-stdout".into())
            .spawn(move || read_lines(stdout, &*output))?;

        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let errors = tail.clone();
        thread::Builder::new()
            .name("claude-stderr".into())
            .spawn(move || keep_tail(stderr, &errors))?;

        let (notify, exited) = async_channel::bounded::<()>(1);
        let reaped = shared.clone();
        thread::Builder::new()
            .name("claude-reaper".into())
            .spawn(move || {
                let status = reap(pid, &reaped);
                sink(Output::Exited(status));
                // Closing the channel wakes every task waiting for the exit.
                drop(notify);
            })?;

        Ok(Self {
            stdin: Mutex::new(Some(writer)),
            shared,
            stderr: tail,
            exited,
        })
    }

    /// Queue one line for standard input. Lines written after standard input is closed are dropped.
    pub(crate) fn write(&self, line: String) {
        if let Some(writer) = self.stdin.lock().unwrap().as_ref() {
            let _ = writer.send(line);
        }
    }

    /// Close standard input once queued lines are written, which asks the agent to finish.
    pub(crate) fn close_stdin(&self) {
        self.stdin.lock().unwrap().take();
    }

    /// Kill the process if it is still running after `grace`.
    pub(crate) fn kill_after(&self, grace: Duration) {
        let shared = self.shared.clone();
        let _ = thread::Builder::new()
            .name("claude-kill".into())
            .spawn(move || {
                let status = shared.status.lock().unwrap();
                let (mut status, _) = shared
                    .changed
                    .wait_timeout_while(status, grace, |status| status.exited.is_none())
                    .unwrap();
                if status.exited.is_none() {
                    let _ = status.child.kill();
                }
            });
    }

    /// Wait until the process has exited and been reaped.
    pub(crate) async fn wait_exit(&self) {
        let _ = self.exited.recv().await;
    }

    pub(crate) fn exit_status(&self) -> Option<ExitStatus> {
        self.shared.status.lock().unwrap().exited
    }

    /// The last lines the process wrote to standard error.
    pub(crate) fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lock().unwrap().iter().cloned().collect()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.exit_status().is_none() {
            self.close_stdin();
            self.kill_after(Duration::from_secs(2));
        }
    }
}

fn write_lines(mut stdin: ChildStdin, lines: mpsc::Receiver<String>) {
    for mut line in lines {
        line.push('\n');
        if stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .is_err()
        {
            break;
        }
    }
}

fn read_lines(stdout: impl Read, sink: &dyn Fn(Output)) {
    let mut reader = BufReader::new(stdout);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                let line = line.trim_end_matches(['\r', '\n']);
                if !line.is_empty() {
                    sink(Output::Line(line.to_owned()));
                }
            }
        }
    }
    sink(Output::Eof);
}

fn keep_tail(stderr: impl Read, tail: &Mutex<VecDeque<String>>) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        let mut tail = tail.lock().unwrap();
        if tail.len() == STDERR_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// Wait until the process has exited, then reap it under the status lock.
fn reap(pid: u32, shared: &Shared) -> ExitStatus {
    wait_for_exit(pid);
    loop {
        if let Some(exit) = shared.record(&mut shared.status.lock().unwrap()) {
            return exit;
        }
        // Without `waitid`, or if it failed, observe the exit by polling on this dedicated thread.
        thread::sleep(Duration::from_millis(50));
    }
}

/// Block until the process exits, leaving it unreaped so that its process id stays reserved.
#[cfg(unix)]
fn wait_for_exit(pid: u32) {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return;
        }
    }
}

#[cfg(not(unix))]
fn wait_for_exit(_pid: u32) {}
