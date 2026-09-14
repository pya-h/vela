//! The JSON API. Every handler here runs behind the token check (see
//! `security`); errors are `{"error": "…"}` with a fitting status.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use portable_pty::{CommandBuilder, PtySize};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Studio;
use crate::compile::{self, Program};
use crate::session::{SessionError, SessionKind};
use crate::workspace::{EntryKind, FsError};

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Value,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError { status, body: json!({ "error": message.into() }) }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<FsError> for ApiError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::InvalidPath(message) => ApiError::new(StatusCode::BAD_REQUEST, message),
            FsError::Outside => ApiError::new(StatusCode::FORBIDDEN, "the path leads outside the workspace"),
            FsError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "no such file or folder"),
            FsError::AlreadyExists => ApiError::new(StatusCode::CONFLICT, "a file or folder with that name already exists"),
            FsError::NotAFile => ApiError::new(StatusCode::BAD_REQUEST, "not a file"),
            FsError::TooLarge => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("files larger than {} MiB can't be opened", crate::MAX_FILE_BYTES / (1024 * 1024)),
            ),
            FsError::NotText => ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "the file isn't UTF-8 text"),
            FsError::Changed { hash } => ApiError {
                status: StatusCode::CONFLICT,
                body: json!({ "error": "the file changed on disk", "hash": hash }),
            },
            FsError::Io(error) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        FsError::from(error).into()
    }
}

type ApiResult = Result<Response, ApiError>;

/// Runs blocking file-system or compiler work off the async threads.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

pub async fn info(State(studio): State<Arc<Studio>>) -> ApiResult {
    Ok(Json(json!({
        "name": "Vela Studio",
        "version": env!("CARGO_PKG_VERSION"),
        "workspace": { "name": studio.workspace.name(), "root": studio.workspace.root() },
        "cc": { "command": studio.config.cc, "available": studio.cc_available },
        "platform": std::env::consts::OS,
    }))
    .into_response())
}

pub async fn files(State(studio): State<Arc<Studio>>) -> ApiResult {
    let tree = blocking(move || studio.workspace.tree()).await?;
    Ok(Json(tree).into_response())
}

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
}

pub async fn read_file(State(studio): State<Arc<Studio>>, Query(query): Query<PathQuery>) -> ApiResult {
    let path = query.path;
    let file = blocking({
        let path = path.clone();
        move || studio.workspace.read(&path)
    })
    .await??;
    Ok(Json(json!({ "path": path, "content": file.content, "hash": file.hash })).into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteRequest {
    path: String,
    content: String,
    /// The hash from when the editor read the file; the write fails if the
    /// file has changed since.
    base_hash: Option<String>,
}

pub async fn write_file(State(studio): State<Arc<Studio>>, Json(request): Json<WriteRequest>) -> ApiResult {
    let hash = blocking(move || studio.workspace.write(&request.path, &request.content, request.base_hash.as_deref())).await??;
    Ok(Json(json!({ "hash": hash })).into_response())
}

#[derive(Deserialize)]
pub struct CreateRequest {
    path: String,
    kind: EntryKind,
}

pub async fn create_file(State(studio): State<Arc<Studio>>, Json(request): Json<CreateRequest>) -> ApiResult {
    let path = blocking(move || studio.workspace.create(&request.path, request.kind)).await??;
    Ok((StatusCode::CREATED, Json(json!({ "path": path }))).into_response())
}

#[derive(Deserialize)]
pub struct RenameRequest {
    from: String,
    to: String,
}

pub async fn rename_file(State(studio): State<Arc<Studio>>, Json(request): Json<RenameRequest>) -> ApiResult {
    let path = blocking(move || studio.workspace.rename(&request.from, &request.to)).await??;
    Ok(Json(json!({ "path": path })).into_response())
}

pub async fn delete_file(State(studio): State<Arc<Studio>>, Json(request): Json<PathQuery>) -> ApiResult {
    blocking(move || studio.workspace.delete(&request.path)).await??;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct BuildRequest {
    path: String,
}

/// Builds a file. A failed build is a normal answer: `{"ok": false,
/// "error": {kind, message, line, col, rendered}}`.
pub async fn build(State(studio): State<Arc<Studio>>, Json(request): Json<BuildRequest>) -> ApiResult {
    let built = blocking(move || compile::build(&studio.workspace, &request.path, &studio.config.cc)).await??;
    Ok(match built {
        // The program is removed with its folder here; only running keeps it.
        Ok(_program) => Json(json!({ "ok": true })),
        Err(failure) => Json(json!({ "ok": false, "error": failure })),
    }
    .into_response())
}

#[derive(Deserialize)]
pub struct TerminalSize {
    cols: Option<u16>,
    rows: Option<u16>,
}

impl TerminalSize {
    fn pty_size(&self) -> PtySize {
        PtySize {
            cols: self.cols.unwrap_or(80).clamp(1, 1000),
            rows: self.rows.unwrap_or(24).clamp(1, 1000),
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Deserialize)]
pub struct RunRequest {
    path: String,
    #[serde(flatten)]
    size: TerminalSize,
}

/// Builds a file and runs it in a new session (stopping the previous run):
/// `{"ok": true, "session": id}`, or the build failure as from `build`.
pub async fn run(State(studio): State<Arc<Studio>>, Json(request): Json<RunRequest>) -> ApiResult {
    let built = blocking({
        let studio = studio.clone();
        let path = request.path.clone();
        move || compile::build(&studio.workspace, &path, &studio.config.cc)
    })
    .await??;
    let Program { executable, folder, scratch } = match built {
        Ok(program) => program,
        Err(failure) => return Ok(Json(json!({ "ok": false, "error": failure })).into_response()),
    };
    let mut command = CommandBuilder::new(executable);
    command.cwd(folder);
    let id = studio.sessions.spawn(SessionKind::Run, command, request.size.pty_size(), Some(scratch)).map_err(session_error)?;
    Ok(Json(json!({ "ok": true, "session": id })).into_response())
}

/// Opens a shell in the workspace folder: `{"session": id}`.
pub async fn terminal(State(studio): State<Arc<Studio>>, Json(size): Json<TerminalSize>) -> ApiResult {
    let mut command = match &studio.config.shell {
        Some(shell) => CommandBuilder::new(shell),
        None => CommandBuilder::new_default_prog(),
    };
    command.cwd(studio.workspace.root());
    let id = studio.sessions.spawn(SessionKind::Terminal, command, size.pty_size(), None).map_err(session_error)?;
    Ok((StatusCode::CREATED, Json(json!({ "session": id }))).into_response())
}

pub async fn stop(State(studio): State<Arc<Studio>>, Path(id): Path<String>) -> ApiResult {
    let session = studio.sessions.get(&id).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such session"))?;
    session.stop();
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn session_error(error: SessionError) -> ApiError {
    match error {
        SessionError::TooManyTerminals => ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many terminals are open"),
        SessionError::Spawn(message) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("couldn't start the process: {}", message)),
    }
}
