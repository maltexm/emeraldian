//! One embedded `nvim --embed` process: the RPC client, its grid, and the calls
//! the rest of the app makes on it.
//!
//! Kept free of `crate::` paths other than `crate::config`, so
//! `tests/nvim_session.rs` can include it by path next to `config.rs` — the
//! crate is a binary and `App` is not reachable from an integration test.
//! Everything that needs the app (starting the session, following the active
//! tab, reporting an exit) lives in `nvim::poll`.

use super::grid::Grid;
use super::rpc::{Incoming, Rpc, Waker};
use crate::config::EditorConfig;
use anyhow::{Result, anyhow};
use rmpv::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a call that has to be answered may take. nvim answers these from
/// its event loop within milliseconds, and the main loop waits while one is
/// out, so a wedged nvim must not hold it up for long.
const CALL: Duration = Duration::from_secs(2);

/// How long attaching the UI may take. Generous on purpose: nvim answers only
/// after its startup — a lazy plugin manager loading its cache included — has
/// finished.
const ATTACH: Duration = Duration::from_secs(10);

/// How long nvim gets to `qall!` before it is killed.
const SHUTDOWN: Duration = Duration::from_secs(2);

/// How long after input, or after nvim last said something, the session counts
/// as busy. A keystroke usually comes back as several messages over a few
/// milliseconds, and the loop should still be polling quickly for the last.
const BUSY_WINDOW: Duration = Duration::from_millis(60);

/// If set, the path of a file that gets one line per redraw: microseconds from
/// the oldest input nvim had not answered yet to the moment its redraw was
/// applied. This is how keystroke-to-screen latency is measured on a live run.
const LATENCY_LOG_ENV: &str = "EMERALDIAN_NVIM_LATENCY";

/// What changed since the last [`Session::poll`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The grid has a complete redraw: it is ready to be drawn.
    Redraw,
    /// nvim ended. Always the last event, and only for an exit emeraldian did
    /// not ask for — a requested shutdown consumes the session instead.
    Exited(Option<i32>),
    /// Something nvim's injected bridge reported (links, saves, app actions).
    /// Empty until the bridge lands, so nothing produces this yet.
    Bridge(BridgeEvent),
}

/// What the injected bridge tells emeraldian about the note being edited.
///
/// Filled in by the bridge task; the variant exists now so the session's event
/// type does not change under the code that has to consume it.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {}

/// A running embedded nvim, with the grid its UI is attached to.
pub struct Session {
    rpc: Rpc,
    grid: Grid,
    waker: Waker,
    /// Buffers shown so far, by canonical path. Switching back to one is a
    /// `:buffer`, never a reload: a reload would drop unsaved edits.
    ///
    /// The handle is kept as nvim sent it: a client that registered the API
    /// types gets `Buffer` as a msgpack extension, everything else as an
    /// integer, and handing back what nvim handed out works for both.
    buffers: HashMap<PathBuf, Value>,
    /// The path [`Session::open`] was last given, exactly as given. Callers
    /// pass the active tab's note on every loop turn, so the common case — the
    /// note is already showing — must not touch the filesystem.
    shown: Option<PathBuf>,
    /// The size nvim was last told about.
    size: (u16, u16),
    /// Until when the loop should poll quickly; see [`BUSY_WINDOW`].
    busy_until: Instant,
    /// When the oldest input nvim has not answered with a redraw was handed
    /// over. Only kept for the latency log.
    input_at: Option<Instant>,
    latency_log: Option<std::fs::File>,
    /// Set once the reader saw nvim exit.
    exited: Option<Option<i32>>,
}

impl Session {
    /// Start nvim as `cfg` says, in `vault_root`, with a `size` grid attached.
    ///
    /// An empty `nvim_appname` runs nvim's own default config, which is what an
    /// emeraldian without a notes profile should get.
    pub fn start(cfg: &EditorConfig, vault_root: &Path, size: (u16, u16)) -> Result<Session> {
        Session::start_with(cfg, vault_root, size, &[])
    }

    /// [`Session::start`] with extra nvim arguments. The tests use it to keep
    /// their instances away from the user's config, shada and swap files.
    pub fn start_with(
        cfg: &EditorConfig,
        vault_root: &Path,
        size: (u16, u16),
        extra_args: &[&str],
    ) -> Result<Session> {
        // A UI cannot be attached to a zero-sized grid, and a pane can be that
        // narrow for a frame while a terminal is being resized.
        let size = (size.0.max(1), size.1.max(1));
        let rpc = Rpc::spawn(&cfg.nvim, &cfg.nvim_appname, vault_root, extra_args)?;

        let options = Value::Map(vec![
            (Value::from("ext_linegrid"), Value::from(true)),
            (Value::from("rgb"), Value::from(true)),
        ]);
        rpc.request(
            "nvim_ui_attach",
            vec![Value::from(size.0), Value::from(size.1), options],
            ATTACH,
        )?;

        // nvim starts in an unnamed buffer that is nobody's note. Left alone it
        // becomes the note's alternate file, so the first `Ctrl+O` -- or any
        // other jump back -- lands in an empty buffer instead of the note the
        // pane replaced. `bufhidden = wipe` takes it out of the list as soon as
        // the first note hides it.
        discard_initial_buffer(&rpc)?;

        let latency_log = std::env::var_os(LATENCY_LOG_ENV).and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        });

        Ok(Session {
            waker: rpc.waker(),
            rpc,
            grid: Grid::new(size.0, size.1),
            buffers: HashMap::new(),
            shown: None,
            size,
            busy_until: Instant::now(),
            input_at: None,
            latency_log,
            exited: None,
        })
    }

    /// Show `path` in the pane, opening it in nvim the first time.
    ///
    /// Callers pass the path every loop turn — the active tab's note — so
    /// showing the note that is already there costs nothing.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        if self.exited.is_some() || self.shown.as_deref() == Some(path) {
            return Ok(());
        }
        let key = canonical(path);
        let buffer = match self.buffers.get(&key) {
            Some(buffer) => buffer.clone(),
            // `bufadd` only names the file, so it answers at once; it is the
            // switch below that reads it. A path given as an argument cannot
            // be mistaken for command syntax (`|`, `%`, spaces).
            None => {
                let buffer = self.request(
                    "nvim_call_function",
                    vec![
                        Value::from("bufadd"),
                        Value::Array(vec![Value::from(key.to_string_lossy().into_owned())]),
                    ],
                )?;
                self.buffers.insert(key, buffer.clone());
                buffer
            }
        };
        // Not a request: reading the file can end in a prompt (a swap file
        // left by a crash) that only a key can answer, and the loop has to stay
        // free to deliver that key.
        //
        // Not `nvim_set_current_buf` either: that is an API call and keys are
        // typeahead, and nvim consumes typeahead first, so a key sent right
        // after the switch would land in the buffer being left. `<Cmd>` runs
        // the switch in the key stream, in order with whatever follows it, in
        // any mode.
        let number = buffer
            .as_i64()
            .ok_or_else(|| anyhow!("nvim answered bufadd with {buffer}"))?;
        self.rpc.notify(
            "nvim_input",
            vec![Value::from(format!("<Cmd>buffer {number}<CR>"))],
        )?;
        self.shown = Some(path.to_path_buf());
        // A buffer switch always redraws.
        self.touch();
        Ok(())
    }

    /// Tell nvim how large the pane is, when that changed.
    pub fn resize(&mut self, width: u16, height: u16) {
        let size = (width.max(1), height.max(1));
        if self.size == size || self.exited.is_some() {
            return;
        }
        self.size = size;
        self.touch();
        // Fire and forget: the answer is the redraw that follows, and the loop
        // must not block on a layout change.
        let _ = self.rpc.notify(
            "nvim_ui_try_resize",
            vec![Value::from(size.0), Value::from(size.1)],
        );
    }

    /// Feed `keys` as nvim input notation — `ihello<Esc>`, `:w<CR>`.
    pub fn input(&mut self, keys: &str) {
        if keys.is_empty() || self.exited.is_some() {
            return;
        }
        self.touch();
        let _ = self
            .rpc
            .notify("nvim_input", vec![Value::from(keys.to_string())]);
    }

    /// Send one mouse event, in pane-relative cells, as the arguments of
    /// `nvim_input_mouse`.
    pub fn mouse(&mut self, button: &str, action: &str, modifier: &str, row: i64, col: i64) {
        if self.exited.is_some() {
            return;
        }
        self.touch();
        // Grid 0 is the global grid, i.e. "the pane this UI draws".
        let _ = self.rpc.notify(
            "nvim_input_mouse",
            vec![
                Value::from(button),
                Value::from(action),
                Value::from(modifier),
                Value::from(0),
                Value::from(row),
                Value::from(col),
            ],
        );
    }

    /// Paste text at the cursor, as one block.
    ///
    /// `nvim_paste` runs nvim's own paste handling — `'autoindent'`, the
    /// unnamed register, `TextYankPost` — which typing the text key by key
    /// would not.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() || self.exited.is_some() {
            return;
        }
        self.touch();
        // `crlf = true` lets nvim read the line endings of what was pasted
        // rather than treating every `\r` as a control character; phase -1 is
        // the whole paste in one call.
        let _ = self.rpc.notify(
            "nvim_paste",
            vec![
                Value::from(text.to_string()),
                Value::from(true),
                Value::from(-1),
            ],
        );
    }

    /// Drain what nvim has sent: redraws into the grid, bridge notifications
    /// into events, and requests nvim makes of us.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut redraw = false;
        // Taken before draining: a message the reader queues after the drain
        // below keeps the flag up for the next turn instead of being lost.
        let mut heard = self.waker.take();
        while let Some(incoming) = self.rpc.try_recv() {
            heard = true;
            match incoming {
                Incoming::Notification { method, params } if method == "redraw" => {
                    redraw |= self.grid.apply(&params);
                }
                // The bridge's notifications. Nothing sends them yet, so they
                // are dropped rather than guessed at.
                Incoming::Notification { .. } => {}
                Incoming::Request { id, .. } => {
                    // Answering is what keeps nvim from waiting on emeraldian;
                    // no request is served yet, and saying so is better than
                    // leaving it to time out.
                    let _ = self.rpc.respond(
                        id,
                        Err("emeraldian does not serve this request".to_string()),
                    );
                }
                Incoming::Exited(code) => {
                    self.exited = Some(code);
                    events.push(Event::Exited(code));
                }
            }
        }
        if heard {
            self.busy_until = Instant::now() + BUSY_WINDOW;
        }
        if redraw {
            self.log_latency();
            events.push(Event::Redraw);
        }
        events
    }

    /// The grid nvim draws into. One cell per screen cell.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Whether nvim was just given input, or just said something, so the main
    /// loop should keep polling for input in short steps.
    #[must_use]
    pub fn busy(&self) -> bool {
        Instant::now() < self.busy_until
    }

    /// Whether nvim has ended; the exit status is delivered once, as
    /// [`Event::Exited`].
    #[must_use]
    pub fn exited(&self) -> bool {
        self.exited.is_some()
    }

    /// Whether the buffer for `path` has unsaved changes.
    ///
    /// Read from nvim rather than cached: the answer has to be right at the
    /// moment a write tool or a tab close asks, and the bridge that will push
    /// these as notifications does not exist yet.
    pub fn is_modified(&self, path: &Path) -> bool {
        self.buffer(path)
            .and_then(|buffer| {
                self.rpc
                    .request(
                        "nvim_get_option_value",
                        vec![
                            Value::from("modified"),
                            Value::Map(vec![(Value::from("buf"), buffer)]),
                        ],
                        CALL,
                    )
                    .ok()
            })
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    /// The buffer's text, unsaved edits included; `None` when nvim has not
    /// opened `path`.
    ///
    /// This is what the assistant reads an open note from, so that it sees what
    /// is on screen rather than what was last written to disk.
    pub fn buffer_text(&self, path: &Path) -> Option<String> {
        let buffer = self.buffer(path)?;
        let lines = self
            .rpc
            .request(
                "nvim_buf_get_lines",
                vec![buffer, Value::from(0), Value::from(-1), Value::from(false)],
                CALL,
            )
            .ok()?;
        let lines = lines.as_array()?;
        let mut text = lines
            .iter()
            .map(|line| {
                line.as_str()
                    .map_or_else(|| line.to_string(), str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        Some(text)
    }

    /// Write every modified buffer.
    pub fn save_all(&mut self) -> Result<()> {
        self.request("nvim_command", vec![Value::from("wall")])?;
        Ok(())
    }

    /// Ask nvim to quit and wait for it, so its own state is written rather
    /// than left behind by a kill.
    pub fn shutdown(self) {
        self.rpc.shutdown(SHUTDOWN);
    }

    /// The nvim buffer handle for `path`, once it has been opened.
    fn buffer(&self, path: &Path) -> Option<Value> {
        self.buffers.get(&canonical(path)).cloned()
    }

    /// A call that has to be answered, with the session's timeout.
    fn request(&self, method: &str, args: Vec<Value>) -> Result<Value> {
        if self.exited.is_some() {
            return Err(anyhow!("nvim is not running"));
        }
        self.rpc.request(method, args, CALL)
    }

    /// Note that nvim was handed work and owes the pane a redraw for it.
    fn touch(&mut self) {
        let now = Instant::now();
        self.busy_until = now + BUSY_WINDOW;
        self.input_at.get_or_insert(now);
    }

    /// Append the time since the oldest unanswered input to the latency log.
    fn log_latency(&mut self) {
        let Some(started) = self.input_at.take() else {
            return;
        };
        if let Some(log) = self.latency_log.as_mut() {
            let _ = writeln!(log, "{}", started.elapsed().as_micros());
        }
    }
}

/// Make nvim's starting buffer vanish once the first note hides it, so no jump
/// back can land in it. A fresh nvim always has a current buffer, so a failure
/// here means the session is not usable anyway.
fn discard_initial_buffer(rpc: &Rpc) -> Result<()> {
    let buffer = rpc.request("nvim_get_current_buf", Vec::new(), CALL)?;
    rpc.request(
        "nvim_set_option_value",
        vec![
            Value::from("bufhidden"),
            Value::from("wipe"),
            Value::Map(vec![(Value::from("buf"), buffer)]),
        ],
        CALL,
    )?;
    Ok(())
}

/// The path as nvim knows it: resolved, so that `/tmp` and a symlinked `/tmp`
/// are one buffer and not two.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
