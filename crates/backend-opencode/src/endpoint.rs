// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Resolution of where the OpenCode server is and how to authenticate to it.
//!
//! The canonical order is: an explicit URL given to the backend (typically from
//! `--opencode-url`), the `OPENCODE_URL` environment variable, the `opencode
//! serve --service` registration file in the XDG state directory, and finally
//! the well-known default `http://127.0.0.1:4096`.

/// A resolved OpenCode service endpoint: its host and port, and optional HTTP
/// basic-auth credentials.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub host: String,
    pub port: u16,
    /// `(username, password)` when the server requires HTTP basic auth.
    pub auth: Option<(String, String)>,
}

impl Resolved {
    /// The value of the `Host` header, `host:port`.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The `Authorization` header value for basic auth, when present.
    pub fn authorization_header(&self) -> Option<String> {
        let (user, password) = self.auth.as_ref()?;
        Some(format!(
            "Basic {}",
            base64_basic(&format!("{user}:{password}"))
        ))
    }
}

/// The well-known default when no flag, env var or service file is present.
pub const DEFAULT_URL: &str = "http://127.0.0.1:4096";

/// The directory `opencode serve --service` writes its registration file to,
/// in the XDG state directory: `$XDG_STATE_HOME/opencode`, else
/// `$HOME/.local/state/opencode`.
pub fn xdg_state_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join(".local").join("state"))
        })?;
    Some(base.join("opencode"))
}

/// Parse an `http://host:port` URL into a [`Resolved`]. `None` for a malformed
/// URL, an unsupported scheme, `localhost` without a port, or a port out of
/// range.
pub fn resolve_url(url: &str) -> Option<Resolved> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let hostport = rest.split(['/', '?']).next()?.trim_end_matches('/');
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) => (host, port),
        // A bare hostname means port 443 for https and 80 for http, but OpenCode
        // always listens on an explicit port; refuse the ambiguity.
        _ => return None,
    };
    let host = host.to_owned();
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some(Resolved {
        host,
        port,
        auth: None,
    })
}

/// Read the `opencode serve --service` registration file, returning the
/// recorded URL and credentials. The file is a JSON object with fields
/// `id`, `version`, `url`, `pid`, `password`.
pub fn read_service_file() -> Option<Resolved> {
    let path = xdg_state_dir()?.join("service.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let url = value.get("url")?.as_str()?;
    let mut resolved = resolve_url(url)?;
    if let Some(password) = value.get("password").and_then(|v| v.as_str())
        && !password.is_empty()
    {
        let username = value
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("opencode")
            .to_owned();
        resolved.auth = Some((username, password.to_owned()));
    }
    Some(resolved)
}

/// Resolve the OpenCode service in the canonical order: `explicit`, the
/// `OPENCODE_URL` environment variable, the XDG state file, then the default
/// URL.
pub fn resolve(explicit: Option<&str>) -> Resolved {
    if let Some(url) = explicit.map(str::trim).filter(|url| !url.is_empty())
        && let Some(resolved) = resolve_url(url)
    {
        return resolved;
    }
    if let Ok(url) = std::env::var("OPENCODE_URL")
        && !url.trim().is_empty()
        && let Some(resolved) = resolve_url(url.trim())
    {
        return resolved;
    }
    if let Some(resolved) = read_service_file() {
        return resolved;
    }
    resolve_url(DEFAULT_URL).expect("the default URL is well-formed")
}

/// The standard base64 alphabet without padding, enough for the short
/// `user:password` credential string that HTTP basic auth carries.
fn base64_basic(bytes: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.as_bytes().chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_expected() {
        // The RFC 4628 examples, without padding.
        assert_eq!(base64_basic("opencode:secret"), "b3BlbmNvZGU6c2VjcmV0");
        assert_eq!(base64_basic("a"), "YQ");
        assert_eq!(base64_basic("a b"), "YSBi");
        assert_eq!(base64_basic("ab"), "YWI");
        assert_eq!(base64_basic(""), "");
    }

    #[test]
    fn url_resolution_reads_host_and_port() {
        assert!(resolve_url("http://localhost").is_none(), "no port");
        assert!(resolve_url("ftp://127.0.0.1:4096").is_none());
        assert!(resolve_url("http://127.0.0.1:0").is_none());
        assert!(resolve_url("http://127.0.0.1:99999").is_none());
        let resolved = resolve_url(DEFAULT_URL).unwrap();
        assert_eq!(resolved.host, "127.0.0.1");
        assert_eq!(resolved.port, 4096);
        assert!(resolved.auth.is_none());
        assert_eq!(resolved.authority(), "127.0.0.1:4096");
        let resolved = resolve_url("http://example.com:8080/path").unwrap();
        assert_eq!(resolved.host, "example.com");
        assert_eq!(resolved.port, 8080);
    }

    #[test]
    fn the_authorization_header_uses_basic_auth() {
        let mut resolved = resolve_url(DEFAULT_URL).unwrap();
        resolved.auth = Some(("opencode".to_owned(), "secret".to_owned()));
        assert_eq!(
            resolved.authorization_header().as_deref(),
            Some("Basic b3BlbmNvZGU6c2VjcmV0")
        );
    }
}
