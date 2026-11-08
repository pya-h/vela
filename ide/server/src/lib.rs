//! Loopback-only IDE server: per-launch token, Host/Origin checks (`security`), and workspace confinement (`workspace`).

mod api;
mod compile;
mod scratch;
mod security;
mod session;
mod web;
mod workspace;
mod ws;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

pub use security::Token;
pub use session::{SessionKind, Sessions};
pub use workspace::MAX_FILE_BYTES;

#[derive(Debug, Clone)]
pub struct Config {
    pub workspace: PathBuf,
    /// Loopback port matched against `Host` and `Origin`.
    pub port: u16,
    pub token: Token,
    pub cc: String,
    pub shell: Option<String>,
    /// Idle duration without a WebSocket before a session is stopped and removed.
    pub idle_timeout: Duration,
    pub max_terminals: usize,
}

impl Config {
    pub fn new(workspace: PathBuf, port: u16, token: Token) -> Config {
        Config {
            workspace,
            port,
            token,
            cc: vela::driver::c_compiler_from_env(),
            shell: None,
            idle_timeout: Duration::from_secs(30),
            max_terminals: 8,
        }
    }
}

pub struct Studio {
    pub config: Config,
    workspace: workspace::Workspace,
    pub sessions: Arc<Sessions>,
    cc_available: bool,
    launch: Mutex<Option<(Token, Instant)>>,
}

pub struct LaunchPage {
    pub path: PathBuf,
    _folder: scratch::ScratchDir,
}

const LAUNCH_CODE_LIFETIME: Duration = Duration::from_secs(300);

impl Studio {
    pub fn new(config: Config) -> std::io::Result<Arc<Studio>> {
        let workspace = workspace::Workspace::open(&config.workspace)?;
        let cc_available = c_compiler_starts(&config.cc);
        let sessions = Sessions::new(config.idle_timeout, config.max_terminals);
        Ok(Arc::new(Studio {
            config,
            workspace,
            sessions,
            cc_available,
            launch: Mutex::new(None),
        }))
    }

    /// One-time code exchanged at `POST /api/launch` so the token stays off process command lines.
    pub fn launch_code(&self) -> std::io::Result<String> {
        let code = Token::generate()?;
        let text = code.as_str().to_string();
        *self
            .launch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((code, Instant::now()));
        Ok(text)
    }

    /// Mode-0600 redirect page so even the one-time code stays off the browser's command line.
    pub fn launch_page(&self) -> std::io::Result<LaunchPage> {
        let folder = scratch::ScratchDir::create()?;
        let url = format!(
            "http://127.0.0.1:{}/#launch={}",
            self.config.port,
            self.launch_code()?
        );
        let path = folder.path().join("open-vela-studio.html");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let html = format!(
            "<!DOCTYPE html>\n<meta charset=\"utf-8\">\n<title>Vela Studio</title>\n<meta http-equiv=\"refresh\" content=\"0; url={0}\">\n<p><a href=\"{0}\">Open Vela Studio</a></p>\n",
            url
        );
        std::io::Write::write_all(&mut options.open(&path)?, html.as_bytes())?;
        Ok(LaunchPage {
            path,
            _folder: folder,
        })
    }

    fn redeem_launch_code(&self, candidate: &str) -> bool {
        let mut launch = self
            .launch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let valid = launch.as_ref().is_some_and(|(code, made)| {
            code.matches(candidate) && made.elapsed() < LAUNCH_CODE_LIFETIME
        });
        if valid {
            *launch = None;
        }
        valid
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        self.workspace.root()
    }
}

fn c_compiler_starts(cc: &str) -> bool {
    let mut words = cc.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    Command::new(program)
        .args(words)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// WebSockets authenticate via their first frame; every other API route requires a bearer token.
pub fn router(studio: Arc<Studio>) -> Router {
    let api = Router::new()
        .route("/api/info", get(api::info))
        .route("/api/files", get(api::files))
        .route("/api/file", get(api::read_file).put(api::write_file))
        .route("/api/file/new", post(api::create_file))
        .route("/api/file/rename", post(api::rename_file))
        .route("/api/file/delete", post(api::delete_file))
        .route("/api/build", post(api::build))
        .route("/api/run", post(api::run))
        .route("/api/terminal", post(api::terminal))
        .route("/api/session/{id}/stop", post(api::stop))
        .route_layer(middleware::from_fn_with_state(
            studio.clone(),
            security::require_token,
        ));
    Router::new()
        .merge(api)
        .route("/api/launch", post(api::launch))
        .route("/api/session/{id}/ws", get(ws::session_socket))
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(
            studio.clone(),
            security::guard,
        ))
        // JSON escaping can multiply size; file bytes are checked separately.
        .layer(DefaultBodyLimit::max(8 * MAX_FILE_BYTES))
        .with_state(studio)
}
