//! Programs and shells running in pseudo-terminals.
//!
//! Each session has three helpers: a thread reading the terminal's output, a
//! thread writing keystrokes to it, and a thread waiting for the process to
//! exit. A task, the pump, gathers their events. It keeps the recent output,
//! so a WebSocket that connects late (or reconnects) sees everything, and
//! forwards output to the one connected WebSocket, if any. When the client is
//! slow, the pump waits, and so does the program writing output, as in a
//! real terminal.
//!
//! A session without a WebSocket for `idle_timeout` is stopped and removed.
//! Stopping sends SIGHUP to the process group (the process leads a new
//! session, so its group holds its children too), then SIGKILL if it's still
//! running shortly after.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, sleep_until};

use crate::scratch::ScratchDir;
use crate::security::random_hex;

/// How much recent output a session keeps for a WebSocket that connects later.
const SCROLLBACK_BYTES: usize = 1024 * 1024;
/// After the process exits, how long to wait for the rest of its output
/// before reporting the exit anyway (a background process may keep the
/// terminal open).
const OUTPUT_GRACE: Duration = Duration::from_millis(250);
/// How long a stopped process has to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    /// A built program.
    Run,
    /// A shell in the workspace.
    Terminal,
}

/// How a session's process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exit {
    /// The exit code; 1 when killed by a signal, as the terminal library reports it.
    pub code: u32,
    /// The signal that ended the process, such as `Hangup`, if any.
    pub signal: Option<String>,
    pub duration_ms: u64,
}

/// What a connected WebSocket receives.
#[derive(Debug)]
pub enum Event {
    Output(Bytes),
    Exit(Exit),
}

#[derive(Debug)]
pub enum SessionError {
    TooManyTerminals,
    Spawn(String),
}

enum Control {
    /// Connects a WebSocket, replacing the one before (which is closed).
    Attach(mpsc::Sender<Event>),
    /// Stops the process and ends the session once its exit is reported.
    End,
}

pub struct Session {
    pub kind: SessionKind,
    pid: Option<u32>,
    alive: Arc<AtomicBool>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: mpsc::Sender<Vec<u8>>,
    control: mpsc::UnboundedSender<Control>,
}

impl Session {
    /// Connects a WebSocket; `false` if the session has ended.
    pub fn attach(&self, events: mpsc::Sender<Event>) -> bool {
        self.control.send(Control::Attach(events)).is_ok()
    }

    /// Sends keystrokes to the process, waiting while its input is backed up.
    pub async fn input(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes).await;
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let size = PtySize { cols: cols.clamp(1, 1000), rows: rows.clamp(1, 1000), pixel_width: 0, pixel_height: 0 };
        let _ = lock(&self.master).resize(size);
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Hangs up the process group, and kills it if it's still running after
    /// a moment.
    pub fn stop(&self) {
        if !self.is_alive() {
            return;
        }
        self.signal(Signal::Hangup);
        let alive = self.alive.clone();
        let pid = self.pid;
        let killer = lock(&self.killer).clone_killer();
        tokio::spawn(async move {
            sleep(STOP_GRACE).await;
            if alive.load(Ordering::SeqCst) {
                signal(pid, killer, Signal::Kill);
            }
        });
    }

    fn signal(&self, which: Signal) {
        signal(self.pid, lock(&self.killer).clone_killer(), which);
    }
}

#[derive(Clone, Copy)]
enum Signal {
    Hangup,
    Kill,
}

#[cfg(unix)]
fn signal(pid: Option<u32>, _killer: Box<dyn ChildKiller + Send + Sync>, which: Signal) {
    let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()).filter(|&pid| pid > 0) else { return };
    let number = match which {
        Signal::Hangup => libc::SIGHUP,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` has no memory-safety preconditions. A negative pid
    // addresses the process group, which the child leads (it called setsid).
    unsafe {
        libc::kill(-pid, number);
    }
}

#[cfg(not(unix))]
fn signal(_pid: Option<u32>, mut killer: Box<dyn ChildKiller + Send + Sync>, _which: Signal) {
    let _ = killer.kill();
}

fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The open sessions.
pub struct Sessions {
    open: Mutex<HashMap<String, Arc<Session>>>,
    /// The run session, if one is open: starting another run stops it.
    run: Mutex<Option<String>>,
    idle_timeout: Duration,
    max_terminals: usize,
}

impl Sessions {
    pub fn new(idle_timeout: Duration, max_terminals: usize) -> Arc<Sessions> {
        Arc::new(Sessions { open: Mutex::new(HashMap::new()), run: Mutex::new(None), idle_timeout, max_terminals })
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        lock(&self.open).get(id).cloned()
    }

    pub fn count(&self, kind: SessionKind) -> usize {
        lock(&self.open).values().filter(|session| session.kind == kind).count()
    }

    /// Starts `command` in a new terminal of `size`, returning the session id.
    /// `scratch` (a built program's folder) is removed when the session ends.
    pub fn spawn(
        self: &Arc<Self>,
        kind: SessionKind,
        mut command: CommandBuilder,
        size: PtySize,
        scratch: Option<ScratchDir>,
    ) -> Result<String, SessionError> {
        if kind == SessionKind::Terminal && self.count(SessionKind::Terminal) >= self.max_terminals {
            return Err(SessionError::TooManyTerminals);
        }
        if kind == SessionKind::Run {
            let previous = lock(&self.run).take();
            if let Some(session) = previous.and_then(|id| self.get(&id)) {
                session.stop();
            }
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");

        let spawn_error = |error: &dyn std::fmt::Display| SessionError::Spawn(error.to_string());
        let pair = native_pty_system().openpty(size).map_err(|e| spawn_error(&e))?;
        let child = pair.slave.spawn_command(command).map_err(|e| spawn_error(&e))?;
        // The terminal's other end now belongs to the child; keeping it open
        // here would hide the end of its output.
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| spawn_error(&e))?;
        let writer = pair.master.take_writer().map_err(|e| spawn_error(&e))?;
        let id = random_hex(16).map_err(|e| spawn_error(&e))?;
        let started = Instant::now();

        let alive = Arc::new(AtomicBool::new(true));
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>(32);
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(64);
        let (exit_tx, exit_rx) = oneshot::channel();
        let (control_tx, control_rx) = mpsc::unbounded_channel();

        let killer = child.clone_killer();
        let pid = child.process_id();
        spawn_thread("vela-studio-pty-read", move || read_output(reader, output_tx))?;
        spawn_thread("vela-studio-pty-write", move || write_input(writer, input_rx))?;
        let waiting = alive.clone();
        let mut child = child;
        spawn_thread("vela-studio-wait", move || {
            let status = child.wait();
            waiting.store(false, Ordering::SeqCst);
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let exit = match status {
                Ok(status) => Exit { code: status.exit_code(), signal: status.signal().map(str::to_string), duration_ms },
                Err(_) => Exit { code: 1, signal: None, duration_ms },
            };
            let _ = exit_tx.send(exit);
        })?;

        let session = Arc::new(Session {
            kind,
            pid,
            alive,
            killer: Mutex::new(killer),
            master: Mutex::new(pair.master),
            input: input_tx,
            control: control_tx,
        });
        lock(&self.open).insert(id.clone(), session.clone());
        if kind == SessionKind::Run {
            *lock(&self.run) = Some(id.clone());
        }
        let pump = Pump { sessions: self.clone(), id: id.clone(), session, idle_timeout: self.idle_timeout, _scratch: scratch };
        tokio::spawn(pump.run(output_rx, exit_rx, control_rx));
        Ok(id)
    }

    /// Ends every session, waiting briefly for their processes to exit and
    /// their built programs to be removed (for server shutdown).
    pub async fn shutdown(&self) {
        let sessions: Vec<Arc<Session>> = lock(&self.open).values().cloned().collect();
        for session in &sessions {
            let _ = session.control.send(Control::End);
        }
        let deadline = Instant::now() + STOP_GRACE * 2 + OUTPUT_GRACE;
        while !lock(&self.open).is_empty() && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
    }

    fn remove(&self, id: &str) {
        lock(&self.open).remove(id);
        let mut run = lock(&self.run);
        if run.as_deref() == Some(id) {
            *run = None;
        }
    }
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), SessionError> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map(drop)
        .map_err(|error| SessionError::Spawn(error.to_string()))
}

fn read_output(mut reader: Box<dyn Read + Send>, output: mpsc::Sender<Vec<u8>>) {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                let Some(chunk) = buffer.get(..n) else { return };
                if output.blocking_send(chunk.to_vec()).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // Linux reports the end of a terminal's output as an error (EIO).
            Err(_) => return,
        }
    }
}

fn write_input(mut writer: Box<dyn Write + Send>, mut input: mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = input.blocking_recv() {
        if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
            return;
        }
    }
}

/// Recent output, dropping the oldest bytes past the limit.
struct Scrollback {
    bytes: VecDeque<u8>,
}

impl Scrollback {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk);
        let excess = self.bytes.len().saturating_sub(SCROLLBACK_BYTES);
        self.bytes.drain(..excess);
    }
}

struct Pump {
    sessions: Arc<Sessions>,
    id: String,
    session: Arc<Session>,
    idle_timeout: Duration,
    /// Removed when the pump ends.
    _scratch: Option<ScratchDir>,
}

impl Pump {
    async fn run(
        self,
        mut output: mpsc::Receiver<Vec<u8>>,
        mut exit: oneshot::Receiver<Exit>,
        mut control: mpsc::UnboundedReceiver<Control>,
    ) {
        let mut scrollback = Scrollback { bytes: VecDeque::new() };
        let mut subscriber: Option<mpsc::Sender<Event>> = None;
        let mut idle_since = Some(tokio::time::Instant::now());
        let mut output_done = false;
        // The exit, once the process has ended, and when that was noticed.
        let mut exited: Option<(Exit, tokio::time::Instant)> = None;
        let mut reported = false;
        // Whether the session ends as soon as the exit is reported.
        let mut ending = false;

        loop {
            if let Some((info, at)) = &exited
                && !reported
                && (output_done || at.elapsed() >= OUTPUT_GRACE)
            {
                reported = true;
                if let Some(events) = &subscriber
                    && events.send(Event::Exit(info.clone())).await.is_err()
                {
                    subscriber = None;
                    idle_since = Some(tokio::time::Instant::now());
                }
                if ending {
                    break;
                }
            }
            let idle_deadline = idle_since.map(|since| since + self.idle_timeout);
            let grace_deadline = exited.as_ref().filter(|_| !reported).map(|(_, at)| *at + OUTPUT_GRACE);
            let watched = subscriber.clone();

            tokio::select! {
                chunk = output.recv(), if !output_done => match chunk {
                    Some(chunk) => {
                        scrollback.push(&chunk);
                        if let Some(events) = &subscriber
                            && events.send(Event::Output(Bytes::from(chunk))).await.is_err()
                        {
                            subscriber = None;
                            idle_since = Some(tokio::time::Instant::now());
                        }
                    }
                    None => output_done = true,
                },
                status = &mut exit, if exited.is_none() => {
                    // The waiter thread always sends; without it, report a failure.
                    let info = status.unwrap_or(Exit { code: 1, signal: None, duration_ms: 0 });
                    exited = Some((info, tokio::time::Instant::now()));
                },
                Some(message) = control.recv() => match message {
                    Control::Attach(events) => {
                        let replay: Vec<u8> = scrollback.bytes.iter().copied().collect();
                        let mut connected = replay.is_empty() || events.send(Event::Output(Bytes::from(replay))).await.is_ok();
                        if connected && reported && let Some((info, _)) = &exited {
                            connected = events.send(Event::Exit(info.clone())).await.is_ok();
                        }
                        // Dropping the previous sender closes that WebSocket.
                        subscriber = connected.then_some(events);
                        idle_since = if connected { None } else { Some(tokio::time::Instant::now()) };
                    }
                    Control::End => {
                        if reported {
                            break;
                        }
                        ending = true;
                        self.session.stop();
                    }
                },
                // A closed WebSocket starts the idle clock.
                () = async { if let Some(events) = &watched { events.closed().await } }, if watched.is_some() => {
                    subscriber = None;
                    idle_since = Some(tokio::time::Instant::now());
                },
                () = sleep_until(grace_deadline.unwrap_or_else(far_future)), if grace_deadline.is_some() => {},
                () = sleep_until(idle_deadline.unwrap_or_else(far_future)), if idle_deadline.is_some() => {
                    if reported {
                        break;
                    }
                    // Nobody is watching a running process: stop it, and end
                    // once its exit is noticed.
                    ending = true;
                    idle_since = None;
                    self.session.stop();
                },
            }
        }
        self.sessions.remove(&self.id);
    }
}

fn far_future() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 3600)
}
