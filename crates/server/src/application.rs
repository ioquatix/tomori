// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::convert::Infallible;
use std::sync::Arc;

use base_http::body::{BodyError, BodyFrame, ReadableBody};
use base_http::{InterimHandler, InterimResponses, Method, Request, Response, Status};
use base_http_server::Handler;
use base_scheduler::Cancellation;

use crate::connection::ConnectionBody;
use crate::hub::Hub;

/// The HTTP routes of the server: `POST /connect` for client connections and `GET /health`.
///
/// The cancellation passed to each call must be specific to that HTTP connection, as [`crate::Server::run`] arranges: a connection that goes silent is ended by cancelling it.
pub struct Application {
    hub: Arc<Hub>,
}

impl Application {
    pub(crate) fn new(hub: Arc<Hub>) -> Self {
        Self { hub }
    }
}

/// A response body: either a client connection or a small fixed document.
pub enum ResponseBody<B: ReadableBody> {
    Connection(ConnectionBody<B>),
    Fixed(Option<Vec<u8>>, u64),
}

impl<B: ReadableBody> ResponseBody<B> {
    fn fixed(data: impl Into<Vec<u8>>) -> Self {
        let data = data.into();
        let length = data.len() as u64;
        Self::Fixed(Some(data), length)
    }
}

impl<B: ReadableBody> ReadableBody for ResponseBody<B> {
    type Data = Vec<u8>;
    type Error = BodyError;

    fn is_ready(&self) -> bool {
        match self {
            Self::Connection(body) => body.is_ready(),
            Self::Fixed(..) => true,
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Fixed(None, _))
    }

    fn length(&self) -> Option<u64> {
        match self {
            Self::Connection(_) => None,
            Self::Fixed(_, length) => Some(*length),
        }
    }

    fn may_have_trailers(&self) -> bool {
        false
    }

    fn close(&mut self, error: Option<BodyError>) {
        match self {
            Self::Connection(body) => body.close(error),
            Self::Fixed(data, _) => *data = None,
        }
    }

    async fn read(&mut self, cancellation: &Cancellation) -> Result<BodyFrame<Vec<u8>>, BodyError> {
        match self {
            Self::Connection(body) => body.read(cancellation).await,
            Self::Fixed(data, _) => Ok(match data.take() {
                Some(data) => BodyFrame::Data { data, end: true },
                None => BodyFrame::End,
            }),
        }
    }
}

fn response<B: ReadableBody>(
    status: Status,
    content_type: &str,
    body: ResponseBody<B>,
) -> Response<ResponseBody<B>> {
    let mut response = Response::new(status, body);
    // These names and values are valid header syntax.
    response
        .fields
        .add("Content-Type", content_type)
        .expect("valid content type");
    response
        .fields
        .add("Cache-Control", "no-store")
        .expect("valid cache control");
    response
}

impl Handler for Application {
    type Error = Infallible;
    type Body<B: ReadableBody> = ResponseBody<B>;

    async fn call<B: ReadableBody, I: InterimHandler>(
        &self,
        request: Request<B>,
        _interim: &mut InterimResponses<I>,
        cancellation: &Cancellation,
    ) -> Result<Response<ResponseBody<B>>, Infallible> {
        let path = request.path.as_ref().map_or("", |path| path.as_str());
        let path = path.split('?').next().unwrap_or_default();
        let get = request.method == Method::GET;
        let post = request.method == Method::POST;
        Ok(match path {
            protocol::CONNECT_PATH if post => {
                let body =
                    ConnectionBody::new(self.hub.clone(), request.body, cancellation.clone());
                response(
                    Status::OK,
                    protocol::CONTENT_TYPE,
                    ResponseBody::Connection(body),
                )
            }
            "/health" if get => response(
                Status::OK,
                "application/json",
                ResponseBody::fixed(format!(
                    "{{\"status\":\"ok\",\"server\":\"{}\",\"protocol\":{}}}\n",
                    crate::SERVER_NAME,
                    protocol::PROTOCOL_VERSION
                )),
            ),
            protocol::CONNECT_PATH | "/health" => response(
                Status::new(405).expect("valid status"),
                "text/plain",
                ResponseBody::fixed("Method not allowed.\n"),
            ),
            _ => response(
                Status::NOT_FOUND,
                "text/plain",
                ResponseBody::fixed("Not found.\n"),
            ),
        })
    }
}
