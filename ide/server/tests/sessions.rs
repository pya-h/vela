//! Running programs and terminals, through a real server on 127.0.0.1 and
//! its WebSocket protocol (see `src/ws.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{Method, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use vela_studio::{Config, Studio};

use common::*;

const LOOP_FOREVER: &str = "task main()\n    hold n = 0\n    loop true\n        n = n + 1\n    fi\nfi\n";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server {
    app: Router,
    studio: Arc<Studio>,
    port: u16,
    dir: TempDir,
}

/// Starts a server on a free loopback port for a workspace holding `files`.
async fn server(test: &str, files: &[(&str, &str)], configure: impl FnOnce(&mut Config)) -> Server {
    let dir = TempDir::new(test);
    for (path, content) in files {
        dir.write(path, content);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = config(&dir.workspace(), port);
    config.idle_timeout = Duration::from_secs(5);
    configure(&mut config);
    let (app, studio) = app(config);
    tokio::spawn(axum::serve(listener, app.clone()).into_future());
    Server { app, studio, port, dir }
}

impl Server {
    async fn post(&self, uri: &str, body: Value) -> common::Reply {
        call(&self.app, &self.studio, Method::POST, uri, Some(body)).await
    }

    async fn run(&self, path: &str) -> String {
        let reply = self.post("/api/run", json!({ "path": path })).await;
        assert_eq!(reply.status, StatusCode::OK);
        let json = reply.json();
        assert_eq!(json["ok"], true, "{}", json);
        json["session"].as_str().unwrap().to_string()
    }

    async fn terminal(&self) -> String {
        let reply = self.post("/api/terminal", json!({ "cols": 80, "rows": 24 })).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{:?}", String::from_utf8_lossy(&reply.body));
        reply.json()["session"].as_str().unwrap().to_string()
    }

    /// Opens a session's WebSocket from this server's page, without authenticating.
    async fn connect_unauthenticated(&self, id: &str, origin: &str) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
        let mut request = format!("ws://127.0.0.1:{}/api/session/{}/ws", self.port, id).into_client_request().unwrap();
        request.headers_mut().insert("origin", origin.parse().unwrap());
        tokio_tungstenite::connect_async(request).await.map(|(socket, _)| socket)
    }

    async fn connect(&self, id: &str) -> Socket {
        let mut socket = self.connect_unauthenticated(id, &format!("http://127.0.0.1:{}", self.port)).await.unwrap();
        let auth = json!({ "type": "auth", "token": self.studio.config.token.as_str() });
        socket.send(Message::text(auth.to_string())).await.unwrap();
        socket
    }

    /// Waits until `condition` holds, failing after a few seconds.
    async fn eventually(&self, what: &str, condition: impl Fn(&Studio) -> bool) {
        for _ in 0..200 {
            if condition(&self.studio) {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {}", what);
    }
}

/// What a socket received until the server closed it or `until` matched the output.
#[derive(Debug, Default)]
struct Received {
    output: String,
    exit: Option<Value>,
    close: Option<u16>,
}

async fn receive(socket: &mut Socket, until: impl Fn(&str) -> bool) -> Received {
    let mut received = Received::default();
    let reading = async {
        while let Some(message) = socket.next().await {
            match message {
                Ok(Message::Binary(bytes)) => {
                    received.output.push_str(&String::from_utf8_lossy(&bytes));
                    if until(&received.output) {
                        return;
                    }
                }
                Ok(Message::Text(text)) => received.exit = Some(serde_json::from_str(&text).unwrap()),
                Ok(Message::Close(frame)) => {
                    received.close = frame.map(|f| u16::from(f.code));
                    return;
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    };
    if timeout(Duration::from_secs(10), reading).await.is_err() {
        panic!("timed out; received so far: {:?}", received);
    }
    received
}

fn never(_: &str) -> bool {
    false
}

#[tokio::test]
async fn a_run_streams_output_then_its_exit() {
    let server = server("run", &[("hello.vela", HELLO)], |_| {}).await;
    let id = server.run("hello.vela").await;
    let mut socket = server.connect(&id).await;
    let received = receive(&mut socket, never).await;
    // The terminal turns newlines into CR LF.
    assert_eq!(received.output, "Hello, Vela!\r\n");
    let exit = received.exit.unwrap();
    assert_eq!((exit["type"].as_str(), exit["code"].as_u64(), &exit["signal"]), (Some("exit"), Some(0), &Value::Null));
    assert!(exit["durationMs"].is_u64());
    assert_eq!(received.close, Some(1000));
}

#[tokio::test]
async fn a_late_connection_replays_the_output() {
    let server = server("replay", &[("hello.vela", HELLO)], |_| {}).await;
    let id = server.run("hello.vela").await;
    let session = server.studio.sessions.get(&id).unwrap();
    server.eventually("the program to exit", |_| !session.is_alive()).await;
    sleep(Duration::from_millis(400)).await;
    let mut socket = server.connect(&id).await;
    let received = receive(&mut socket, never).await;
    assert_eq!(received.output, "Hello, Vela!\r\n");
    assert_eq!(received.exit.unwrap()["code"], 0);
}

#[tokio::test]
async fn a_failed_build_starts_nothing() {
    let server = server("run-fails", &[("bad.vela", "task main()\n    show(missing)\nfi\n")], |_| {}).await;
    let reply = server.post("/api/run", json!({ "path": "bad.vela" })).await.json();
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"]["line"], 2);
    assert_eq!(server.studio.sessions.count(vela_studio::SessionKind::Run), 0);
}

#[tokio::test]
async fn stop_ends_a_running_program_and_a_new_run_replaces_it() {
    let server = server("stop", &[("forever.vela", LOOP_FOREVER), ("hello.vela", HELLO)], |_| {}).await;
    let id = server.run("forever.vela").await;
    let mut socket = server.connect(&id).await;
    sleep(Duration::from_millis(200)).await;
    assert!(server.studio.sessions.get(&id).unwrap().is_alive());
    let stop = call(&server.app, &server.studio, Method::POST, &format!("/api/session/{}/stop", id), None).await;
    assert_eq!(stop.status, StatusCode::NO_CONTENT);
    let exit = receive(&mut socket, never).await.exit.unwrap();
    assert!(exit["signal"].is_string(), "{}", exit);

    // Starting a run stops the one before.
    let first = server.run("forever.vela").await;
    let mut first_socket = server.connect(&first).await;
    sleep(Duration::from_millis(200)).await;
    let second = server.run("hello.vela").await;
    assert!(receive(&mut first_socket, never).await.exit.unwrap()["signal"].is_string());
    let mut second_socket = server.connect(&second).await;
    assert_eq!(receive(&mut second_socket, never).await.output, "Hello, Vela!\r\n");

    let missing = call(&server.app, &server.studio, Method::POST, "/api/session/nope/stop", None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_terminal_runs_a_shell_in_the_workspace() {
    let server = server("terminal", &[("hello.vela", HELLO)], |_| {}).await;
    let id = server.terminal().await;
    let mut socket = server.connect(&id).await;
    socket.send(Message::text(json!({ "type": "resize", "cols": 101, "rows": 33 }).to_string())).await.unwrap();
    sleep(Duration::from_millis(100)).await;
    socket.send(Message::binary(b"stty size; ls; echo marker-$((6 * 7))\n".to_vec())).await.unwrap();
    let received = receive(&mut socket, |output| output.contains("marker-42\r\n")).await;
    assert!(received.output.contains("33 101"), "{:?}", received.output);
    assert!(received.output.contains("hello.vela"), "{:?}", received.output);

    socket.send(Message::binary(b"exit 3\n".to_vec())).await.unwrap();
    let received = receive(&mut socket, never).await;
    assert_eq!(received.exit.unwrap()["code"], 3);
}

#[tokio::test]
async fn output_keeps_flowing_while_a_big_paste_waits() {
    let server = server("paste", &[], |_| {}).await;
    let id = server.terminal().await;
    let mut socket = server.connect(&id).await;
    // A program that writes forever and never reads its input, in raw mode
    // (as full-screen programs use), where input waits instead of being dropped.
    socket.send(Message::binary(b"stty raw -echo; exec yes\n".to_vec())).await.unwrap();
    // Raw mode also stops turning newlines into CR LF.
    receive(&mut socket, |output| output.contains("y\ny\n")).await;

    let (mut sink, mut stream) = socket.split();
    // Far more input than the terminal and the server buffer (32 MiB).
    let paste = tokio::spawn(async move {
        for _ in 0..512 {
            if sink.send(Message::binary(vec![b'x'; 64 * 1024])).await.is_err() {
                return;
            }
        }
    });
    sleep(Duration::from_millis(300)).await;
    // While the paste is stuck, the program's output still arrives: more
    // than every buffer on the way holds.
    let mut bytes = 0;
    let reading = async {
        while let Some(Ok(message)) = stream.next().await {
            if let Message::Binary(chunk) = message {
                bytes += chunk.len();
                if bytes > 32 * 1024 * 1024 {
                    return;
                }
            }
        }
    };
    let flowed = timeout(Duration::from_secs(10), reading).await.is_ok();
    paste.abort();
    assert!(flowed, "output stopped after {} bytes", bytes);
}

#[tokio::test]
async fn websockets_require_the_token_and_a_known_session() {
    let server = server("ws-auth", &[], |_| {}).await;
    let id = server.terminal().await;
    let origin = format!("http://127.0.0.1:{}", server.port);

    // A wrong token, or anything but the auth message first, is refused.
    for first in [json!({ "type": "auth", "token": "0".repeat(64) }).to_string(), json!({ "type": "resize", "cols": 1, "rows": 1 }).to_string()] {
        let mut socket = server.connect_unauthenticated(&id, &origin).await.unwrap();
        socket.send(Message::text(first)).await.unwrap();
        assert_eq!(receive(&mut socket, never).await.close, Some(4401));
    }
    // An unknown session.
    let mut socket = server.connect("0123").await;
    assert_eq!(receive(&mut socket, never).await.close, Some(4404));
    // Another website's page can't even open the socket.
    let refused = server.connect_unauthenticated(&id, "http://evil.example").await;
    assert!(matches!(refused, Err(tokio_tungstenite::tungstenite::Error::Http(ref response)) if response.status() == 403), "{:?}", refused.map(|_| ()));

    // A second connection to a session replaces the first.
    let mut first = server.connect(&id).await;
    sleep(Duration::from_millis(100)).await;
    let _second = server.connect(&id).await;
    assert_eq!(receive(&mut first, never).await.close, Some(4409));
}

#[tokio::test]
async fn idle_sessions_are_stopped_and_removed() {
    let server = server("idle", &[], |config| config.idle_timeout = Duration::from_millis(300)).await;
    let id = server.terminal().await;
    let session = server.studio.sessions.get(&id).unwrap();
    // Connected sessions stay open, even when quiet.
    let socket = server.connect(&id).await;
    sleep(Duration::from_millis(700)).await;
    assert!(session.is_alive());
    drop(socket);
    server.eventually("the idle terminal to end", |studio| studio.sessions.get(&id).is_none()).await;
    assert!(!session.is_alive());

    // The same for a session nobody ever connects to.
    let id = server.terminal().await;
    let session = server.studio.sessions.get(&id).unwrap();
    server.eventually("the unconnected terminal to end", |studio| studio.sessions.get(&id).is_none()).await;
    assert!(!session.is_alive());
}

#[tokio::test]
async fn terminals_are_limited() {
    let server = server("limit", &[], |config| config.max_terminals = 1).await;
    let _first = server.terminal().await;
    let second = server.post("/api/terminal", json!({})).await;
    assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS);
    // A run doesn't count as a terminal.
    server.dir.write("hello.vela", HELLO);
    server.run("hello.vela").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_terminal_limit_holds_under_concurrent_requests() {
    let server = Arc::new(server("limit-concurrent", &[], |config| config.max_terminals = 2).await);
    let requests: Vec<_> = (0..16)
        .map(|_| {
            let server = server.clone();
            tokio::spawn(async move { server.post("/api/terminal", json!({ "cols": 80, "rows": 24 })).await.status })
        })
        .collect();
    let mut created = 0;
    for request in requests {
        let status = request.await.unwrap();
        assert!(matches!(status, StatusCode::CREATED | StatusCode::TOO_MANY_REQUESTS), "{}", status);
        created += usize::from(status == StatusCode::CREATED);
    }
    assert_eq!(created, 2);
    assert_eq!(server.studio.sessions.count(vela_studio::SessionKind::Terminal), 2);
}

#[tokio::test]
async fn a_run_whose_build_fails_still_stops_the_running_program() {
    let server = server("run-then-fail", &[("forever.vela", LOOP_FOREVER), ("bad.vela", "task main()\n    show(missing)\nfi\n")], |_| {}).await;
    let id = server.run("forever.vela").await;
    let mut socket = server.connect(&id).await;
    sleep(Duration::from_millis(200)).await;
    let reply = server.post("/api/run", json!({ "path": "bad.vela" })).await.json();
    assert_eq!(reply["ok"], false);
    assert!(receive(&mut socket, never).await.exit.unwrap()["signal"].is_string());
}

/// Whether a process runs `sleep SECONDS`.
#[cfg(target_os = "linux")]
fn sleeping(seconds: &str) -> bool {
    std::fs::read_dir("/proc").unwrap().flatten().any(|entry| {
        std::fs::read(entry.path().join("cmdline")).is_ok_and(|cmdline| cmdline == format!("sleep\0{}\0", seconds).as_bytes())
    })
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_background_job_is_hung_up_when_its_shell_exits() {
    // `sh` doesn't hang up its own jobs when it exits, as zsh does.
    if !std::path::Path::new("/bin/sh").exists() {
        return;
    }
    let server = server("background-job", &[], |config| config.shell = Some("/bin/sh".to_string())).await;
    let id = server.terminal().await;
    let mut socket = server.connect(&id).await;
    socket.send(Message::binary(b"sleep 9973 & sleep 0.3; exit\n".to_vec())).await.unwrap();
    assert!(receive(&mut socket, never).await.exit.is_some());
    for _ in 0..50 {
        if !sleeping("9973") {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("the background job is still running");
}

#[tokio::test]
async fn a_terminal_that_exited_frees_its_place() {
    let server = server("limit-exit", &[], |config| config.max_terminals = 1).await;
    let first = server.terminal().await;
    let mut socket = server.connect(&first).await;
    socket.send(Message::binary(b"exit\n".to_vec())).await.unwrap();
    assert!(receive(&mut socket, never).await.exit.is_some());
    // Its session lingers for the page, but a new terminal can start.
    server.terminal().await;
}

#[tokio::test]
async fn nothing_starts_once_shutdown_has_begun() {
    let server = server("closing", &[], |_| {}).await;
    server.studio.sessions.shutdown().await;
    let refused = server.post("/api/terminal", json!({})).await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn concurrent_runs_leave_one_program_running() {
    let server = server("concurrent-runs", &[("spin.vela", LOOP_FOREVER)], |_| {}).await;
    let runs = (0..4).map(|_| server.post("/api/run", json!({ "path": "spin.vela" })));
    let replies = futures_util::future::join_all(runs).await;
    assert!(replies.iter().all(|reply| reply.json()["ok"] == true));
    server
        .eventually("all but one run to stop", |studio| {
            replies
                .iter()
                .filter_map(|reply| studio.sessions.get(reply.json()["session"].as_str().unwrap()))
                .filter(|session| session.is_alive())
                .count()
                == 1
        })
        .await;
}

#[tokio::test]
async fn a_client_that_stops_reading_doesnt_block_others() {
    let server = server("stalled", &[], |_| {}).await;
    let id = server.terminal().await;
    // A client that never reads, while the shell writes without end.
    let mut stalled = server.connect(&id).await;
    stalled.send(Message::binary(b"yes\n".to_vec())).await.unwrap();
    sleep(Duration::from_millis(500)).await;
    // A new connection still gets the output, and replaces the stalled one.
    let mut second = server.connect(&id).await;
    let received = receive(&mut second, |output| output.contains("y\r\ny")).await;
    assert!(received.output.contains('y'));
    drop(stalled);
}

#[tokio::test]
async fn a_closed_page_is_noticed_while_typed_input_waits() {
    let server = server("pending-close", &[], |config| config.idle_timeout = Duration::from_millis(300)).await;
    let id = server.terminal().await;
    let mut socket = server.connect(&id).await;
    // A program that reads nothing, in raw mode, where input waits.
    socket.send(Message::binary(b"stty raw -echo; exec sleep 1000\n".to_vec())).await.unwrap();
    sleep(Duration::from_millis(300)).await;
    for _ in 0..64 {
        socket.send(Message::binary(vec![b'x'; 64 * 1024])).await.unwrap();
    }
    drop(socket);
    server.eventually("the session to end after its page closed", |studio| studio.sessions.get(&id).is_none()).await;
}

#[tokio::test]
async fn shutdown_stops_every_session() {
    let server = server("shutdown", &[("shutdown_forever.vela", LOOP_FOREVER)], |_| {}).await;
    let run = server.run("shutdown_forever.vela").await;
    let terminal = server.terminal().await;
    let sessions = [server.studio.sessions.get(&run).unwrap(), server.studio.sessions.get(&terminal).unwrap()];
    let built = built_programs("shutdown_forever");
    assert_eq!(built.len(), 1, "{:?}", built);
    server.studio.sessions.shutdown().await;
    assert!(sessions.iter().all(|session| !session.is_alive()));
    // The sessions are gone, and so is the built program's folder.
    assert!(server.studio.sessions.get(&run).is_none() && server.studio.sessions.get(&terminal).is_none());
    assert!(!built[0].exists());
}

/// The folders where this test process built a program called `name`.
fn built_programs(name: &str) -> Vec<std::path::PathBuf> {
    let prefix = format!("vela-studio-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.file_name().is_some_and(|folder| folder.to_string_lossy().starts_with(&prefix)))
        .filter(|path| path.join(format!("{}{}", name, std::env::consts::EXE_SUFFIX)).exists())
        .collect()
}
