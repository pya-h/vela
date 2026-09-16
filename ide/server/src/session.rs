//! Programs and shells running in pseudo-terminals.
//!
//! Each session has three helpers: a thread reading the terminal's output, a
//! thread writing keystrokes to it, and a thread waiting for the process to
//! exit. A task, the pump, gathers their events. It keeps the recent output,
//! so a WebSocket that connects late (or reconnects) sees everything, and
//! forwards output to the one connected WebSocket, if any. When the client is
//! slow, the pump holds back reading output, and so the program writing it
//! waits, as in a real terminal; it still handles its other events meanwhile.
//!
//! A session without a WebSocket for `idle_timeout` is stopped and removed.
//! Stopping sends SIGHUP to the process group (the process leads a new
//! session, so its group holds its children too), then SIGKILL if it's still
//! running shortly after.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc::error::TrySendError;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
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
    /// The server is shutting down.
    Closing,
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

    /// Where keystrokes for the process go. Sending waits while its input is
    /// backed up; the channel closes when the terminal stops taking input.
    pub fn input(&self) -> mpsc::Sender<Vec<u8>> {
        self.input.clone()
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

/// Hangs up every process still in the session whose leader, `leader`, has
/// just exited (the child called setsid, so the session's id is its pid).
/// Processes that ignore hangups keep running. Only on Linux, where `/proc`
/// lists each process's session.
#[cfg(target_os = "linux")]
fn hang_up_session(leader: u32) {
    let Ok(entries) = std::fs::read_dir("/proc") else { return };
    for pid in entries.flatten().filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok()) {
        // `pid (name) state ppid pgrp session …`; the name may hold spaces and parentheses.
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, rest)| rest).split_whitespace().collect();
        if fields.get(3).and_then(|session| session.parse::<u32>().ok()) == Some(leader) {
            // SAFETY: `kill` has no memory-safety preconditions.
            unsafe {
                libc::kill(pid, libc::SIGHUP);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn hang_up_session(_leader: u32) {}

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
    /// The run session, if one is open: starting another run stops it. Held
    /// while a run starts, so concurrent runs can't both survive.
    run: Mutex<Option<String>>,
    /// Held while a terminal starts, from checking the limit until it is
    /// registered, so concurrent requests can't start more than the limit.
    terminal_start: Mutex<()>,
    idle_timeout: Duration,
    max_terminals: usize,
    /// Set by `shutdown`: no new sessions start.
    closing: AtomicBool,
}

impl Sessions {
    pub fn new(idle_timeout: Duration, max_terminals: usize) -> Arc<Sessions> {
        Arc::new(Sessions {
            open: Mutex::new(HashMap::new()),
            run: Mutex::new(None),
            terminal_start: Mutex::new(()),
            idle_timeout,
            max_terminals,
            closing: AtomicBool::new(false),
        })
    }

    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        lock(&self.open).get(id).cloned()
    }

    /// The sessions of `kind` whose process is still running. (One that has
    /// exited lingers until its page disconnects, but doesn't count.)
    pub fn count(&self, kind: SessionKind) -> usize {
        lock(&self.open).values().filter(|session| session.kind == kind && session.is_alive()).count()
    }

    /// Stops the program started by the latest run, if it is still running.
    pub fn stop_run(&self) {
        let previous = lock(&self.run).take();
        if let Some(session) = previous.and_then(|id| self.get(&id)) {
            session.stop();
        }
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
        if self.closing.load(Ordering::SeqCst) {
            return Err(SessionError::Closing);
        }
        let _terminal_start = (kind == SessionKind::Terminal).then(|| lock(&self.terminal_start));
        if kind == SessionKind::Terminal && self.count(SessionKind::Terminal) >= self.max_terminals {
            return Err(SessionError::TooManyTerminals);
        }
        // For a run, the lock is held until the new run is registered.
        let mut run = (kind == SessionKind::Run).then(|| lock(&self.run));
        if let Some(previous) = run.as_mut().and_then(|run| run.take())
            && let Some(session) = self.get(&previous)
        {
            session.stop();
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");

        let pair = native_pty_system().openpty(size).map_err(spawn_error)?;
        let child = pair.slave.spawn_command(command).map_err(spawn_error)?;
        // The terminal's other end now belongs to the child; keeping it open
        // here would hide the end of its output.
        drop(pair.slave);
        let mut orphan = child.clone_killer();
        let started = self.start(kind, child, pair.master, scratch);
        match (&started, run.as_mut()) {
            (Ok(id), Some(run)) => **run = Some(id.clone()),
            // Nothing would ever stop or wait for it.
            (Err(_), _) => drop(orphan.kill()),
            _ => {}
        }
        started
    }

    /// Sets up the helpers and the pump for a spawned process.
    fn start(
        self: &Arc<Self>,
        kind: SessionKind,
        mut child: Box<dyn Child + Send + Sync>,
        master: Box<dyn MasterPty + Send>,
        scratch: Option<ScratchDir>,
    ) -> Result<String, SessionError> {
        let reader = master.try_clone_reader().map_err(spawn_error)?;
        let writer = master.take_writer().map_err(spawn_error)?;
        let id = random_hex(16).map_err(spawn_error)?;
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
        spawn_thread("vela-studio-wait", move || {
            let status = child.wait();
            waiting.store(false, Ordering::SeqCst);
            // Jobs it left running in process groups of their own get the
            // hangup a closed terminal gives. (Their session keeps the pid in
            // use, so nothing else can have taken it yet.)
            if let Some(pid) = pid {
                hang_up_session(pid);
            }
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
            master: Mutex::new(master),
            input: input_tx,
            control: control_tx,
        });
        lock(&self.open).insert(id.clone(), session.clone());
        let pump = Pump { sessions: self.clone(), id: id.clone(), session, idle_timeout: self.idle_timeout, _scratch: scratch };
        tokio::spawn(pump.run(output_rx, exit_rx, control_rx));
        Ok(id)
    }

    /// Ends every session, waiting briefly for their processes to exit and
    /// their built programs to be removed (for server shutdown).
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
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
        // `try_lock`: a run that is starting holds this lock, and will replace the id anyway.
        if let Ok(mut run) = self.run.try_lock()
            && run.as_deref() == Some(id)
        {
            *run = None;
        }
    }
}

fn spawn_error(error: impl std::fmt::Display) -> SessionError {
    SessionError::Spawn(error.to_string())
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), SessionError> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map(drop)
        .map_err(spawn_error)
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
        // An event the subscriber had no room for. While one waits, no more
        // output is read, which holds back the program; everything else goes on.
        let mut held: Option<Event> = None;
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
                && held.is_none()
                && (output_done || at.elapsed() >= OUTPUT_GRACE)
            {
                reported = true;
                deliver(Event::Exit(info.clone()), &mut held, &mut subscriber, &mut idle_since);
                if ending {
                    break;
                }
            }
            let idle_deadline = idle_since.map(|since| since + self.idle_timeout);
            let grace_deadline = exited.as_ref().filter(|_| !reported).map(|(_, at)| *at + OUTPUT_GRACE);
            let watched = subscriber.clone();
            let waiting = held.is_some();

            tokio::select! {
                chunk = output.recv(), if !output_done && !waiting => match chunk {
                    Some(chunk) => {
                        scrollback.push(&chunk);
                        deliver(Event::Output(Bytes::from(chunk)), &mut held, &mut subscriber, &mut idle_since);
                    }
                    None => output_done = true,
                },
                // Room for the held event.
                permit = async { match &watched { Some(events) => events.reserve().await, None => std::future::pending().await } }, if waiting && watched.is_some() => match permit {
                    Ok(permit) => {
                        if let Some(event) = held.take() {
                            permit.send(event);
                        }
                    }
                    Err(_) => {
                        held = None;
                        subscriber = None;
                        idle_since = Some(tokio::time::Instant::now());
                    }
                },
                status = &mut exit, if exited.is_none() => {
                    // The waiter thread always sends; without it, report a failure.
                    let info = status.unwrap_or(Exit { code: 1, signal: None, duration_ms: 0 });
                    exited = Some((info, tokio::time::Instant::now()));
                },
                Some(message) = control.recv() => match message {
                    Control::Attach(events) => {
                        // The replay includes anything held for the previous subscriber.
                        held = None;
                        let replay: Vec<u8> = scrollback.bytes.iter().copied().collect();
                        // A new channel has room for both messages.
                        let mut connected = replay.is_empty() || events.try_send(Event::Output(Bytes::from(replay))).is_ok();
                        if connected && reported && let Some((info, _)) = &exited {
                            connected = events.try_send(Event::Exit(info.clone())).is_ok();
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
                () = async { if let Some(events) = &watched { events.closed().await } }, if watched.is_some() && !waiting => {
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
            // A stopped session doesn't wait for a subscriber that stopped reading.
            if ending && held.is_some() && exited.is_some() {
                held = None;
            }
        }
        self.sessions.remove(&self.id);
    }
}

/// Sends `event` to the subscriber, if any, without waiting: with no room, it
/// is held for later; with the subscriber gone, it is dropped and the idle
/// clock starts.
fn deliver(
    event: Event,
    held: &mut Option<Event>,
    subscriber: &mut Option<mpsc::Sender<Event>>,
    idle_since: &mut Option<tokio::time::Instant>,
) {
    let Some(events) = subscriber.as_ref() else { return };
    match events.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(event)) => *held = Some(event),
        Err(TrySendError::Closed(_)) => {
            *subscriber = None;
            *idle_since = Some(tokio::time::Instant::now());
        }
    }
}

fn far_future() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 3600)
}
