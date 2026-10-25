//! Synchronous msgpack-RPC client for `nvim --embed`.
//!
//! One reader thread decodes messages from nvim's stdout. Responses are routed
//! by msgid to the `request` call that waits for them; notifications and
//! requests from nvim go to a channel the main loop drains with `try_recv`.
//! A second thread drains stderr into a small ring buffer for error messages.
//!
//! This file must stay free of `crate::` paths: `tests/nvim_rpc.rs` includes it
//! by path because the crate is a binary.

use anyhow::{Result, anyhow, bail};
use rmpv::Value;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Bytes that may wait for a stalled nvim before further calls are refused.
/// A single frame larger than this is still accepted when nothing else waits.
const MAX_QUEUED: usize = 8 * 1024 * 1024;

/// Lines of nvim stderr kept for error messages.
const STDERR_LINES: usize = 50;
/// How long the reader waits for nvim to be reaped after its stdout closed
/// before it kills it.
const REAP_GRACE: Duration = Duration::from_secs(1);

const MSG_REQUEST: i64 = 0;
const MSG_RESPONSE: i64 = 1;
const MSG_NOTIFICATION: i64 = 2;

/// A message from nvim that is not the response to one of our requests.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Notification {
        method: String,
        params: Vec<Value>,
    },
    /// nvim called `rpcrequest`; answer with [`Rpc::respond`] using the same `id`.
    Request {
        id: u64,
        method: String,
        params: Vec<Value>,
    },
    /// The nvim process ended. `None` when it died from a signal. Always the
    /// last message; afterwards every call fails immediately.
    Exited(Option<i32>),
}

/// Raised by the reader thread whenever a message is queued for `try_recv`.
///
/// crossterm cannot wait on a channel, so the main loop cannot block on nvim
/// and the keyboard at once. Instead it asks `take()` after each short poll:
/// `true` means "something arrived since the last call", so it drains
/// `try_recv` and redraws, and keeps the poll timeout short while it stays
/// `true`. Cheap to clone; all clones share one flag.
#[derive(Debug, Clone, Default)]
pub struct Waker(Arc<AtomicBool>);

impl Waker {
    fn raise(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether anything arrived since the last call; resets the flag.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

type Reply = std::result::Result<Value, String>;

struct Shared {
    /// Waiting `request` calls by msgid. Set to `None` once the channel is
    /// closed, under the same lock, so a late request cannot be left waiting.
    pending: Mutex<Option<HashMap<u64, Sender<Reply>>>>,
    stderr: Mutex<VecDeque<String>>,
    /// Set after the child has been reaped and `Exited` queued.
    closed: AtomicBool,
}

/// Locks, ignoring poisoning: the guarded data stays consistent because every
/// critical section is a single insert/remove/push.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Rpc {
    child: Arc<Mutex<Child>>,
    /// Frames for the writer thread, which owns stdin. `None` once dropped.
    out: Option<Sender<Vec<u8>>>,
    /// Bytes handed to the writer thread and not yet written.
    queued: Arc<AtomicUsize>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    incoming: Mutex<Receiver<Incoming>>,
    waker: Waker,
}

impl Rpc {
    /// Start `cmd --embed [extra_args]` in `cwd`. `NVIM_APPNAME` is set only
    /// when `appname` is non-empty.
    pub fn spawn(cmd: &str, appname: &str, cwd: &Path, extra_args: &[&str]) -> Result<Rpc> {
        let mut command = Command::new(cmd);
        command
            .arg("--embed")
            .args(extra_args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !appname.is_empty() {
            command.env("NVIM_APPNAME", appname);
        }
        let mut child = command
            .spawn()
            .map_err(|e| anyhow!("cannot start `{cmd}`: {e}"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("no stderr pipe"))?;

        let shared = Arc::new(Shared {
            pending: Mutex::new(Some(HashMap::new())),
            stderr: Mutex::new(VecDeque::new()),
            closed: AtomicBool::new(false),
        });
        let child = Arc::new(Mutex::new(child));
        let (tx, rx) = mpsc::channel();
        let waker = Waker::default();

        {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("nvim-stderr".into())
                .spawn(move || drain_stderr(stderr, &shared))?;
        }
        let (out, frames) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        {
            let queued = Arc::clone(&queued);
            std::thread::Builder::new()
                .name("nvim-write".into())
                .spawn(move || writer_loop(stdin, &frames, &queued))?;
        }
        {
            let shared = Arc::clone(&shared);
            let child = Arc::clone(&child);
            let waker = waker.clone();
            std::thread::Builder::new()
                .name("nvim-rpc".into())
                .spawn(move || reader_loop(stdout, tx, &shared, &child, &waker))?;
        }

        Ok(Rpc {
            child,
            out: Some(out),
            queued,
            shared,
            next_id: AtomicU64::new(0),
            incoming: Mutex::new(rx),
            waker,
        })
    }

    /// Call `method` and wait up to `timeout` for the result. An RPC error
    /// becomes `Err` carrying nvim's message.
    pub fn request(&self, method: &str, args: Vec<Value>, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = lock(&self.shared.pending);
            match pending.as_mut() {
                Some(map) => map.insert(id, tx),
                None => bail!(self.closed_error()),
            };
        }
        let msg = Value::Array(vec![
            Value::from(MSG_REQUEST),
            Value::from(id),
            Value::from(method),
            Value::Array(args),
        ]);
        if let Err(e) = self.write(&msg) {
            self.forget(id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(anyhow!("{message}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.forget(id);
                Err(anyhow!(
                    "nvim request `{method}` timed out after {timeout:?}"
                ))
            }
            // The reader dropped our sender: the channel closed while we waited.
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(self.closed_error())),
        }
    }

    /// Fire-and-forget call.
    pub fn notify(&self, method: &str, args: Vec<Value>) -> Result<()> {
        if self.is_closed() {
            bail!(self.closed_error());
        }
        self.write(&Value::Array(vec![
            Value::from(MSG_NOTIFICATION),
            Value::from(method),
            Value::Array(args),
        ]))
    }

    /// Next queued message from nvim, if any. Never blocks.
    pub fn try_recv(&self) -> Option<Incoming> {
        lock(&self.incoming).try_recv().ok()
    }

    /// Answer an [`Incoming::Request`]. `Err(text)` is sent as an RPC error.
    pub fn respond(&self, id: u64, result: std::result::Result<Value, String>) -> Result<()> {
        let (error, value) = match result {
            Ok(v) => (Value::Nil, v),
            Err(message) => (Value::from(message), Value::Nil),
        };
        self.write(&Value::Array(vec![
            Value::from(MSG_RESPONSE),
            Value::from(id),
            error,
            value,
        ]))
    }

    /// Handle the main loop checks after each poll; see [`Waker`].
    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// The process id of the child.
    pub fn pid(&self) -> u32 {
        lock(&self.child).id()
    }

    /// Whether the channel is closed (nvim exited).
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// The last lines nvim wrote to stderr.
    pub fn stderr_tail(&self) -> String {
        lock(&self.shared.stderr)
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ask nvim to `qall!`, wait up to `timeout` for it to exit, then kill it.
    pub fn shutdown(self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        // `qall!` makes nvim exit without answering; notify never waits.
        let _ = self.notify("nvim_command", vec![Value::from("qall!")]);
        while !self.is_closed() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Drop kills and reaps a child that is still running.
    }

    fn write(&self, msg: &Value) -> Result<()> {
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, msg)?;
        let Some(out) = &self.out else {
            bail!(self.closed_error());
        };
        // Never write here: a stalled nvim would block the caller. The writer
        // thread owns stdin and sends each frame whole, so abandoning a call
        // (timeout) never leaves half a frame in the stream.
        let len = buf.len();
        let before = self.queued.fetch_add(len, Ordering::AcqRel);
        if before > 0 && before + len > MAX_QUEUED {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            bail!("nvim is not accepting input ({before} bytes waiting)");
        }
        if out.send(buf).is_err() {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            bail!(self.closed_error());
        }
        Ok(())
    }

    fn forget(&self, id: u64) {
        if let Some(map) = lock(&self.shared.pending).as_mut() {
            map.remove(&id);
        }
    }

    fn closed_error(&self) -> String {
        let tail = self.stderr_tail();
        if tail.is_empty() {
            "nvim is not running".to_string()
        } else {
            format!("nvim is not running: {tail}")
        }
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        // Dropping the sender lets the writer finish and close stdin, which
        // lets a healthy nvim exit on its own; a stuck one is killed, and the
        // kill also unblocks a writer stuck on a full pipe.
        self.out.take();
        let mut child = lock(&self.child);
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Owns nvim's stdin and writes queued frames one after the other. It may block
/// on a full pipe; nothing else waits for it, and killing nvim ends it with a
/// broken pipe. After a write error it stops, so later sends fail.
fn writer_loop(mut stdin: ChildStdin, frames: &Receiver<Vec<u8>>, queued: &AtomicUsize) {
    while let Ok(frame) = frames.recv() {
        let written = stdin.write_all(&frame);
        queued.fetch_sub(frame.len(), Ordering::AcqRel);
        if written.is_err() {
            return;
        }
    }
}

fn drain_stderr(stderr: impl Read, shared: &Shared) {
    let mut reader = BufReader::new(stderr);
    // Not `lines()`: nvim may write non-UTF-8 bytes; decode them lossily.
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw).trim_end().to_string();
        let mut ring = lock(&shared.stderr);
        if ring.len() == STDERR_LINES {
            ring.pop_front();
        }
        ring.push_back(line);
    }
}

fn reader_loop(
    stdout: impl Read,
    tx: Sender<Incoming>,
    shared: &Shared,
    child: &Mutex<Child>,
    waker: &Waker,
) {
    let mut reader = BufReader::new(stdout);
    // EOF, a read error or a malformed message all end the session.
    while let Ok(value) = rmpv::decode::read_value(&mut reader) {
        let Some(parts) = value.as_array() else {
            continue;
        };
        match parts.first().and_then(Value::as_i64) {
            Some(MSG_RESPONSE) if parts.len() == 4 => {
                let (Some(id), error, result) = (parts[1].as_u64(), &parts[2], &parts[3]) else {
                    continue;
                };
                let reply = if error.is_nil() {
                    Ok(result.clone())
                } else {
                    Err(error_text(error))
                };
                let waiting = lock(&shared.pending).as_mut().and_then(|m| m.remove(&id));
                if let Some(waiting) = waiting {
                    let _ = waiting.send(reply);
                }
            }
            Some(MSG_NOTIFICATION) if parts.len() == 3 => {
                let (Some(method), Some(params)) = (parts[1].as_str(), parts[2].as_array()) else {
                    continue;
                };
                let _ = tx.send(Incoming::Notification {
                    method: method.to_string(),
                    params: params.clone(),
                });
                waker.raise();
            }
            Some(MSG_REQUEST) if parts.len() == 4 => {
                let (Some(id), Some(method), Some(params)) =
                    (parts[1].as_u64(), parts[2].as_str(), parts[3].as_array())
                else {
                    continue;
                };
                let _ = tx.send(Incoming::Request {
                    id,
                    method: method.to_string(),
                    params: params.clone(),
                });
                waker.raise();
            }
            _ => {}
        }
    }

    // Stop new requests and fail the waiting ones now: dropping their senders
    // wakes each `request` with "disconnected".
    drop(lock(&shared.pending).take());

    let code = reap(child);
    shared.closed.store(true, Ordering::Release);
    let _ = tx.send(Incoming::Exited(code));
    waker.raise();
}

/// Wait for the child after its stdout closed; kill it if it lingers.
fn reap(child: &Mutex<Child>) -> Option<i32> {
    let deadline = Instant::now() + REAP_GRACE;
    loop {
        {
            let mut child = lock(child);
            match child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    return child.wait().ok().and_then(|s| s.code());
                }
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// nvim errors are `[type, message]`; show just the message when it has that shape.
fn error_text(error: &Value) -> String {
    match error.as_array().map(Vec::as_slice) {
        Some([_, message]) => message
            .as_str()
            .map_or_else(|| message.to_string(), str::to_string),
        _ => error
            .as_str()
            .map_or_else(|| error.to_string(), str::to_string),
    }
}
