#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use vela_studio::{Config, Studio, Token, router};

pub const PORT: u16 = 4000;

pub const HELLO: &str = "task main()\n    show(\"Hello, Vela!\")\nfi\n";

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(test: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("vela-studio-test-{}-{}-{}", std::process::id(), test, n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ws")).unwrap();
        fs::create_dir_all(dir.join("outside")).unwrap();
        TempDir(dir)
    }

    pub fn workspace(&self) -> PathBuf {
        self.0.join("ws")
    }

    pub fn outside(&self) -> PathBuf {
        self.0.join("outside")
    }

    pub fn write(&self, path: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let full = self.workspace().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, content).unwrap();
        full
    }

    pub fn read(&self, path: &str) -> String {
        fs::read_to_string(self.workspace().join(path)).unwrap()
    }

    pub fn exists(&self, path: &str) -> bool {
        self.workspace().join(path).symlink_metadata().is_ok()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

pub fn config(workspace: &Path, port: u16) -> Config {
    let mut config = Config::new(workspace.to_path_buf(), port, Token::generate().unwrap());
    config.shell = Some("/bin/sh".to_string());
    config
}

pub fn app(config: Config) -> (Router, Arc<Studio>) {
    let studio = Studio::new(config).unwrap();
    (router(studio.clone()), studio)
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", String::from_utf8_lossy(&self.body)))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|value| value.to_str().unwrap())
    }
}

pub fn request(studio: &Studio, method: Method, uri: &str, body: Option<Value>) -> axum::http::request::Builder {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("127.0.0.1:{}", studio.config.port))
        .header("authorization", format!("Bearer {}", studio.config.token.as_str()));
    if body.is_some() { builder.header("content-type", "application/json") } else { builder }
}

pub async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    Reply { status, headers, body }
}

pub async fn call(app: &Router, studio: &Studio, method: Method, uri: &str, body: Option<Value>) -> Reply {
    let builder = request(studio, method, uri, body.clone());
    let body = body.map_or_else(Body::empty, |json| Body::from(json.to_string()));
    send(app, builder.body(body).unwrap()).await
}

pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{:02X}", b) })
        .collect()
}
