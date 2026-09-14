//! Vela Studio's server: the parts of the IDE a browser page can't do by
//! itself. It serves the frontend, reads and writes files in one workspace
//! folder, builds programs with `vela::driver` (exactly as the `vela` command
//! does), and runs programs and shells in pseudo-terminals streamed over
//! WebSockets. Language analysis runs in the page (see `ide/wasm`).
//!
//! The server can run code, so it only answers this user's IDE page: it
//! listens on loopback, requires the per-launch token, and rejects other
//! hosts and origins (see `security`). Every path is confined to the
//! workspace (see `workspace`).

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
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

pub use security::Token;
pub use session::{SessionKind, Sessions};
pub use workspace::MAX_FILE_BYTES;

/// Settings for one server launch.
#[derive(Debug, Clone)]
pub struct Config {
    /// The workspace folder; every file path is relative to it.
    pub workspace: PathBuf,
    /// The loopback port the server listens on. Requests must name it in
    /// their `Host` (and `Origin`) header.
    pub port: u16,
    pub token: Token,
    /// The C compiler command, as `vela` reads it from `$CC`.
    pub cc: String,
    /// The shell for terminals; `None` runs the user's default shell.
    pub shell: Option<String>,
    /// How long a session may go without a connected WebSocket before it is
    /// stopped and removed.
    pub idle_timeout: Duration,
    /// How many terminals may be open at once.
    pub max_terminals: usize,
}

impl Config {
    /// The defaults for a launch on `port` in `workspace`.
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

/// Everything the request handlers share.
pub struct Studio {
    pub config: Config,
    workspace: workspace::Workspace,
    pub sessions: Arc<Sessions>,
    /// Whether the C compiler could be started when the server launched.
    cc_available: bool,
}

impl Studio {
    /// Opens the workspace (which must be an existing folder) and checks the
    /// C compiler.
    pub fn new(config: Config) -> std::io::Result<Arc<Studio>> {
        let workspace = workspace::Workspace::open(&config.workspace)?;
        let cc_available = c_compiler_starts(&config.cc);
        let sessions = Sessions::new(config.idle_timeout, config.max_terminals);
        Ok(Arc::new(Studio { config, workspace, sessions, cc_available }))
    }

    /// The workspace folder, with symlinks resolved.
    pub fn workspace_root(&self) -> &std::path::Path {
        self.workspace.root()
    }
}

/// Whether `cc --version` runs, for the IDE's "C compiler missing" banner.
fn c_compiler_starts(cc: &str) -> bool {
    let mut words = cc.split_whitespace();
    let Some(program) = words.next() else { return false };
    Command::new(program)
        .args(words)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The whole application: the JSON API (token required), the session
/// WebSocket (token sent as its first message), and the frontend, all behind
/// the host and origin checks.
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
        .route_layer(middleware::from_fn_with_state(studio.clone(), security::require_token));
    Router::new()
        .merge(api)
        .route("/api/session/{id}/ws", get(ws::session_socket))
        .fallback(web::serve)
        .layer(middleware::from_fn_with_state(studio.clone(), security::guard))
        // A file's JSON may be several times its size (escapes); the file
        // size itself is checked separately.
        .layer(DefaultBodyLimit::max(8 * MAX_FILE_BYTES))
        .with_state(studio)
}
