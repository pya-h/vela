//! Keeping the server to this user's IDE page (see `ide.md` §7):
//!
//! - the session token, generated per launch and required on every API
//!   request (`Authorization: Bearer …`; WebSockets send it as their first
//!   message, see `ws`), compared in constant time;
//! - `Host` must be `127.0.0.1:PORT` or `localhost:PORT`, which defeats DNS
//!   rebinding, and an `Origin`, when sent, must be the page's own, which
//!   stops other websites; no CORS headers are ever sent;
//! - response headers that forbid framing, sniffing, and, on the page, any
//!   script or connection that isn't the server's own.

use std::fmt;
use std::io;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::Studio;
use crate::api::ApiError;

/// The per-launch secret: 32 random bytes as hex.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn generate() -> io::Result<Token> {
        random_hex(32).map(Token)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `candidate` is this token, taking the same time for every
    /// candidate of the right length.
    pub fn matches(&self, candidate: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), candidate.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
    }
}

/// Keeps the token out of logs and panic messages.
impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

/// `bytes` is a byte count, not a bit count.
pub fn random_hex(bytes: usize) -> io::Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|error| io::Error::other(format!("no random numbers: {}", error)))?;
    Ok(buffer.iter().map(|b| format!("{:02x}", b)).collect())
}

pub fn host_allowed(host: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost"]
        .iter()
        .any(|name| host.eq_ignore_ascii_case(&format!("{}:{}", name, port)))
}

pub fn origin_allowed(origin: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost"]
        .iter()
        .any(|name| origin.eq_ignore_ascii_case(&format!("http://{}:{}", name, port)))
}

/// The `Content-Security-Policy` for the page: only the server's own
/// scripts, styles, fonts, and connections, plus compiling WebAssembly.
/// `nonce` allows the `<style>` elements the editor creates at run time
/// (CodeMirror adds its styles that way); the page reads it from its
/// `csp-nonce` meta tag.
pub fn content_security_policy(port: u16, nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'nonce-{nonce}'; img-src 'self' data:; \
         font-src 'self'; connect-src 'self' ws://127.0.0.1:{port} ws://localhost:{port}; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

pub async fn guard(State(studio): State<Arc<Studio>>, request: Request, next: Next) -> Response {
    let port = studio.config.port;
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !host.is_some_and(|host| host_allowed(host, port)) {
        return ApiError::new(StatusCode::FORBIDDEN, "unexpected Host header").into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN)
        && !origin
            .to_str()
            .is_ok_and(|origin| origin_allowed(origin, port))
    {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "requests from other origins are not allowed",
        )
        .into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

/// API responses are never cached.
pub async fn require_token(
    State(studio): State<Arc<Studio>>,
    request: Request,
    next: Next,
) -> Response {
    if !bearer_matches(request.headers(), &studio.config.token) {
        let mut response =
            ApiError::new(StatusCode::UNAUTHORIZED, "missing or wrong session token")
                .into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn bearer_matches(headers: &HeaderMap, token: &Token) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|candidate| token.matches(candidate))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

    use super::*;

    #[test]
    fn tokens_are_random_hex_and_compare_exactly() {
        let (a, b) = (Token::generate().unwrap(), Token::generate().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 64);
        assert!(a.as_str().bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(a.matches(a.as_str()));
        assert!(!a.matches(b.as_str()));
        assert!(!a.matches(&a.as_str()[..63]));
        assert!(!a.matches(""));
        assert_eq!(format!("{:?}", a), "Token(…)");
    }

    #[test]
    fn only_this_servers_host_and_origin_are_allowed() {
        assert!(host_allowed("127.0.0.1:4000", 4000));
        assert!(host_allowed("LOCALHOST:4000", 4000));
        for host in [
            "127.0.0.1:4001",
            "127.0.0.1",
            "evil.example:4000",
            "localhost.evil.example:4000",
            "[::1]:4000",
            "0.0.0.0:4000",
        ] {
            assert!(!host_allowed(host, 4000), "{}", host);
        }
        assert!(origin_allowed("http://localhost:4000", 4000));
        for origin in [
            "https://127.0.0.1:4000",
            "http://127.0.0.1:4001",
            "http://evil.example",
            "null",
            "http://127.0.0.1:4000/",
        ] {
            assert!(!origin_allowed(origin, 4000), "{}", origin);
        }
    }
}
