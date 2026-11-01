// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Calls to the OpenCode server's HTTP interface.
//!
//! Every helper builds a [`Request`], submits it through the pooled
//! [`Client`], and parses the JSON body. The client and its requests borrow
//! from the scheduler, so these run on the connection thread.

use base_http::{
    Field, Method, Path, Request,
    body::{BodyFrame, BufferedBody, EmptyBody, ReadableBody},
};
use base_http_client::{Client, ResponseBody};
use base_scheduler::Cancellation;
use serde_json::Value;

use crate::endpoint::Resolved;
use crate::wire::question_from_form;

/// A failed call, with the HTTP status when the server answered.
pub struct Failure {
    pub status: Option<u16>,
    pub message: String,
}

impl From<base_http_client::Error> for Failure {
    fn from(error: base_http_client::Error) -> Self {
        Failure {
            status: None,
            message: error.to_string(),
        }
    }
}

/// Submit a request, appending the endpoint's credentials when it has them.
async fn exchange<'scope, B>(
    client: &Client<'scope>,
    mut request: Request<B>,
    auth: Option<&str>,
    cancellation: &Cancellation,
) -> Result<base_http::Response<ResponseBody>, Failure>
where
    B: ReadableBody + 'scope,
    B::Data: Into<Vec<u8>>,
{
    if let Some(authorization) = auth {
        let field = Field::new("authorization", authorization).map_err(|error| Failure {
            status: None,
            message: error.to_string(),
        })?;
        request.fields.append(field).map_err(|error| Failure {
            status: None,
            message: error.to_string(),
        })?;
    }
    // Opt out of connection reuse: opencode closes idle keep-alive connections
    // after five seconds, and `base-http-client` pools and reuses connections
    // without retrying. A request that reuses a connection opencode already
    // dropped reads EOF and fails with "truncated HTTP/1 message" (base-rs/
    // base-http#36). `Connection: close` keeps the client from reusing: each
    // call gets a fresh connection, which stays valid for the single exchange.
    if let Ok(field) = Field::new("connection", "close") {
        request.fields.append(field).map_err(|error| Failure {
            status: None,
            message: error.to_string(),
        })?;
    }
    Ok(client.call(request, cancellation).await?)
}

/// Read a response body to the end, failing with the status when the server
/// refused the call.
async fn read_json(response: base_http::Response<ResponseBody>) -> Result<Value, Failure> {
    let status = response.status.as_u16();
    let mut body = response.body;
    let mut bytes = Vec::new();
    let cancellation = Cancellation::new();
    loop {
        let frame = body.read(&cancellation).await.map_err(|error| Failure {
            status: Some(status),
            message: error.to_string(),
        })?;
        let end = frame.is_end();
        if let BodyFrame::Data { data, .. } = frame {
            bytes.extend_from_slice(&data);
        }
        if end {
            break;
        }
    }
    if !(200..300).contains(&status) {
        let message = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .or_else(|| value.get("data").and_then(|data| data.get("message")))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("the server answered with status {status}"));
        return Err(Failure {
            status: Some(status),
            message,
        });
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|error| Failure {
        status: Some(status),
        message: format!("the server sent an unparseable body: {error}"),
    })
}

fn post(path: &str, payload: &Value) -> Request<BufferedBody> {
    let mut request = Request::new(
        Method::POST,
        Path::from(path.to_owned()),
        BufferedBody::from(payload.to_string().into_bytes()),
    );
    if let Ok(field) = Field::new("content-type", "application/json") {
        let _ = request.fields.append(field);
    }
    request
}

fn get(path: &str) -> Request<EmptyBody> {
    Request::get(Path::from(path.to_owned()))
}

/// `POST /api/session`: start a session in `directory`, optionally titled.
pub async fn create_session<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    directory: &str,
    title: Option<&str>,
    cancellation: &Cancellation,
) -> Result<Value, Failure> {
    let mut payload = serde_json::json!({ "location": { "directory": directory } });
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        payload["title"] = Value::String(title.to_owned());
    }
    let response = exchange(
        client,
        post("/api/session", &payload),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    let value = read_json(response).await?;
    // The session record is nested under `data`, as with the list endpoint.
    Ok(value.get("data").cloned().unwrap_or(value))
}

/// `GET /api/session?directory=`: the existing sessions in a directory.
pub async fn list_sessions<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    directory: &str,
    cancellation: &Cancellation,
) -> Result<Vec<Value>, Failure> {
    let path = format!("/api/session?directory={}", urlencoding(directory));
    let response = exchange(
        client,
        get(&path),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    let value = read_json(response).await?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// `GET /api/session/:id`: one session's record.
pub async fn session_info<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    cancellation: &Cancellation,
) -> Result<Value, Failure> {
    let path = format!("/api/session/{}", urlencoding(session_id));
    let response = exchange(
        client,
        get(&path),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    let value = read_json(response).await?;
    // The session record is nested under `data`, as with the list endpoint.
    Ok(value.get("data").cloned().unwrap_or(value))
}

/// `GET /api/session?parentID=`: the children of a session.
pub async fn child_sessions<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    parent_id: &str,
    cancellation: &Cancellation,
) -> Result<Vec<Value>, Failure> {
    let path = format!("/api/session?parentID={}", urlencoding(parent_id));
    let response = exchange(
        client,
        get(&path),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    let value = read_json(response).await?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// `POST /api/session/:id/prompt`: deliver one user turn. `key` is what the
/// `session.inbox.delivered` event names when the turn is accepted.
pub async fn send_prompt<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    key: &str,
    text: &str,
    cancellation: &Cancellation,
) -> Result<(), Failure> {
    let path = format!("/api/session/{}/prompt", urlencoding(session_id));
    let payload = serde_json::json!({
        "id": key,
        "text": text,
        "delivery": "queue"
    });
    let response = exchange(
        client,
        post(&path, &payload),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    read_json(response).await?;
    Ok(())
}

/// `POST /api/session/:id/interrupt`: stop the running turn.
pub async fn interrupt<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    cancellation: &Cancellation,
) -> Result<(), Failure> {
    let path = format!("/api/session/{}/interrupt", urlencoding(session_id));
    let response = exchange(
        client,
        post(&path, &Value::Object(serde_json::Map::new())),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    read_json(response).await?;
    Ok(())
}

/// `GET /api/session/:id/permission`: the session's open permission requests.
pub async fn open_permissions<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    cancellation: &Cancellation,
) -> Result<Vec<Value>, Failure> {
    let path = format!("/api/session/{}/permission", urlencoding(session_id));
    let response = exchange(
        client,
        get(&path),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    let value = read_json(response).await?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// `POST /api/session/:id/permission/:requestID/reply`: answer a permission.
/// `decision` is `once`, `always` or `reject`.
pub async fn reply_permission<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    request_id: &str,
    decision: &str,
    cancellation: &Cancellation,
) -> Result<(), Failure> {
    let path = format!(
        "/api/session/{}/permission/{}/reply",
        urlencoding(session_id),
        urlencoding(request_id)
    );
    let payload = serde_json::json!({ "decision": decision });
    let response = exchange(
        client,
        post(&path, &payload),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    read_json(response).await?;
    Ok(())
}

/// `GET /api/session/:id/form`: the session's open forms.
pub async fn open_forms<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    cancellation: &Cancellation,
) -> Result<Vec<Value>, Failure> {
    let path = format!("/api/session/{}/form", urlencoding(session_id));
    let response = exchange(
        client,
        get(&path),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    let value = read_json(response).await?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// `POST /api/session/:id/form/:formID/reply`: answer a form.
pub async fn reply_form<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    form_id: &str,
    answer: &Value,
    cancellation: &Cancellation,
) -> Result<(), Failure> {
    let path = format!(
        "/api/session/{}/form/{}/reply",
        urlencoding(session_id),
        urlencoding(form_id)
    );
    let payload = serde_json::json!({ "answer": answer });
    let response = exchange(
        client,
        post(&path, &payload),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    read_json(response).await?;
    Ok(())
}

/// `GET /api/session/:id/message`: the session's messages, for a snapshot.
pub async fn session_messages<'scope>(
    client: &Client<'scope>,
    resolved: &Resolved,
    session_id: &str,
    cancellation: &Cancellation,
) -> Result<Value, Failure> {
    let path = format!("/api/session/{}/message", urlencoding(session_id));
    let response = exchange(
        client,
        get(&path),
        resolved.authorization_header().as_deref(),
        cancellation,
    )
    .await?;
    read_json(response).await
}

/// The pieces of a session needed to reconstruct a [`backend::Snapshot`]:
/// its record, its messages, its open permission and form, and its children.
pub struct SnapshotParts {
    pub messages: Value,
    pub permissions: Vec<Value>,
    pub forms: Vec<Value>,
    pub children: Vec<Value>,
}

impl SnapshotParts {
    /// The snapshot's workers, derived from the children.
    pub fn workers(&self) -> Vec<backend::Worker> {
        self.children
            .iter()
            .filter_map(crate::wire::worker_from_session)
            .collect()
    }

    /// The snapshot's single pending permission, if any.
    pub fn permission(&self) -> Option<backend::Permission> {
        self.permissions
            .first()
            .and_then(crate::wire::permission_from_request)
    }

    /// The snapshot's single pending question, if any.
    pub fn question(&self) -> Option<backend::Question> {
        self.forms
            .first()
            .and_then(|form| form.get("form").or(Some(form)))
            .map(question_from_form)
    }

    /// Fetch every piece for `session_id`.
    pub async fn fetch<'scope>(
        client: &Client<'scope>,
        resolved: &Resolved,
        session_id: &str,
        cancellation: &Cancellation,
    ) -> Result<Self, Failure> {
        // Validate the session exists before fetching its parts.
        let _info = session_info(client, resolved, session_id, cancellation).await?;
        let messages = session_messages(client, resolved, session_id, cancellation).await?;
        let permissions = open_permissions(client, resolved, session_id, cancellation).await?;
        let forms = open_forms(client, resolved, session_id, cancellation).await?;
        let children = child_sessions(client, resolved, session_id, cancellation).await?;
        Ok(Self {
            messages,
            permissions,
            forms,
            children,
        })
    }
}

/// Percent-encode the segment of a path: ids and directories are opaque.
fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
