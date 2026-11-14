// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use base_scheduler::{
    BufferPool, BufferPoolOptions, BufferPooling, Clock, IoBuf, SocketGetOption,
    SocketLocalAddress, SocketOperation, SocketStreamReader, SocketStreamWriter,
};
use base_scheduler_native::{NativeOptions, NativeScheduler, NativeSocket};
use futures_lite::future::block_on;

use super::*;
use crate::transcriber::NullTranscriber;

/// A scheduler whose accepts first fail with queued errors, and whose accepted sockets can be made never to finish closing, as a connection that ignores cancellation would.
struct Faulty {
    inner: NativeScheduler,
    accept_errors: Mutex<VecDeque<io::Error>>,
    accepts: AtomicUsize,
    stall_connection_closes: bool,
    /// Every timer fails, unless it is cancelled.
    failing_timers: bool,
    /// Closing the listener fails, once it has closed.
    failing_listener_close: bool,
    /// Called each time a timer's wait ends, before its result is returned.
    after_wait: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called each time accepting fails, before the failure is returned.
    after_accept_failure: Option<Box<dyn Fn() + Send + Sync>>,
    /// Setting `TCP_NODELAY` fails, leaving the socket as it was.
    failing_nodelay: bool,
    /// Whether each accepted socket had `TCP_NODELAY`, as the operating system reports it when the socket is closed.
    nodelay: Arc<Mutex<Vec<bool>>>,
}

impl Faulty {
    fn new() -> Self {
        Self {
            inner: NativeScheduler::with_options(NativeOptions {
                workers: 2,
                threads: 2,
                capacity: 256,
                ..Default::default()
            })
            .unwrap(),
            accept_errors: Mutex::new(VecDeque::new()),
            accepts: AtomicUsize::new(0),
            stall_connection_closes: false,
            failing_timers: false,
            failing_listener_close: false,
            after_wait: None,
            after_accept_failure: None,
            failing_nodelay: false,
            nodelay: Arc::default(),
        }
    }
}

/// A socket, and whether it was accepted rather than listening.
struct Tagged {
    socket: NativeSocket,
    accepted: bool,
}

impl SocketOperation for Faulty {
    type Socket = Tagged;
}

impl SocketAccept for Faulty {
    async fn socket_accept(
        &self,
        listener: &Tagged,
        cancellation: &Cancellation,
    ) -> OperationResult<(Tagged, SocketAddress)> {
        self.accepts.fetch_add(1, Ordering::Relaxed);
        let error = self.accept_errors.lock().unwrap().pop_front();
        if let Some(error) = error {
            return Err(error.into());
        }
        let accepted = self
            .inner
            .socket_accept(&listener.socket, cancellation)
            .await;
        if let (Err(OperationError::Io(_)), Some(after_accept_failure)) =
            (&accepted, &self.after_accept_failure)
        {
            after_accept_failure();
        }
        let (socket, peer) = accepted?;
        let socket = Tagged {
            socket,
            accepted: true,
        };
        Ok((socket, peer))
    }
}

impl SocketClose for Faulty {
    async fn socket_close(&self, socket: Tagged) -> io::Result<()> {
        if socket.accepted {
            let nodelay = self
                .inner
                .socket_get_option(&socket.socket, TcpNoDelay, &Cancellation::never())
                .await
                .unwrap();
            self.nodelay.lock().unwrap().push(nodelay);
        }
        if socket.accepted && self.stall_connection_closes {
            let _held = socket;
            return pending().await;
        }
        let listener = !socket.accepted;
        let closed = self.inner.socket_close(socket.socket).await;
        if listener && self.failing_listener_close {
            return Err(io::Error::other("the listener did not close cleanly"));
        }
        closed
    }

    async fn socket_close_submit(&self, socket: Tagged) -> io::Result<()> {
        self.inner.socket_close_submit(socket.socket).await
    }
}

impl SocketSetOption<TcpNoDelay> for Faulty {
    async fn socket_set_option(
        &self,
        socket: &Tagged,
        option: TcpNoDelay,
        value: bool,
        cancellation: &Cancellation,
    ) -> OperationResult<()> {
        if self.failing_nodelay {
            return Err(io::Error::other("the option is refused").into());
        }
        self.inner
            .socket_set_option(&socket.socket, option, value, cancellation)
            .await
    }
}

impl SocketLocalAddress for Faulty {
    fn socket_local_address(&self, socket: &Tagged) -> io::Result<SocketAddress> {
        self.inner.socket_local_address(&socket.socket)
    }
}

impl BufferPooling for Faulty {
    type Pool = <NativeScheduler as BufferPooling>::Pool;

    fn buffer_pool(&self, options: BufferPoolOptions) -> io::Result<Self::Pool> {
        self.inner.buffer_pool(options)
    }
}

impl SocketStreamReading for Faulty {
    fn socket_stream_reader<'scheduler>(
        &'scheduler self,
        socket: &Tagged,
    ) -> io::Result<
        impl SocketStreamReader<Buffer = <Self::Pool as BufferPool>::Buffer>
        + 'scheduler
        + use<'scheduler>,
    > {
        self.inner.socket_stream_reader(&socket.socket)
    }

    fn socket_stream_reader_with_pool<'scheduler>(
        &'scheduler self,
        socket: &Tagged,
        pool: Self::Pool,
    ) -> io::Result<
        impl SocketStreamReader<Buffer = <Self::Pool as BufferPool>::Buffer>
        + 'scheduler
        + use<'scheduler>,
    > {
        self.inner
            .socket_stream_reader_with_pool(&socket.socket, pool)
    }
}

impl SocketStreamWriting for Faulty {
    fn socket_stream_writer<'scheduler>(
        &'scheduler self,
        socket: &Tagged,
    ) -> io::Result<
        impl SocketStreamWriter<Buffer = <Self::Pool as BufferPool>::Buffer>
        + 'scheduler
        + use<'scheduler>,
    > {
        self.inner.socket_stream_writer(&socket.socket)
    }

    fn socket_stream_writer_with_buffer<'scheduler, B: IoBuf>(
        &'scheduler self,
        socket: &Tagged,
    ) -> io::Result<impl SocketStreamWriter<Buffer = B> + 'scheduler + use<'scheduler, B>> {
        self.inner
            .socket_stream_writer_with_buffer::<B>(&socket.socket)
    }
}

impl Clock for Faulty {
    fn now(&self) -> Instant {
        self.inner.now()
    }

    async fn sleep_until(
        &self,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> OperationResult<()> {
        self.inner.sleep_until(deadline, cancellation).await
    }
}

impl Timers for Faulty {
    type Timer = <NativeScheduler as Timers>::Timer;

    fn timer(&self) -> Self::Timer {
        self.inner.timer()
    }

    async fn timer_wait_until(
        &self,
        timer: &mut Self::Timer,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> OperationResult<()> {
        if self.failing_timers && !cancellation.is_cancelled() {
            return Err(io::Error::other("the timer is broken").into());
        }
        let result = self
            .inner
            .timer_wait_until(timer, deadline, cancellation)
            .await;
        if let Some(after_wait) = &self.after_wait {
            after_wait();
        }
        result
    }
}

#[test]
fn counts_that_change_back_before_they_are_due_are_not_published() {
    let published = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let published = published.clone();
        server()
            .with_counts(move |counts| published.lock().unwrap().push(counts))
            .with_counts_interval(Duration::ZERO)
    };
    let listed = server.counts();
    // One more space than is listed.
    let more = Counts {
        spaces: listed.spaces + 1,
        ..listed
    };
    // Serving stops as the wait for each change to be due ends; `listen` is called first.
    let stopping_after = |listen: Box<dyn Fn() + Send + Sync>| {
        let live = Cancellation::new();
        let scheduler = Faulty {
            after_wait: Some(Box::new({
                let live = live.clone();
                move || {
                    listen();
                    live.cancel();
                }
            })),
            ..Faulty::new()
        };
        (scheduler, live)
    };
    // Last published with a space more than is listed, the counts as listed are published once due.
    let (scheduler, live) = stopping_after(Box::new(|| {}));
    block_on(server.publish(&scheduler, more, &live));
    assert_eq!(*published.lock().unwrap(), [listed]);
    // But if a space is listed by then, as there was when they were last published, nothing is.
    let hub = server.hub.clone();
    let (scheduler, live) = stopping_after(Box::new(move || {
        let (commands, _) = async_channel::unbounded();
        let request = crate::hub::CreateRequest {
            title: "Space".to_owned(),
            backend: "fake".to_owned(),
            directory: "/tmp".to_owned(),
            session: None,
            creator: "nobody".to_owned(),
            creator_space: None,
            environment: "sky".to_owned(),
            instructions: String::new(),
        };
        hub.register_space(&request, "session".to_owned(), Default::default(), commands)
            .unwrap();
    }));
    block_on(server.publish(&scheduler, more, &live));
    assert_eq!(server.counts(), more);
    assert_eq!(*published.lock().unwrap(), [listed]);
}

/// A server running on its own thread with a [`Faulty`] scheduler.
struct Running {
    address: SocketAddr,
    cancellation: Cancellation,
    /// Receives what `run` returned, and how many accepts were attempted.
    finished: Receiver<(Result<Statistics, Error>, usize)>,
}

fn start(
    accept_errors: Vec<io::Error>,
    stall_connection_closes: bool,
    drain_timeout: Duration,
) -> Running {
    start_with(
        Faulty {
            accept_errors: Mutex::new(accept_errors.into()),
            stall_connection_closes,
            ..Faulty::new()
        },
        move |server| server.drain_timeout = drain_timeout,
    )
}

/// Serve with `scheduler`, configured by `configure`.
fn start_with(scheduler: Faulty, configure: impl FnOnce(&mut Server) + Send + 'static) -> Running {
    let cancellation = Cancellation::new();
    let (address_sender, address) = channel();
    let (sender, finished) = channel();
    let running = cancellation.clone();
    std::thread::spawn(move || {
        let mut server = Server::new(Backends::new(), Arc::new(NullTranscriber::new()))
            .with_workspace_roots(WorkspaceRoots::none());
        configure(&mut server);
        let result = block_on(async {
            let address = "127.0.0.1:0".parse().unwrap();
            let listener = bind(&scheduler.inner, address, &running).await.unwrap();
            let address = scheduler.inner.socket_local_address(&listener).unwrap();
            address_sender.send(address).unwrap();
            let listener = Tagged {
                socket: listener,
                accepted: false,
            };
            server.run(&scheduler, listener, &running).await
        });
        let accepts = scheduler.accepts.load(Ordering::Relaxed);
        block_on(scheduler.inner.shutdown());
        let _ = sender.send((result, accepts));
    });
    Running {
        address: address.recv().unwrap(),
        cancellation,
        finished,
    }
}

#[test]
fn a_refusal_is_said_without_the_registration_s_secrets() {
    let server = tomori_backend::ToolServer::new("tomori", "http://127.0.0.1:1/mcp")
        .with_header("Authorization", "Bearer tomori_0123456789abcdef");
    assert_eq!(
        redacted(
            "refused Bearer tomori_0123456789abcdef, then tomori_0123456789abcdef".to_owned(),
            Some(&server)
        ),
        "refused (secret), then (secret)"
    );
    // A short word, such as the scheme, is left, and so is everything without a registration.
    assert_eq!(
        redacted("Bearer tokens only".to_owned(), Some(&server)),
        "Bearer tokens only"
    );
    assert_eq!(redacted("lost".to_owned(), None), "lost");
}

#[test]
fn the_tools_url_names_a_loopback_address_for_every_address() {
    for (listening, url) in [
        ("0.0.0.0:8766", "http://127.0.0.1:8766/mcp"),
        ("[::]:8766", "http://[::1]:8766/mcp"),
        ("127.0.0.1:1", "http://127.0.0.1:1/mcp"),
        ("[::1]:2", "http://[::1]:2/mcp"),
    ] {
        assert_eq!(tools_url(listening.parse().unwrap()), url);
    }
}

#[test]
fn a_tools_endpoint_that_cannot_accept_leaves_the_server_serving() {
    // So that the warning and the error are written, whichever test runs first.
    log::set_max_level(log::LevelFilter::Trace);
    let (failed, accept_failed) = channel();
    let scheduler = Faulty {
        after_accept_failure: Some(Box::new(move || {
            let _ = failed.send(());
        })),
        ..Faulty::new()
    };
    let cancellation = Cancellation::new();
    let (sender, address) = channel();
    let serving = std::thread::spawn({
        let cancellation = cancellation.clone();
        move || {
            let server = Server::new(Backends::new(), Arc::new(NullTranscriber::new()))
                .with_workspace_roots(WorkspaceRoots::none());
            let result = block_on(async {
                let address: SocketAddr = "127.0.0.1:0".parse().unwrap();
                let listener = bind(&scheduler.inner, address, &cancellation)
                    .await
                    .unwrap();
                sender
                    .send(scheduler.inner.socket_local_address(&listener).unwrap())
                    .unwrap();
                // Bound but never listening, so that accepting on it fails at once, and on every address, which is warned of.
                let every: SocketAddr = "0.0.0.0:0".parse().unwrap();
                let tools = scheduler
                    .inner
                    .socket_open(every.into(), SocketType::Stream, None, &cancellation)
                    .await
                    .unwrap();
                scheduler
                    .inner
                    .socket_bind(&tools, every, &cancellation)
                    .await
                    .unwrap();
                let [listener, tools] = [listener, tools].map(|socket| Tagged {
                    socket,
                    accepted: false,
                });
                server
                    .run_with_tools(&scheduler, listener, tools, &cancellation)
                    .await
            });
            block_on(scheduler.inner.shutdown());
            result
        }
    });
    let address = address.recv().unwrap();
    assert!(health(address).contains("\"status\":\"ok\""));
    // Serving stops only once accepting on the tools' listener has failed, which ends their serving with that error rather than with the cancellation.
    accept_failed
        .recv_timeout(Duration::from_secs(10))
        .expect("accepting on the tools' listener fails");
    cancellation.cancel();
    assert!(serving.join().unwrap().is_ok());
}

/// Request `/health` and return the response, reading only as far as the end of its body, so that a socket the server never closes does not matter.
fn health(address: SocketAddr) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    let mut buffer = [0; 1024];
    while !response.ends_with(b"}\n") {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "the response ended early");
        response.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(response).unwrap()
}

/// Serve `/health` once with `scheduler`, then stop, and return whether the accepted socket had `TCP_NODELAY` as it closed.
fn nodelay_serving_health(scheduler: Faulty) -> Vec<bool> {
    let nodelay = scheduler.nodelay.clone();
    let running = start_with(scheduler, |_| {});
    assert!(health(running.address).contains("\"status\":\"ok\""));
    running.cancellation.cancel();
    let (result, _) = running.finished.recv().unwrap();
    assert_eq!(result.unwrap().connections, 1);
    nodelay.lock().unwrap().clone()
}

#[test]
fn accepted_sockets_disable_nagle_s_algorithm() {
    assert_eq!(nodelay_serving_health(Faulty::new()), [true]);
}

#[test]
fn a_socket_that_refuses_tcp_nodelay_is_still_served() {
    // So that the debug message is written, whichever test runs first.
    log::set_max_level(log::LevelFilter::Trace);
    let scheduler = Faulty {
        failing_nodelay: true,
        ..Faulty::new()
    };
    assert_eq!(nodelay_serving_health(scheduler), [false]);
}

#[test]
fn the_drain_timeout_can_be_changed() {
    let server = server();
    assert_eq!(server.drain_timeout, DEFAULT_DRAIN_TIMEOUT);
    let server = server.with_drain_timeout(Duration::from_millis(250));
    assert_eq!(server.drain_timeout, Duration::from_millis(250));
}

fn server() -> Server {
    Server::new(Backends::new(), Arc::new(NullTranscriber::new()))
        .with_workspace_roots(WorkspaceRoots::none())
}

/// The identifier, label, version and whether it has a server side of each environment `server` offers, in order.
fn offered(server: &Server) -> Vec<(String, String, u32, bool)> {
    server
        .environments()
        .iter()
        .map(|environment| {
            (
                environment.id().to_owned(),
                environment.label().to_owned(),
                environment.version(),
                environment.server_side().is_some(),
            )
        })
        .collect()
}

#[test]
fn an_environment_offered_again_replaces_the_one_with_its_identifier_in_its_place() {
    struct Idle;
    impl tomori_environment::ServerSide for Idle {
        fn run(&self, _: &tomori_environment::Space) -> tomori_environment::Result<()> {
            Ok(())
        }
    }
    let ids = offered;
    // None are offered until some are, and one offered alone is the default.
    assert!(server().environments().is_empty());
    let server = server().with_environment(Environment::new("observe", "Observe"));
    assert_eq!(
        ids(&server),
        [("observe".to_owned(), "Observe".to_owned(), 1, false)]
    );
    // Those offered before are kept, in order.
    let server = server.with_environments([
        Environment::new("sky", "Sky"),
        Environment::new("observe", "Observe").with_instructions("Mind the topology."),
        Environment::new(tomori_protocol::GRID, "Grid"),
    ]);
    // The identifier is the key: the first, the default, is replaced in its place, whatever its label and version.
    let server = server.with_environment(Environment::new("sky", "Night").with_version(2));
    // One with a server side replaces one without, and is offered with the new one's instructions, here none.
    let server =
        server.with_environment(Environment::new("observe", "Live").with_server_side(Idle));
    // One not offered yet goes after the others.
    let server = server.with_environment(Environment::new("farmyard", "Farmyard"));
    assert_eq!(
        ids(&server),
        [
            ("sky".to_owned(), "Night".to_owned(), 2, false),
            ("observe".to_owned(), "Live".to_owned(), 1, true),
            (
                tomori_protocol::GRID.to_owned(),
                "Grid".to_owned(),
                1,
                false
            ),
            ("farmyard".to_owned(), "Farmyard".to_owned(), 1, false),
        ]
    );
    assert_eq!(server.environments()[1].instructions(), "");
    let space = tomori_environment::Space::new("space-1", "Fleet", "/");
    assert_eq!(
        server.environments()[1]
            .server_side()
            .expect("it has a side")
            .run(&space),
        Ok(())
    );
    assert_eq!(
        Environments::new(server.environments()).choose(None),
        Ok("sky".to_owned())
    );
    // One without a server side replaces one with, and has none.
    let server = server.with_environment(Environment::new("observe", "Observe"));
    assert_eq!(
        ids(&server)[1],
        ("observe".to_owned(), "Observe".to_owned(), 1, false)
    );
    assert_eq!(server.environments().len(), 4);
}

// Small builders called only from other crates are inlined there, where their coverage is not counted as theirs, so each is also called in this crate's tests.
#[test]
fn the_participant_retention_given_is_kept() {
    let retention = Duration::from_secs(7);
    assert_eq!(
        server().with_participant_retention(retention).retention,
        retention
    );
}

#[test]
fn the_test_scheduler_passes_through_what_it_does_not_break() {
    let scheduler = Faulty::new();
    let never = Cancellation::never();
    let options = BufferPoolOptions {
        buffers: 1,
        buffer_size: 64,
        alignment: 8,
    };
    let pool = scheduler.buffer_pool(options).unwrap();
    block_on(async {
        scheduler
            .sleep_until(scheduler.now(), &never)
            .await
            .unwrap();
        let address = "127.0.0.1:0".parse().unwrap();
        let socket = Tagged {
            socket: bind(&scheduler.inner, address, &never).await.unwrap(),
            accepted: false,
        };
        assert!(
            scheduler
                .socket_stream_reader_with_pool(&socket, pool)
                .is_ok()
        );
        assert!(
            scheduler
                .socket_stream_writer_with_buffer::<Vec<u8>>(&socket)
                .is_ok()
        );
        scheduler.socket_close_submit(socket).await.unwrap();
    });
    block_on(scheduler.inner.shutdown());
}

/// Cards read from a script, last first, panicking once it is empty. A gated read waits until its gate is opened
/// or dropped, so that a test can end serving while it is under way without depending on how long it takes.
struct Scripted {
    gate: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    results: Mutex<Vec<Result<crate::cards::Fetched, crate::cards::Failure>>>,
}

impl crate::cards::CardSource for Scripted {
    fn source(&self) -> String {
        "o/r".to_owned()
    }

    fn service(&self) -> String {
        "GitHub".to_owned()
    }

    fn fetch(&self) -> Result<crate::cards::Fetched, crate::cards::Failure> {
        if let Some(gate) = &self.gate {
            let _ = gate.lock().unwrap().recv();
        }
        self.results
            .lock()
            .unwrap()
            .pop()
            .expect("the script has another read")
    }
}

fn scripted(
    results: Vec<Result<crate::cards::Fetched, crate::cards::Failure>>,
) -> Arc<dyn crate::cards::CardSource> {
    Arc::new(Scripted {
        gate: None,
        results: Mutex::new(results),
    })
}

/// A source whose one read answers only once the returned gate is opened or dropped.
fn gated() -> (
    Arc<dyn crate::cards::CardSource>,
    std::sync::mpsc::Sender<()>,
) {
    let (open, gate) = std::sync::mpsc::channel();
    let source = Arc::new(Scripted {
        gate: Some(Mutex::new(gate)),
        results: Mutex::new(vec![Ok(crate::cards::Fetched::default())]),
    });
    (source, open)
}

#[test]
fn participants_are_remembered_as_long_as_asked() {
    // Tested here as well as by the integration tests that use it, so that its coverage does not depend on
    // whether they inline it.
    let retention = Duration::from_secs(60);
    assert_eq!(
        server().with_participant_retention(retention).retention,
        retention
    );
}

#[test]
fn cards_are_read_on_a_thread_of_their_own_and_a_lost_read_is_a_failure() {
    use crate::cards::{Failure, Fetched};
    let live = Cancellation::new();
    let read = |source, cancellation: &Cancellation| block_on(read_cards(source, cancellation));
    assert_eq!(
        read(scripted(vec![Ok(Fetched::default())]), &live),
        Some(Ok(Fetched::default()))
    );
    // A read that panics answers nothing.
    assert_eq!(
        read(scripted(Vec::new()), &live),
        Some(Err(Failure::new("reading ended without an answer")))
    );
    crate::system::refuse("tomori-cards");
    let refused = read(scripted(Vec::new()), &live).unwrap().unwrap_err();
    assert!(
        refused.message.starts_with("could not start reading: "),
        "{refused:?}"
    );
    // Serving ends while it reads: its answer is ignored.
    let cancelled = Cancellation::new();
    cancelled.cancel();
    let (slow, open) = gated();
    assert_eq!(read(slow, &cancelled), None);
    drop(open);
}

#[test]
fn cards_offer_their_environment_exactly_as_with_environment_does() {
    let offered_both_ways = |before: &[Environment], environment: Environment| {
        let with_cards = server()
            .with_environments(before.to_vec())
            .with_cards(environment.clone(), scripted(Vec::new()));
        let with_environment = server()
            .with_environments(before.to_vec())
            .with_environment(environment);
        assert_eq!(offered(&with_cards), offered(&with_environment));
        with_cards
    };
    let github = || Environment::new("github", "GitHub").with_version(2);
    // One not offered yet goes after the others.
    let added = offered_both_ways(&[Environment::new("sky", "Sky")], github());
    assert_eq!(
        offered(&added),
        [
            ("sky".to_owned(), "Sky".to_owned(), 1, false),
            ("github".to_owned(), "GitHub".to_owned(), 2, false),
        ]
    );
    // One offered already, here the default, is replaced in its place, and stays the default.
    let replaced = offered_both_ways(
        &[
            Environment::new("github", "Issues"),
            Environment::new("sky", "Sky"),
        ],
        github(),
    );
    assert_eq!(
        offered(&replaced),
        [
            ("github".to_owned(), "GitHub".to_owned(), 2, false),
            ("sky".to_owned(), "Sky".to_owned(), 1, false),
        ]
    );
    assert_eq!(
        Environments::new(replaced.environments()).choose(None),
        Ok("github".to_owned())
    );
    // Either way, the cards are shown in its spaces.
    watching(added);
    watching(replaced);
}

#[test]
fn cards_offered_while_serving_run_their_environment_s_server_side() {
    /// A server side that says which space it runs for, and ends.
    struct Announcing(std::sync::Mutex<std::sync::mpsc::Sender<String>>);
    impl tomori_environment::ServerSide for Announcing {
        fn run(&self, space: &tomori_environment::Space) -> tomori_environment::Result<()> {
            self.0.lock().unwrap().send(space.id().to_owned()).unwrap();
            Ok(())
        }
    }
    let server = server().with_environments([
        Environment::new("sky", "Sky"),
        Environment::new("github", "GitHub"),
    ]);
    server.hub.serving();
    let id = github_space(&server);
    let (sender, ran) = std::sync::mpsc::channel();
    let server = server.with_cards(
        Environment::new("github", "GitHub")
            .with_server_side(Announcing(std::sync::Mutex::new(sender))),
        scripted(Vec::new()),
    );
    assert_eq!(ran.recv_timeout(Duration::from_secs(30)), Ok(id));
    server.hub.shutdown();
}

/// A server showing `source`'s cards, with an owner watching them in a space that shows them.
fn server_with_watcher(source: Arc<dyn crate::cards::CardSource>) -> Server {
    watching(server().with_cards(Environment::new("github", "GitHub"), source))
}

/// Register a space in the GitHub environment on `server`, returning its identifier.
fn github_space(server: &Server) -> String {
    let (commands, _receiver) = async_channel::unbounded();
    let request = crate::hub::CreateRequest {
        title: "Cards".to_owned(),
        backend: "fake".to_owned(),
        directory: "/tmp".to_owned(),
        session: None,
        creator: "nobody".to_owned(),
        creator_space: None,
        environment: "github".to_owned(),
        instructions: String::new(),
    };
    server
        .hub
        .register_space(&request, "session".to_owned(), Default::default(), commands)
        .unwrap()
}

/// `server`, which shows cards in the GitHub environment, with an owner watching them in a space of it, whose cards are due at once.
fn watching(server: Server) -> Server {
    let id = github_space(&server);
    let (connection, ..) = server
        .hub
        .connect(Cancellation::new(), crate::Access::open())
        .unwrap();
    for message in [
        tomori_protocol::ClientMessage::Hello {
            protocol: tomori_protocol::PROTOCOL_VERSION,
            participant: "ada".to_owned(),
            name: "Ada".to_owned(),
            names: false,
            environments: None,
            environment_state: false,
            projection: false,
            pairing_window: false,
            speech_held: false,
            images: false,
            diagrams: false,
            world_edges: false,
            projection_patches: false,
            live_panels: false,
            spawns: false,
            limits: None,
        },
        tomori_protocol::ClientMessage::Join { space: id },
        tomori_protocol::ClientMessage::WatchCards,
    ] {
        server
            .hub
            .receive(connection, tomori_protocol::Frame::client(&message));
    }
    assert_eq!(
        server.hub.cards_next(Instant::now()),
        crate::cards::Next::Now
    );
    server
}

#[test]
fn cards_wait_until_due_or_watched_and_a_failing_timer_ends_their_reading() {
    crate::test_log::record();
    let scheduler = Faulty {
        failing_timers: true,
        ..Faulty::new()
    };
    let live = Cancellation::new();
    // A server without cards reads none.
    block_on(server().refresh_cards(&scheduler, &live));
    let server = server().with_cards(Environment::new("github", "GitHub"), scripted(Vec::new()));
    assert!(
        server
            .environments()
            .iter()
            .any(|environment| environment.id() == "github" && environment.label() == "GitHub")
    );
    // Nobody watches, so it waits, until serving ends.
    let later = Cancellation::new();
    block_on(zip(server.refresh_cards(&scheduler, &later), async {
        later.cancel()
    }));
    // Read just now, it waits until they are due, ending as the timer fails.
    server
        .hub
        .cards_fetched(Ok(crate::cards::Fetched::default()), Instant::now());
    block_on(server.refresh_cards(&scheduler, &live));
    // Serving ends while a read is under way: its answer is not waited for.
    let (slow, open) = gated();
    let watched = server_with_watcher(slow);
    let later = Cancellation::new();
    block_on(zip(watched.refresh_cards(&Faulty::new(), &later), async {
        later.cancel()
    }));
    drop(open);
    // A timer cancelled as serving ends is no failure.
    let later = Cancellation::new();
    block_on(zip(server.refresh_cards(&Faulty::new(), &later), async {
        later.cancel()
    }));
    assert!(!crate::test_log::logged("The cards timer failed").is_empty());
}

#[test]
fn timers_that_fail_end_the_sweep_the_pacing_the_counts_and_the_drain() {
    log::set_max_level(log::LevelFilter::Trace);
    let scheduler = Faulty {
        failing_timers: true,
        ..Faulty::new()
    };
    let live = Cancellation::new();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    // Told nothing, as its timer fails first.
    let server = server().with_counts(drop::<Counts>);
    block_on(server.sweep(&scheduler, &live));
    block_on(server.sweep(&scheduler, &cancelled));
    block_on(server.look_at_timers(&scheduler, &live));
    block_on(server.look_at_timers(&scheduler, &cancelled));
    // A deferred snapshot is waited for.
    let (commands, _receiver) = async_channel::unbounded();
    let request = crate::hub::CreateRequest {
        title: "Space".to_owned(),
        backend: "fake".to_owned(),
        directory: "/tmp".to_owned(),
        session: None,
        creator: "nobody".to_owned(),
        creator_space: None,
        environment: "sky".to_owned(),
        instructions: String::new(),
    };
    let id = server
        .hub
        .register_space(&request, "session".to_owned(), Default::default(), commands)
        .unwrap();
    // The second event within the interval defers its snapshot.
    server.hub.drive_event(&id, tomori_backend::Event::Busy);
    server.hub.drive_event(&id, tomori_backend::Event::Idle);
    assert!(server.hub.snapshot_deadline().is_some());
    block_on(server.pace(&scheduler, &live));
    block_on(server.pace(&scheduler, &cancelled));
    // Counts that changed are waited for.
    let changed = Counts {
        spaces: 99,
        ..server.counts()
    };
    block_on(server.publish(&scheduler, changed, &live));
    block_on(server.publish(&scheduler, changed, &cancelled));
    // Connections still open are abandoned, and said with what they were doing.
    let mut connections = FuturesUnordered::new();
    connections.push(pending::<Finished>());
    let mut tracker = Tracker::default();
    tracker.open.insert(
        1,
        Open {
            peer: "127.0.0.1:1".parse().unwrap(),
            watch: server.admission.watch(Cancellation::new()),
        },
    );
    block_on(server.drain(&scheduler, &mut connections, &mut tracker));
    assert_eq!(connections.len(), 1);
    block_on(scheduler.inner.shutdown());
}

#[test]
fn a_listener_that_does_not_close_cleanly_is_only_said() {
    crate::test_log::record();
    let running = start_with(
        Faulty {
            failing_listener_close: true,
            ..Faulty::new()
        },
        |_| {},
    );
    assert!(health(running.address).starts_with("HTTP/1.1 200"));
    running.cancellation.cancel();
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(result.unwrap().connections, 1);
    assert!(!crate::test_log::logged("Closing the listener failed").is_empty());
}

#[test]
fn a_pause_whose_timer_fails_ends_the_server() {
    let running = start_with(
        Faulty {
            accept_errors: Mutex::new([io::Error::from(io::ErrorKind::OutOfMemory)].into()),
            failing_timers: true,
            ..Faulty::new()
        },
        |_| {},
    );
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    let Err(Error::Io(_)) = result else {
        unreachable!("serving fails with the timer's error, not {result:?}");
    };
}

#[test]
fn a_server_cancelled_while_pausing_or_full_stops_cleanly() {
    // Cancelled while accepting is paused.
    let running = start_with(
        Faulty {
            accept_errors: Mutex::new([io::Error::from(io::ErrorKind::OutOfMemory)].into()),
            ..Faulty::new()
        },
        |server| server.drain_timeout = Duration::ZERO,
    );
    running.cancellation.cancel();
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(result.unwrap().connections, 0);
    // A response kept alive shows that the connection was accepted and stays open.
    let healthy = |connection: &mut TcpStream| {
        write!(
            connection,
            "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut response = Vec::new();
        let mut buffer = [0; 1024];
        while !response.ends_with(b"}\n") {
            let count = connection.read(&mut buffer).unwrap();
            response.extend_from_slice(&buffer[..count]);
        }
    };
    // Cancelled while as many connections as it serves are open. The connection is served only while the server waits at the limit, and it can never finish, since its socket never finishes closing, so only the cancellation can end that wait. A connection that could finish might do so first: cancelling marks every signal before it wakes any waiter, so the connection can see its own cancellation and end before the wait is woken.
    let running = start_with(
        Faulty {
            stall_connection_closes: true,
            ..Faulty::new()
        },
        |server| {
            server.max_connections = 1;
            server.drain_timeout = Duration::ZERO;
        },
    );
    let mut idle = TcpStream::connect(running.address).unwrap();
    healthy(&mut idle);
    running.cancellation.cancel();
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(result.unwrap().connections, 1);
    drop(idle);
    // At the limit, another is served only once the open connection closes.
    let running = start_with(Faulty::new(), |server| {
        server.max_connections = 1;
        server.drain_timeout = Duration::ZERO;
    });
    let mut idle = TcpStream::connect(running.address).unwrap();
    healthy(&mut idle);
    let mut next = TcpStream::connect(running.address).unwrap();
    drop(idle);
    healthy(&mut next);
    // Whether the cancellation ends the wait at the limit or the next accept, which depends on timing, both are covered elsewhere.
    running.cancellation.cancel();
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(result.unwrap().connections, 2);
    drop(next);
}

/// A scheduler that refuses the `step`th of opening, configuring, binding and listening, and does nothing else.
struct Refusing {
    step: usize,
}

impl Refusing {
    fn at(&self, step: usize) -> OperationResult<()> {
        if self.step == step {
            Err(io::Error::other("refused").into())
        } else {
            Ok(())
        }
    }
}

impl SocketOperation for Refusing {
    type Socket = ();
}

impl SocketOpen for Refusing {
    async fn socket_open(
        &self,
        _: base_scheduler::SocketDomain,
        _: SocketType,
        _: Option<base_scheduler::SocketProtocol>,
        _: &Cancellation,
    ) -> OperationResult<()> {
        self.at(0)
    }
}

impl SocketSetOption<SocketReuseAddress> for Refusing {
    async fn socket_set_option(
        &self,
        _: &(),
        _: SocketReuseAddress,
        _: bool,
        _: &Cancellation,
    ) -> OperationResult<()> {
        self.at(1)
    }
}

impl SocketBind for Refusing {
    async fn socket_bind(&self, _: &(), _: SocketAddress, _: &Cancellation) -> OperationResult<()> {
        self.at(2)
    }
}

impl SocketListen for Refusing {
    async fn socket_listen(&self, _: &(), _: i32, _: &Cancellation) -> OperationResult<()> {
        self.at(3)
    }
}

#[test]
fn binding_fails_at_whichever_step_is_refused() {
    let address = "127.0.0.1:0".parse().unwrap();
    let never = Cancellation::never();
    for step in 0..4 {
        assert!(block_on(bind(&Refusing { step }, address, &never)).is_err());
    }
    assert!(block_on(bind(&Refusing { step: 4 }, address, &never)).is_ok());
}

/// What a space's driver works with: a hub with `backend`, room for `slots` spaces in `/tmp`, and a scheduler.
struct Supervision {
    backend: crate::fake::FakeBackend,
    hub: Arc<Hub>,
    roots: WorkspaceRoots,
    resolving: Arc<Resolving>,
    slots: Arc<SpaceSlots>,
    scheduler: Faulty,
}

impl Supervision {
    fn new(slots: usize) -> Self {
        let backend = crate::fake::FakeBackend::new();
        let server = Server::new(
            Backends::new().with(backend.clone()),
            Arc::new(NullTranscriber::new()),
        );
        Self {
            backend,
            hub: server.hub.clone(),
            roots: WorkspaceRoots::new(["/tmp"]).unwrap(),
            resolving: Arc::default(),
            slots: SpaceSlots::new(slots),
            scheduler: Faulty::new(),
        }
    }

    fn places(&self) -> Places<'_> {
        Places {
            roots: &self.roots,
            resolving: &self.resolving,
            slots: &self.slots,
        }
    }

    fn request(&self, backend: &str) -> crate::hub::CreateRequest {
        crate::hub::CreateRequest {
            title: "Space".to_owned(),
            backend: backend.to_owned(),
            directory: "/tmp".to_owned(),
            session: None,
            creator: "nobody".to_owned(),
            creator_space: None,
            environment: "sky".to_owned(),
            instructions: String::new(),
        }
    }

    fn create(&self, request: crate::hub::CreateRequest, cancellation: &Cancellation) {
        block_on(run_space(
            &self.scheduler,
            self.hub.clone(),
            request,
            Voices::default(),
            self.places(),
            cancellation,
        ));
    }

    /// A space kept from before a restart, bound to `session` of `backend` in `directory`, listed again.
    fn kept(
        &self,
        name: &str,
        backend: &str,
        directory: &str,
        session: &str,
    ) -> crate::hub::Restore {
        let state =
            std::env::temp_dir().join(format!("tomori-server-kept-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let store = crate::kept::SpaceStore::open(&state).unwrap();
        store
            .keep(|| {
                Some(vec![crate::kept::KeptSpace {
                    id: "space-kept".to_owned(),
                    title: "Kept".to_owned(),
                    renamed_by: None,
                    backend: backend.to_owned(),
                    directory: directory.to_owned(),
                    session: session.to_owned(),
                    environment: "sky".to_owned(),
                    attribution: true,
                    brief: false,
                    movement: None,
                    instructions: String::new(),
                    closed: None,
                    closed_order: None,
                    placements: Default::default(),
                    stage: Default::default(),
                    places: Default::default(),
                    active: None,
                    timers: Default::default(),
                }])
            })
            .unwrap();
        let restore = self.hub.restore(Arc::new(store)).pop().unwrap();
        let _ = std::fs::remove_dir_all(&state);
        restore
    }

    fn restore(&self, restore: crate::hub::Restore, cancellation: &Cancellation) {
        block_on(run_restored(
            &self.scheduler,
            self.hub.clone(),
            restore,
            Voices::default(),
            self.places(),
            cancellation,
        ));
    }

    /// The kept space's state.
    fn kept_state(&self) -> tomori_protocol::SpaceState {
        self.hub.space_state("space-kept")
    }

    /// A session of the fake backend that something else is bound to.
    fn held(&self) -> String {
        let session = block_on(self.backend.create(SessionSpec {
            directory: "/tmp".to_owned(),
            title: "Held".to_owned(),
        }))
        .unwrap();
        tomori_backend::Session::id(&session)
    }

    /// A session of the fake backend that nothing is bound to, as one left by a server that stopped.
    fn left(&self) -> String {
        let session = block_on(self.backend.create(SessionSpec {
            directory: "/tmp".to_owned(),
            title: "Left".to_owned(),
        }))
        .unwrap();
        block_on(tomori_backend::Session::close(&session)).unwrap();
        tomori_backend::Session::id(&session)
    }
}

#[test]
fn a_space_that_cannot_start_releases_its_place_and_closes_what_it_started() {
    crate::test_log::record();
    let supervision = Supervision::new(1);
    let live = Cancellation::new();
    // A backend that is not enabled starts nothing.
    supervision.create(supervision.request("gone"), &live);
    // A session whose state cannot be read is closed, even if closing fails.
    supervision.backend.fail_snapshots(true);
    supervision.backend.fail_closing();
    supervision.create(supervision.request("fake"), &live);
    assert!(supervision.backend.started()[0].log().closed);
    supervision.backend.fail_snapshots(false);
    // A session started as the server stops is closed.
    supervision.hub.stopping();
    supervision.create(supervision.request("fake"), &live);
    assert!(supervision.backend.started()[1].log().closed);
    assert!(supervision.hub.status().spaces.is_empty());
    assert!(supervision.slots.acquire().is_some());
    // Closing such a session may succeed too.
    let supervision = Supervision::new(1);
    supervision.hub.stopping();
    supervision.create(supervision.request("fake"), &live);
    assert!(supervision.backend.started()[0].log().closed);
}

#[test]
fn a_space_whose_place_was_taken_after_the_hub_let_it_through_starts_nothing() {
    // The hub refuses a create once every place is taken, but creates made at once may all pass it before any takes a place.
    let supervision = Supervision::new(0);
    supervision.create(supervision.request("fake"), &Cancellation::new());
    assert!(supervision.backend.started().is_empty());
    assert!(supervision.hub.status().spaces.is_empty());
}

#[test]
fn a_kept_space_that_cannot_be_bound_again_says_why() {
    crate::test_log::record();
    let reason = |supervision: &Supervision| supervision.kept_state().error;
    let live = Cancellation::new();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    // A directory outside the roots, unless the server is stopping.
    let supervision = Supervision::new(1);
    let restore = supervision.kept("outside", "fake", "/nonexistent", "fake-1");
    supervision.restore(restore, &live);
    assert_eq!(reason(&supervision).as_deref(), Some(DIRECTORY_UNAVAILABLE));
    let supervision = Supervision::new(1);
    let restore = supervision.kept("stopping", "fake", "/nonexistent", "fake-1");
    supervision.restore(restore, &cancelled);
    assert_ne!(reason(&supervision).as_deref(), Some(DIRECTORY_UNAVAILABLE));
    // No room for it.
    let supervision = Supervision::new(0);
    let restore = supervision.kept("full", "fake", "/tmp", "fake-1");
    supervision.restore(restore, &live);
    assert_eq!(reason(&supervision), Some(supervision.slots.full_error()));
    // A session the agent no longer has.
    let supervision = Supervision::new(1);
    let restore = supervision.kept("missing", "fake", "/tmp", "fake-99");
    supervision.restore(restore, &live);
    assert!(
        reason(&supervision)
            .unwrap()
            .starts_with("Could not resume the fake session")
    );
    // A session another binding holds, which may be let go.
    let supervision = Supervision::new(1);
    let restore = supervision.kept("held", "fake", "/tmp", &supervision.held());
    supervision.restore(restore, &live);
    assert!(
        reason(&supervision)
            .unwrap()
            .starts_with("Could not resume the fake session")
    );
    assert_eq!(
        supervision.kept_state().activity,
        tomori_protocol::Activity::Closed
    );
    // A session whose state cannot be read, closed even if closing fails.
    let supervision = Supervision::new(1);
    let session = supervision.left();
    supervision.backend.fail_snapshots(true);
    supervision.backend.fail_closing();
    let restore = supervision.kept("unread", "fake", "/tmp", &session);
    supervision.restore(restore, &live);
    assert!(
        reason(&supervision)
            .unwrap()
            .starts_with("Could not read the fake session again")
    );
    // A session bound as the server stops is closed, whether or not closing fails.
    let supervision = Supervision::new(1);
    let session = supervision.left();
    let restore = supervision.kept("closed", "fake", "/tmp", &session);
    supervision.hub.stopping();
    supervision.restore(restore, &live);
    assert!(supervision.backend.started()[0].log().closed);
    let supervision = Supervision::new(1);
    let session = supervision.left();
    supervision.backend.fail_closing();
    let restore = supervision.kept("late", "fake", "/tmp", &session);
    supervision.hub.stopping();
    supervision.restore(restore, &live);
    assert!(supervision.backend.started()[0].log().closed);
}

#[test]
fn a_kept_space_bound_again_is_driven_until_its_session_closes() {
    let supervision = Supervision::new(1);
    let session = supervision.left();
    let restore = supervision.kept("bound", "fake", "/tmp", &session);
    let hub = supervision.hub.clone();
    let backend = supervision.backend.clone();
    let closing = std::thread::spawn(move || {
        crate::test_support::eventually(|| {
            (hub.space_state("space-kept").activity != tomori_protocol::Activity::Unknown)
                .then_some(())
        });
        backend.started()[0].emit(Event::Closed { reason: None });
    });
    supervision.restore(restore, &Cancellation::new());
    closing.join().unwrap();
    assert_eq!(
        supervision.kept_state().activity,
        tomori_protocol::Activity::Closed
    );
    assert!(supervision.slots.acquire().is_some());
}

#[test]
fn a_session_driven_until_the_server_stops_is_closed_even_if_closing_fails() {
    crate::test_log::record();
    let supervision = Supervision::new(1);
    supervision.backend.fail_closing();
    let backend = supervision.hub.backends.get("fake").unwrap();
    let session = block_on(backend.create(SessionSpec {
        directory: "/tmp".to_owned(),
        title: "Driven".to_owned(),
    }))
    .unwrap();
    let (commands, receiver) = async_channel::unbounded();
    let space = supervision
        .hub
        .register_space(
            &supervision.request("fake"),
            session.id(),
            tomori_backend::Snapshot::default(),
            commands,
        )
        .unwrap();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    block_on(run_session(
        &supervision.hub,
        &space,
        session,
        receiver,
        Voices::default(),
        &cancelled,
    ));
    assert!(supervision.backend.started()[0].log().closed);
}

#[test]
fn sessions_are_listed_only_for_a_directory_and_backend_it_has() {
    let supervision = Supervision::new(1);
    let live = Cancellation::new();
    for (backend, directory) in [("fake", "/nonexistent"), ("gone", "/tmp")] {
        block_on(list_sessions(
            &supervision.scheduler,
            supervision.hub.clone(),
            "/ada".to_owned(),
            backend.to_owned(),
            directory.to_owned(),
            supervision.places(),
            &live,
        ));
    }
}

#[test]
fn a_check_that_takes_too_long_or_outlasts_the_server_refuses_the_directory() {
    crate::test_log::record();
    let scheduler = Faulty::new();
    let (_sender, resolved) = async_channel::bounded(1);
    let live = Cancellation::new();
    assert_eq!(
        block_on(checked(
            &scheduler,
            "/slow",
            resolved.clone(),
            Duration::ZERO,
            &live
        )),
        None
    );
    let cancelled = Cancellation::new();
    cancelled.cancel();
    assert_eq!(
        block_on(checked(
            &scheduler,
            "/slow",
            resolved,
            Duration::from_secs(60),
            &cancelled
        )),
        None
    );
    block_on(scheduler.inner.shutdown());
    assert!(!crate::test_log::logged("checking it took longer than").is_empty());
}

#[test]
fn a_driven_session_follows_its_new_name_its_failure_and_instructions_with_nothing_to_give() {
    let supervision = Supervision::new(1);
    let backend = supervision.hub.backends.get("fake").unwrap();
    let start = || {
        let session = block_on(backend.create(SessionSpec {
            directory: "/tmp".to_owned(),
            title: "Driven".to_owned(),
        }))
        .unwrap();
        let (commands, receiver) = async_channel::unbounded();
        let space = supervision
            .hub
            .register_space(
                &supervision.request("fake"),
                session.id(),
                tomori_backend::Snapshot::default(),
                commands.clone(),
            )
            .unwrap();
        let fake = supervision.backend.started().pop().unwrap();
        (session, space, fake, commands, receiver)
    };
    // Named anew once it has started, then closed.
    let (session, space, fake, _commands, receiver) = start();
    fake.emit(Event::Busy);
    let hub = supervision.hub.clone();
    let renaming = std::thread::spawn(move || {
        crate::test_support::eventually(|| {
            (hub.status().spaces[0].activity == tomori_protocol::Activity::Running).then_some(())
        });
        fake.rename("renamed");
        fake.emit(Event::Idle);
        fake.emit(Event::Closed { reason: None });
    });
    assert_eq!(
        block_on(drive(
            &supervision.hub,
            &space,
            &session,
            &receiver,
            None,
            &mut None
        )),
        None
    );
    renaming.join().unwrap();
    assert_eq!(supervision.hub.space_session(&space), "renamed");
    // Its events end in a failure.
    let (session, space, _fake, _commands, receiver) = start();
    block_on(session.close()).unwrap();
    assert_eq!(
        block_on(session.next_event()).unwrap(),
        Event::Closed { reason: None }
    );
    assert!(
        block_on(drive(
            &supervision.hub,
            &space,
            &session,
            &receiver,
            None,
            &mut None
        ))
        .is_some()
    );
    // Instructions are skipped once the space has closed, when none are due.
    let (session, space, _fake, commands, receiver) = start();
    supervision.hub.space_ended(&space, None);
    commands.try_send(SpaceCommand::Instructions).unwrap();
    commands.try_send(SpaceCommand::Close).unwrap();
    assert_eq!(
        block_on(drive(
            &supervision.hub,
            &space,
            &session,
            &receiver,
            None,
            &mut None
        )),
        None
    );
}

#[test]
fn the_supervisor_stops_once_the_hub_shuts_down_and_forgets_only_for_backends_it_has() {
    crate::test_log::record();
    let supervision = Supervision::new(1);
    block_on(forget_instructions(
        supervision.hub.clone(),
        "gone".to_owned(),
        "fake-1".to_owned(),
    ));
    block_on(forget_instructions(
        supervision.hub.clone(),
        "fake".to_owned(),
        "fake-1".to_owned(),
    ));
    supervision.backend.take_no_instructions();
    block_on(forget_instructions(
        supervision.hub.clone(),
        "fake".to_owned(),
        "fake-1".to_owned(),
    ));
    // Without roots configured, spaces run in the current directory.
    let server = Server::new(Backends::new(), Arc::new(NullTranscriber::new()));
    server.hub.shutdown();
    block_on(server.supervise(&supervision.scheduler, &Cancellation::new()));
    // Roots that cannot be read refuse every directory.
    assert!(available(Err("gone".to_owned())).roots().is_empty());
    // A connection unknown to the tracker is counted.
    let mut tracker = Tracker::default();
    tracker.record((7, Ok(1)));
    assert_eq!(tracker.statistics.requests, 1);
}

#[test]
fn a_deleted_spaces_session_is_deleted_only_once_nothing_else_holds_it_and_it_is_kept_no_more() {
    let supervision = Supervision::new(1);
    let finish = |backend: &str, session: &str, asked: bool| {
        block_on(finish_deletion(
            supervision.hub.clone(),
            crate::hub::Deleted {
                backend: backend.to_owned(),
                session: session.to_owned(),
                // Nobody is on this connection, so nobody is told.
                asker: asked.then(|| ("nobody".to_owned(), 7)),
            },
        ))
    };
    let left = supervision.left();
    // A backend this server no longer has deletes nothing, and takes no instructions to remove.
    finish("gone", &left, true);
    finish("gone", &left, false);
    assert!(supervision.backend.deleted().is_empty());
    assert!(supervision.backend.forgotten().is_empty());
    // Kept, it loses Tomori's instructions; asked, it is deleted.
    finish("fake", &left, false);
    assert_eq!(supervision.backend.forgotten()[0].0, left);
    finish("fake", &left, true);
    assert_eq!(supervision.backend.deleted(), [left]);
    // One the backend has bound again is the backend's to refuse, and keeps its instructions.
    let held = supervision.held();
    finish("fake", &held, true);
    assert_eq!(supervision.backend.deleted().len(), 1);
    assert_eq!(supervision.backend.forgotten().len(), 1);
    // One a listed space is bound to by now is neither deleted nor stripped of its instructions.
    let bound = supervision.left();
    let _restore = supervision.kept("deleting", "fake", "/tmp", &bound);
    finish("fake", &bound, true);
    finish("fake", &bound, false);
    assert_eq!(supervision.backend.deleted().len(), 1);
    assert_eq!(supervision.backend.forgotten().len(), 1);
    // A backend that no longer deletes sessions keeps it, without its instructions.
    supervision.backend.delete_nothing();
    let undeleted = supervision.left();
    finish("fake", &undeleted, true);
    assert_eq!(supervision.backend.deleted().len(), 1);
    assert_eq!(supervision.backend.forgotten()[1].0, undeleted);
    // While the record of spaces cannot be written, which could list the deleted space again after a restart, no
    // session is deleted: the kept space's state directory is gone, and its space closes, which changes the record.
    supervision
        .hub
        .restore_failed("space-kept", "Gone.".to_owned(), true);
    let unwritten = supervision.left();
    finish("fake", &unwritten, true);
    assert_eq!(supervision.backend.deleted().len(), 1);
    // It is kept, so it loses Tomori's instructions.
    assert_eq!(supervision.backend.forgotten()[2].0, unwritten);
}

#[test]
fn a_token_store_that_cannot_be_read_again_keeps_what_it_read() {
    crate::test_log::record();
    let state = std::env::temp_dir().join(format!("tomori-server-reload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let store = Arc::new(tomori_security::TokenStore::open(&state).unwrap());
    store.issue("Ada", tomori_security::Grant::Owner).unwrap();
    let server = server().with_tokens(store.clone(), false);
    std::fs::write(state.join("tokens.json"), "not json").unwrap();
    server.revoke_withdrawn();
    assert_eq!(store.entries().len(), 1);
    let _ = std::fs::remove_dir_all(&state);
    assert!(!crate::test_log::logged("Could not read the token store").is_empty());
}

#[test]
fn accept_failures_are_classified_by_their_effect_on_the_listener() {
    use io::ErrorKind::*;
    for kind in [
        ConnectionAborted,
        ConnectionReset,
        ConnectionRefused,
        Interrupted,
        WouldBlock,
        NetworkDown,
        NetworkUnreachable,
        HostUnreachable,
    ] {
        assert_eq!(
            accept_failure(&io::Error::from(kind)),
            AcceptFailure::Retry,
            "{kind:?}"
        );
    }
    assert_eq!(
        accept_failure(&io::Error::from(OutOfMemory)),
        AcceptFailure::Backoff
    );
    for kind in [InvalidInput, BrokenPipe, PermissionDenied, Other] {
        assert_eq!(
            accept_failure(&io::Error::from(kind)),
            AcceptFailure::Fatal,
            "{kind:?}"
        );
    }
    // A closed scheduler ends serving.
    let closed = io::Error::new(BrokenPipe, "scheduler is closed");
    assert_eq!(accept_failure(&closed), AcceptFailure::Fatal);
    #[cfg(unix)]
    {
        let classify = |code| accept_failure(&io::Error::from_raw_os_error(code));
        for code in [
            libc::ECONNABORTED,
            libc::ECONNRESET,
            libc::EINTR,
            libc::EAGAIN,
            libc::EPROTO,
            libc::ENETDOWN,
            libc::ENETUNREACH,
            libc::EHOSTUNREACH,
        ] {
            assert_eq!(classify(code), AcceptFailure::Retry, "{code}");
        }
        // Pending network errors of the new connection, and a firewall rejecting the peer, which Linux reports from accept.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        for code in [
            libc::EPERM,
            libc::ENOPROTOOPT,
            libc::EHOSTDOWN,
            libc::ENONET,
        ] {
            assert_eq!(classify(code), AcceptFailure::Retry, "{code}");
        }
        for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
            assert_eq!(classify(code), AcceptFailure::Backoff, "{code}");
        }
        for code in [
            libc::EBADF,
            libc::EINVAL,
            libc::ENOTSOCK,
            libc::EOPNOTSUPP,
            libc::EACCES,
        ] {
            assert_eq!(classify(code), AcceptFailure::Fatal, "{code}");
        }
    }
}

#[test]
fn a_recurring_per_peer_accept_error_pauses_accepting() {
    crate::test_log::record();
    let resets = (0..=ACCEPT_RETRIES)
        .map(|_| io::Error::from(io::ErrorKind::ConnectionReset))
        .collect::<Vec<_>>();
    let started = Instant::now();
    let running = start(resets, false, DEFAULT_DRAIN_TIMEOUT);
    assert!(health(running.address).starts_with("HTTP/1.1 200"));
    // One more reset than the immediate retries allowed paused accepting before the request was accepted.
    let elapsed = started.elapsed();
    assert!(elapsed >= ACCEPT_BACKOFF, "{elapsed:?}");
    running.cancellation.cancel();
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert_eq!(result.unwrap().connections, 1);
}

#[test]
fn serving_continues_after_transient_accept_errors() {
    crate::test_log::record();
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut errors = vec![
        io::Error::from(io::ErrorKind::ConnectionAborted),
        io::Error::from(io::ErrorKind::ConnectionReset),
        io::Error::from(io::ErrorKind::NetworkDown),
        io::Error::from(io::ErrorKind::HostUnreachable),
        io::Error::from(io::ErrorKind::OutOfMemory),
    ];
    #[cfg(unix)]
    errors.extend([
        io::Error::from_raw_os_error(libc::EMFILE),
        io::Error::from_raw_os_error(libc::EPROTO),
        io::Error::from_raw_os_error(libc::ENFILE),
    ]);
    let failures = errors.len();
    let running = start(errors, false, DEFAULT_DRAIN_TIMEOUT);
    assert!(health(running.address).starts_with("HTTP/1.1 200"));
    assert!(health(running.address).starts_with("HTTP/1.1 200"));
    running.cancellation.cancel();
    let (result, accepts) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    let statistics = result.unwrap();
    assert_eq!(statistics.connections, 2);
    assert_eq!(statistics.requests, 2);
    assert!(accepts > failures + 2, "only {accepts} accepts");
}

#[test]
fn a_failure_to_accept_ends_the_server() {
    let broken = io::Error::other("the listener is broken");
    let running = start(vec![broken], false, DEFAULT_DRAIN_TIMEOUT);
    // Nothing cancels the server: the spaces, sweep and pacing end because serving failed.
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    let Err(Error::Io(_)) = result else {
        unreachable!("serving fails with the accept's error, not {result:?}");
    };
    assert!(!running.cancellation.is_cancelled());
    // The listener is closed. Another socket listening on the port since would only make this fail spuriously, never pass falsely; and tests bind only ephemeral ports, which the system hands out across a wide range before reusing one, so reuse within this window is unlikely.
    assert!(TcpStream::connect(running.address).is_err());
}

#[test]
fn the_listener_closes_before_a_bounded_drain() {
    let drain = Duration::from_secs(1);
    let running = start(Vec::new(), true, drain);
    // This connection is served, then never finishes closing.
    assert!(health(running.address).starts_with("HTTP/1.1 200"));
    let cancelled = Instant::now();
    running.cancellation.cancel();
    // New peers are refused while the connection drains, rather than queued. A peer that connects as the server is cancelled may still be accepted, since an accept that completes wins over a concurrent cancellation, but the accept after it is already cancelled, so at most one is.
    // Probed at least once, whether or not the first probe still connects, so that how the race went changes no coverage.
    let mut probes = 0;
    let mut open = true;
    while open {
        open = TcpStream::connect(running.address).is_ok();
        probes += usize::from(open);
        assert!(
            !open | (cancelled.elapsed() < drain / 2),
            "the listener stayed open"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        running.finished.try_recv().is_err(),
        "the drain ended early"
    );
    let (result, _) = running
        .finished
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    let elapsed = cancelled.elapsed();
    assert!(elapsed >= drain, "the drain ended after {elapsed:?}");
    assert!(elapsed < drain * 4, "the drain took {elapsed:?}");
    let connections = result.unwrap().connections;
    // Every condition is evaluated, so that how the race went changes no coverage.
    let served = (connections == 1) | ((connections == 2) & (probes > 0));
    assert!(
        served,
        "{connections} connections served after {probes} peers connected while cancelling"
    );
}

/// An error whose message already ends with its cause's.
#[derive(Debug)]
struct Repeating(std::io::Error);

impl std::fmt::Display for Repeating {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "writing failed: {}", self.0)
    }
}

impl std::error::Error for Repeating {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[test]
fn a_cause_an_error_already_says_is_not_said_twice() {
    let error = Repeating(io::Error::other("the peer reset"));
    assert_eq!(describe(&error), "writing failed: the peer reset");
}
