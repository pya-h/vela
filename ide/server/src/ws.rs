//! The WebSocket connecting a terminal view to a session.
//!
//! Browsers can't set headers on a WebSocket, so the client authenticates
//! with its first message, `{"type": "auth", "token": "…"}`. After that:
//!
//! - client to server: binary messages are keystrokes; text messages are
//!   JSON control messages, `{"type": "resize", "cols": 80, "rows": 24}`;
//! - server to client: binary messages are terminal output (recent output
//!   first, on connecting); a text message `{"type": "exit", "code": 0,
//!   "signal": null, "durationMs": 3}` reports the end of the process, and
//!   the server then closes the socket.
//!
//! Close codes: 4401 unauthorized, 4404 no such session, 4409 replaced by a
//! newer connection to the same session.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::Studio;
use crate::session::Event;

/// How long a new connection has to send the token.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest message a client may send (a big paste).
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Keystrokes kept while the process isn't reading its input. Past this,
/// more is dropped, as a terminal drops input its full buffer can't take.
const MAX_PENDING_INPUT: usize = 4 * 1024 * 1024;

pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_NO_SESSION: u16 = 4404;
pub const CLOSE_REPLACED: u16 = 4409;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ClientMessage {
    Auth { token: String },
    Resize { cols: u16, rows: u16 },
}

pub async fn session_socket(State(studio): State<Arc<Studio>>, Path(id): Path<String>, upgrade: WebSocketUpgrade) -> Response {
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| connect(studio, id, socket))
}

async fn connect(studio: Arc<Studio>, id: String, mut socket: WebSocket) {
    let authenticated = match timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            matches!(serde_json::from_str(&text), Ok(ClientMessage::Auth { token }) if studio.config.token.matches(&token))
        }
        _ => false,
    };
    if !authenticated {
        return close(socket, CLOSE_UNAUTHORIZED, "unauthorized").await;
    }
    let (events_tx, mut events) = mpsc::channel(16);
    let Some(session) = studio.sessions.get(&id).filter(|session| session.attach(events_tx)) else {
        return close(socket, CLOSE_NO_SESSION, "no such session").await;
    };

    // Keystrokes the process isn't reading yet. While they wait, output keeps
    // flowing (the process may be writing rather than reading), and messages
    // are still read, so a closed page or a resize is noticed at once.
    let input = session.input();
    let mut pending: Vec<u8> = Vec::new();
    let mut input_closed = false;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(Event::Output(bytes)) => {
                    if socket.send(Message::Binary(bytes)).await.is_err() {
                        return;
                    }
                }
                Some(Event::Exit(exit)) => {
                    let message = json!({ "type": "exit", "code": exit.code, "signal": exit.signal, "durationMs": exit.duration_ms });
                    if socket.send(Message::Text(message.to_string().into())).await.is_ok() {
                        close(socket, 1000, "exited").await;
                    }
                    return;
                }
                // The session ended or another connection replaced this one.
                None => return close(socket, CLOSE_REPLACED, "session closed or opened elsewhere").await,
            },
            permit = input.reserve(), if !pending.is_empty() && !input_closed => match permit {
                Ok(permit) => permit.send(std::mem::take(&mut pending)),
                // The terminal takes no more input; drop what was typed.
                Err(_) => {
                    input_closed = true;
                    pending = Vec::new();
                }
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    if !input_closed && pending.len() + bytes.len() <= MAX_PENDING_INPUT {
                        pending.extend_from_slice(&bytes);
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ClientMessage::Resize { cols, rows }) = serde_json::from_str(&text) {
                        session.resize(cols, rows);
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            },
        }
    }
}

/// How long closing waits for the client to answer the close frame.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Sends a close frame and waits for the client's answer before dropping the
/// connection. Dropping it at once, with messages from the client still
/// unread (a resize, a keystroke), resets the TCP connection, and the browser
/// may then discard what was sent just before, such as the exit message.
async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    if socket.send(Message::Close(Some(CloseFrame { code, reason: Utf8Bytes::from_static(reason) }))).await.is_err() {
        return;
    }
    let _ = timeout(CLOSE_TIMEOUT, async {
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}
