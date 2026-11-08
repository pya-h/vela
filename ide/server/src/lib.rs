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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// Whether the C compiler could be started when the server launched.
    cc_available: bool,
    /// The outstanding launch code (see `launch_code`) and when it was made.
    launch: Mutex<Option<(Token, Instant)>>,
}

/// The browser's launch page (see `Studio::launch_page`), removed when dropped.
pub struct LaunchPage {
    pub path: PathBuf,
    _folder: scratch::ScratchDir,
}

/// How long a launch code can be exchanged for the token.
const LAUNCH_CODE_LIFETIME: Duration = Duration::from_secs(300);

impl Studio {
    /// The workspace must be an existing folder.
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

    /// A new one-time code that the page exchanges for the token
    /// (`POST /api/launch`), replacing any earlier one. The browser is opened
    /// with the code rather than the token, because a program's command line
    /// is visible to other users of the machine; a code that has been used,
    /// or is a few minutes old, is worthless.
    pub fn launch_code(&self) -> std::io::Result<String> {
        let code = Token::generate()?;
        let text = code.as_str().to_string();
        *self
            .launch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((code, Instant::now()));
        Ok(text)
    }

    /// A page that sends the browser to Vela Studio with a new launch code,
    /// written to a file only this user can read. Opening the browser with this
    /// file keeps the code off the browser's command line, which other users
    /// can read and could use before the browser does. The file is removed
    /// when the returned value is dropped.
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

    /// Takes the launch code if `candidate` is it and it hasn't expired.
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

    /// The workspace folder, with symlinks resolved.
    pub fn workspace_root(&self) -> &std::path::Path {
        self.workspace.root()
    }
}

/// Whether `cc --version` runs, for the IDE's "C compiler missing" banner.
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

/// The session WebSocket takes the token as its first message (see `ws`);
/// everything else is behind the token check.
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
        // A file's JSON may be several times its size (escapes); the file
        // size itself is checked separately.
        .layer(DefaultBodyLimit::max(8 * MAX_FILE_BYTES))
        .with_state(studio)
}
