use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
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
        ApiError {
            status,
            body: json!({ "error": message.into() }),
        }
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
            FsError::Outside => ApiError::new(
                StatusCode::FORBIDDEN,
                "the path leads outside the workspace",
            ),
            FsError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "no such file or folder"),
            FsError::AlreadyExists => ApiError::new(
                StatusCode::CONFLICT,
                "a file or folder with that name already exists",
            ),
            FsError::NotAFile => ApiError::new(StatusCode::BAD_REQUEST, "not a file"),
            FsError::TooLarge => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "files larger than {} MiB can't be opened",
                    crate::MAX_FILE_BYTES / (1024 * 1024)
                ),
            ),
            FsError::NotText => ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "the file isn't UTF-8 text",
            ),
            FsError::Changed { hash } => ApiError {
                status: StatusCode::CONFLICT,
                body: json!({ "error": "the file changed on disk", "hash": hash }),
            },
            FsError::ReadOnly => ApiError::new(StatusCode::FORBIDDEN, "the file is read-only"),
            FsError::Denied => ApiError::new(StatusCode::FORBIDDEN, "permission denied"),
            FsError::Io(error) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            }
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(error: std::io::Error) -> Self {
        FsError::from(error).into()
    }
}

type ApiResult = Result<Response, ApiError>;

/// `Json` extractor returning `{"error": …}` on rejection.
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(ApiError::new(rejection.status(), rejection.body_text())),
        }
    }
}

/// `Query` extractor returning `{"error": …}` on rejection.
pub struct ApiQuery<T>(pub T);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(ApiQuery(value)),
            Err(rejection) => Err(ApiError::new(rejection.status(), rejection.body_text())),
        }
    }
}

// Keep fs and compiler work off the async executor.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[derive(Deserialize)]
pub struct LaunchRequest {
    code: String,
}

/// Only endpoint without a bearer token; exchanges the one-time launch code.
pub async fn launch(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<LaunchRequest>,
) -> ApiResult {
    if !studio.redeem_launch_code(&request.code) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "this link has already been used or has expired",
        ));
    }
    let mut response = Json(json!({ "token": studio.config.token.as_str() })).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
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

pub async fn read_file(
    State(studio): State<Arc<Studio>>,
    ApiQuery(query): ApiQuery<PathQuery>,
) -> ApiResult {
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
    /// Hash from the read; fails with `Changed` if disk was modified in between.
    base_hash: Option<String>,
}

pub async fn write_file(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<WriteRequest>,
) -> ApiResult {
    let hash = blocking(move || {
        studio.workspace.write(
            &request.path,
            &request.content,
            request.base_hash.as_deref(),
        )
    })
    .await??;
    Ok(Json(json!({ "hash": hash })).into_response())
}

#[derive(Deserialize)]
pub struct CreateRequest {
    path: String,
    kind: EntryKind,
}

pub async fn create_file(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<CreateRequest>,
) -> ApiResult {
    let path = blocking(move || studio.workspace.create(&request.path, request.kind)).await??;
    Ok((StatusCode::CREATED, Json(json!({ "path": path }))).into_response())
}

#[derive(Deserialize)]
pub struct RenameRequest {
    from: String,
    to: String,
}

pub async fn rename_file(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<RenameRequest>,
) -> ApiResult {
    let path = blocking(move || studio.workspace.rename(&request.from, &request.to)).await??;
    Ok(Json(json!({ "path": path })).into_response())
}

pub async fn delete_file(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<PathQuery>,
) -> ApiResult {
    blocking(move || studio.workspace.delete(&request.path)).await??;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct BuildRequest {
    path: String,
}

pub async fn build(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<BuildRequest>,
) -> ApiResult {
    let built =
        blocking(move || compile::build(&studio.workspace, &request.path, &studio.config.cc))
            .await??;
    Ok(match built {
        // `ScratchDir` cleans up the executable on drop; only `run` keeps it.
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

pub async fn run(
    State(studio): State<Arc<Studio>>,
    ApiJson(request): ApiJson<RunRequest>,
) -> ApiResult {
    // Stop any running program even if the new build fails (the UI only keeps one active run).
    studio.sessions.stop_run();
    let built = blocking({
        let studio = studio.clone();
        let path = request.path.clone();
        move || compile::build(&studio.workspace, &path, &studio.config.cc)
    })
    .await??;
    let Program {
        executable,
        folder,
        scratch,
    } = match built {
        Ok(program) => program,
        Err(failure) => return Ok(Json(json!({ "ok": false, "error": failure })).into_response()),
    };
    let mut command = CommandBuilder::new(executable);
    command.cwd(folder);
    let id = studio
        .sessions
        .spawn(
            SessionKind::Run,
            command,
            request.size.pty_size(),
            Some(scratch),
        )
        .map_err(session_error)?;
    Ok(Json(json!({ "ok": true, "session": id })).into_response())
}

pub async fn terminal(
    State(studio): State<Arc<Studio>>,
    ApiJson(size): ApiJson<TerminalSize>,
) -> ApiResult {
    let mut command = match &studio.config.shell {
        Some(shell) => CommandBuilder::new(shell),
        None => CommandBuilder::new_default_prog(),
    };
    command.cwd(studio.workspace.root());
    let id = studio
        .sessions
        .spawn(SessionKind::Terminal, command, size.pty_size(), None)
        .map_err(session_error)?;
    Ok((StatusCode::CREATED, Json(json!({ "session": id }))).into_response())
}

pub async fn stop(State(studio): State<Arc<Studio>>, Path(id): Path<String>) -> ApiResult {
    let session = studio
        .sessions
        .get(&id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such session"))?;
    session.stop();
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn session_error(error: SessionError) -> ApiError {
    match error {
        SessionError::TooManyTerminals => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too many terminals are open")
        }
        SessionError::Closing => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Vela Studio is stopping")
        }
        SessionError::Spawn(message) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't start the process: {}", message),
        ),
    }
}
