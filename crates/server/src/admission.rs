// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Deadlines for HTTP connections that are not serving a request.
//!
//! A peer that opens a socket and sends nothing, or trickles a request head, or leaves a keep-alive connection idle, would otherwise hold one of the server's connection slots for ever. Each connection is idle from when it is accepted until its request head is complete, and again from when a response is finished until the next head is complete. A connection that stays idle for the request timeout is cancelled. `/connect` exchanges are then covered by the hub, which releases a connection that has not said Hello within the same timeout, and by the keepalive sweep. See `design/server.md`.
//!
//! TODO(upstream): base-http1's `RequestReader::read_request` and base-http-server's `serve_connection` have no request-head or keep-alive idle timeout. Once `base_http1::Options` (or `base_http_server::Options`) offers a deadline for reading a request head, measured from accept and from the end of the previous response, use it and remove this module.

use std::convert::Infallible;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use base_http::body::{BodyFrame, ReadableBody};
use base_http::{InterimHandler, InterimResponses, Request, Response};
use base_http_server::Handler;
use base_scheduler::Cancellation;

use crate::application::{Application, ResponseBody};

/// The default request timeout.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The connections being watched.
#[derive(Default)]
pub struct Admission {
    watches: Mutex<Vec<Weak<Watch>>>,
}

/// One connection's progress.
pub struct Watch {
    /// When the connection became idle, as first seen by a sweep, or `None` while a request is being served.
    idle: Mutex<Idle>,
    /// Ends only this connection.
    cancellation: Cancellation,
}

enum Idle {
    /// Idle, and not yet seen by a sweep.
    Unseen,
    Since(Instant),
    Serving,
}

impl Watch {
    fn set(&self, idle: Idle) {
        *self.idle.lock().expect("the watch is never left poisoned") = idle;
    }
}

impl Admission {
    /// Watch a newly accepted connection, which is idle until its first request head is complete.
    pub fn watch(&self, cancellation: Cancellation) -> Arc<Watch> {
        let watch = Arc::new(Watch {
            idle: Mutex::new(Idle::Unseen),
            cancellation,
        });
        let mut watches = self
            .watches
            .lock()
            .expect("the watches are never left poisoned");
        watches.retain(|watch| watch.strong_count() > 0);
        watches.push(Arc::downgrade(&watch));
        watch
    }

    /// Cancel connections that have been idle for `timeout`. An idle connection's time is measured from the first sweep that sees it idle.
    pub fn sweep(&self, now: Instant, timeout: Duration) {
        let mut expired = Vec::new();
        {
            let mut watches = self
                .watches
                .lock()
                .expect("the watches are never left poisoned");
            watches.retain(|watch| {
                let Some(watch) = watch.upgrade() else {
                    return false;
                };
                let mut idle = watch.idle.lock().expect("the watch is never left poisoned");
                match *idle {
                    Idle::Unseen => *idle = Idle::Since(now),
                    Idle::Since(since) if now.saturating_duration_since(since) >= timeout => {
                        expired.push(watch.cancellation.clone());
                        return false;
                    }
                    Idle::Since(_) | Idle::Serving => {}
                }
                true
            });
        }
        if !expired.is_empty() {
            log::debug!(
                "Closing {} connections that sent no request in time.",
                expired.len()
            );
        }
        for cancellation in expired {
            cancellation.cancel();
        }
    }
}

/// The application, serving one watched connection.
pub struct Admitted<'a> {
    pub application: &'a Application,
    pub watch: Arc<Watch>,
}

impl Handler for Admitted<'_> {
    type Error = Infallible;
    type Body<B: ReadableBody> = Tracked<ResponseBody<B>>;

    async fn call<B: ReadableBody, I: InterimHandler>(
        &self,
        request: Request<B>,
        interim: &mut InterimResponses<I>,
        cancellation: &Cancellation,
    ) -> Result<Response<Self::Body<B>>, Infallible> {
        // The request head is complete.
        self.watch.set(Idle::Serving);
        let response = self
            .application
            .call(request, interim, cancellation)
            .await?;
        let watch = self.watch.clone();
        Ok(response.map_body(|body| Tracked { body, watch }))
    }
}

/// A response body that marks its connection idle once the response is finished.
pub struct Tracked<B> {
    body: B,
    watch: Arc<Watch>,
}

impl<B> Drop for Tracked<B> {
    fn drop(&mut self) {
        self.watch.set(Idle::Unseen);
    }
}

impl<B: ReadableBody> ReadableBody for Tracked<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn is_ready(&self) -> bool {
        self.body.is_ready()
    }

    fn is_empty(&self) -> bool {
        self.body.is_empty()
    }

    fn length(&self) -> Option<u64> {
        self.body.length()
    }

    fn may_have_trailers(&self) -> bool {
        self.body.may_have_trailers()
    }

    fn close(&mut self, error: Option<B::Error>) {
        self.body.close(error)
    }

    async fn read(&mut self, cancellation: &Cancellation) -> Result<BodyFrame<B::Data>, B::Error> {
        self.body.read(cancellation).await
    }
}
