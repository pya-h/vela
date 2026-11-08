//! Embedded in release builds, read from `ide/web/dist` in debug ones;
//! until the frontend is built, the page explains how.

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::{self, HeaderValue};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::Studio;
use crate::api::ApiError;
use crate::security::{content_security_policy, random_hex};

#[derive(RustEmbed)]
#[folder = "../web/dist"]
#[allow_missing = true]
struct Frontend;

/// Shown when `ide/web/dist` hasn't been built.
const NOT_BUILT_PAGE: &str = include_str!("not-built.html");
const NOT_BUILT_STYLE: &str = include_str!("not-built.css");
const NOT_BUILT_STYLE_PATH: &str = "vela-studio-not-built.css";
/// Replaced in HTML pages by a fresh nonce for each response.
const NONCE_PLACEHOLDER: &str = "__VELA_CSP_NONCE__";

pub async fn serve(State(studio): State<Arc<Studio>>, method: Method, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return ApiError::new(StatusCode::NOT_FOUND, "no such API endpoint").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = if path.is_empty() { "index.html" } else { path };
    let (body, content_type, cache) = match Frontend::get(path) {
        Some(file) => {
            // Vite puts content-hashed files in assets/, so they never change.
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (file.data.into_owned(), content_type(path), cache)
        }
        None if path == "index.html" => (
            NOT_BUILT_PAGE.as_bytes().to_vec(),
            content_type(path),
            "no-cache",
        ),
        None if path == NOT_BUILT_STYLE_PATH => (
            NOT_BUILT_STYLE.as_bytes().to_vec(),
            content_type(path),
            "no-cache",
        ),
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let (body, policy) = if content_type.starts_with("text/html") {
        let Ok(nonce) = random_hex(16) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let page = String::from_utf8_lossy(&body).replace(NONCE_PLACEHOLDER, &nonce);
        let Ok(policy) =
            HeaderValue::from_str(&content_security_policy(studio.config.port, &nonce))
        else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        (page.into_bytes(), Some(policy))
    } else {
        (body, None)
    };
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Some(policy) = policy {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    response
}

fn content_type(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
