//! One embedded `nvim --embed` process: the RPC client, its grid, and the calls
//! the rest of the app makes on it.
//!
//! Kept free of `crate::` paths other than `crate::config`, so
//! `tests/nvim_session.rs` can include it by path next to `config.rs` — the
//! crate is a binary and `App` is not reachable from an integration test.
//! Everything that needs the app (starting the session, following the active
//! tab, reporting an exit) lives in `nvim::poll`.

use super::bridge::{self, Anchor, BridgeEvent};
use super::grid::Grid;
use super::rpc::{Incoming, Rpc, Waker};
use crate::config::EditorConfig;
use anyhow::{Result, anyhow};
use rmpv::Value;
use std::collections::HashSet;
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
    Bridge(BridgeEvent),
}

/// A running embedded nvim, with the grid its UI is attached to.
pub struct Session {
    rpc: Rpc,
    grid: Grid,
    waker: Waker,
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
    /// A jump to a heading or block that waits for its note to be on screen:
    /// the path as [`Session::open`] will be given it, and where to go.
    pending_anchor: Option<(PathBuf, Anchor)>,
    /// The names `[[` can be completed to, answered to the bridge's
    /// `emeraldian.notes` request.
    notes: Vec<String>,
    /// The index revision `notes` was built from.
    notes_revision: Option<u64>,
    /// Buffers the bridge last reported as having unsaved edits, by canonical
    /// path.
    modified: HashSet<PathBuf>,
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
        require_supported_nvim(&rpc)?;

        // nvim starts in an unnamed buffer that is nobody's note. Left alone it
        // becomes the note's alternate file, so the first `Ctrl+O` -- or any
        // other jump back -- lands in an empty buffer instead of the note the
        // pane replaced. `bufhidden = wipe` takes it out of the list as soon as
        // the first note hides it.
        discard_initial_buffer(&rpc)?;
        inject_bridge(&rpc, vault_root, cfg.auto_save)?;

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
            shown: None,
            size,
            busy_until: Instant::now(),
            input_at: None,
            latency_log,
            exited: None,
            pending_anchor: None,
            notes: Vec::new(),
            notes_revision: None,
            modified: HashSet::new(),
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
        // `bufadd` gives the buffer nvim already has for the file, whoever
        // opened it, and otherwise only names a new one, so it answers at once;
        // it is the switch below that reads the file. No handle is kept from
        // one call to the next: switching back to a buffer is a `:buffer`,
        // never a reload (that would drop unsaved edits), and nvim itself says
        // which buffer that is. A path given as an argument cannot be mistaken
        // for command syntax (`|`, `%`, spaces).
        let buffer = self.request(
            "nvim_call_function",
            vec![
                Value::from("bufadd"),
                Value::Array(vec![Value::from(key.to_string_lossy().into_owned())]),
            ],
        )?;
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
        self.apply_anchor(path);
        Ok(())
    }

    /// Put the cursor on a heading or block of `path` once it is on screen:
    /// at once when it already is, otherwise right after [`Session::open`]
    /// switches to it. Only the latest request is kept.
    pub fn goto(&mut self, path: &Path, anchor: Anchor) {
        if self.exited.is_some() {
            return;
        }
        self.pending_anchor = Some((path.to_path_buf(), anchor));
        if self.shown.as_deref() == Some(path) {
            self.apply_anchor(path);
        }
    }

    /// Send the queued jump if it is for `path`. The command goes in the key
    /// stream, right behind whatever switched buffers, so the order holds; its
    /// text travels hex-encoded (see `goto_hex` in `bridge.lua`).
    fn apply_anchor(&mut self, path: &Path) {
        if self
            .pending_anchor
            .as_ref()
            .map(|(for_path, _)| for_path.as_path())
            != Some(path)
        {
            return;
        }
        let Some((_, anchor)) = self.pending_anchor.take() else {
            return;
        };
        let (kind, text) = bridge::anchor_parts(&anchor);
        let hex: String = text.bytes().map(|byte| format!("{byte:02x}")).collect();
        let _ = self.rpc.notify(
            "nvim_input",
            vec![Value::from(format!(
                "<Cmd>lua require('emeraldian').goto_hex('{kind}','{hex}')<CR>"
            ))],
        );
        self.touch();
    }

    /// Record that nvim itself moved to `path` (reported as
    /// [`BridgeEvent::Entered`]), so that [`Session::open`] does not switch to
    /// the buffer it is already in.
    pub fn note_shown(&mut self, path: &Path) {
        self.shown = Some(path.to_path_buf());
    }

    /// The names `[[` completes to, built from the index at `revision`.
    pub fn set_notes(&mut self, revision: u64, names: Vec<String>) {
        self.notes = names;
        self.notes_revision = Some(revision);
    }

    /// The index revision [`Session::set_notes`] was last given.
    #[must_use]
    pub fn notes_revision(&self) -> Option<u64> {
        self.notes_revision
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
                // The bridge's notifications; anything else is dropped rather
                // than guessed at.
                Incoming::Notification { method, params } => {
                    if let Some(event) = bridge::parse_notification(&method, &params) {
                        if let BridgeEvent::Modified(path, dirty) = &event {
                            let path = canonical(path);
                            if *dirty {
                                self.modified.insert(path);
                            } else {
                                self.modified.remove(&path);
                            }
                        }
                        events.push(Event::Bridge(event));
                    }
                }
                Incoming::Request { id, method, .. } => {
                    // Answering is what keeps nvim from waiting on emeraldian,
                    // so every request gets one, an error for what is not
                    // served.
                    let answer = if method == "emeraldian.notes" {
                        Ok(Value::Array(
                            self.notes
                                .iter()
                                .map(|name| Value::from(name.as_str()))
                                .collect(),
                        ))
                    } else {
                        Err(format!("emeraldian does not serve {method}"))
                    };
                    let _ = self.rpc.respond(id, answer);
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
    /// Asked of nvim rather than read from the reports of the bridge: this is
    /// what a write tool, a tab close or a quit decides on, and a report can
    /// trail a keystroke. When nvim cannot answer, the buffer counts as
    /// modified — unless nvim is gone, which takes its buffers with it.
    pub fn is_modified(&self, path: &Path) -> bool {
        if self.exited.is_some() {
            return false;
        }
        let buffer = match self.buffer(path) {
            Ok(Some(buffer)) => buffer,
            Ok(None) => return false,
            Err(_) => return !self.rpc.is_closed(),
        };
        match self.rpc.request(
            "nvim_get_option_value",
            vec![
                Value::from("modified"),
                Value::Map(vec![(Value::from("buf"), buffer)]),
            ],
            CALL,
        ) {
            Ok(value) => value.as_bool().unwrap_or(true),
            Err(_) => !self.rpc.is_closed(),
        }
    }

    /// The buffer's text, unsaved edits included; `None` when nvim has not
    /// opened `path`.
    ///
    /// This is what the assistant reads an open note from, so that it sees what
    /// is on screen rather than what was last written to disk.
    pub fn buffer_text(&self, path: &Path) -> Option<String> {
        let buffer = self.buffer(path).ok()??;
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

    /// The process id of the nvim child.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.rpc.pid()
    }

    /// Whether the last `modified` report for `path` said it was dirty: cheap,
    /// for drawing the tab bar. Anything that decides about data asks
    /// [`Session::is_modified`] instead, because a report can trail a keystroke
    /// by a few milliseconds.
    #[must_use]
    pub fn shows_modified(&self, path: &Path) -> bool {
        self.modified.contains(&canonical(path))
    }

    /// Reload every buffer without unsaved edits whose file changed on disk.
    ///
    /// For after emeraldian or its assistant wrote a file: the buffer of a note
    /// that is not being edited must show what is there now. A buffer with
    /// unsaved edits is left alone (see `checktime` in `bridge.lua`).
    pub fn checktime(&mut self) {
        let _ = self.bridge_call("checktime", Vec::new());
    }

    /// The note at `from` was moved to `to` on disk: its buffer follows, with
    /// unsaved edits (written to the new file) and its place in the text. The
    /// old name is gone from nvim, so nothing can write the old file back.
    ///
    /// A path nvim has no buffer for is no error: there is nothing to move.
    pub fn rename(&mut self, from: &Path, to: &Path) -> Result<()> {
        let Some(buffer) = self.buffer(from)? else {
            return Ok(());
        };
        let name = canonical(to);
        self.bridge_call(
            "rename_buffer",
            vec![buffer, Value::from(name.to_string_lossy().into_owned())],
        )?;
        self.modified.remove(&canonical(from));
        if self.shown.as_deref() == Some(from) {
            self.shown = Some(to.to_path_buf());
        }
        if let Some((path, _)) = self.pending_anchor.as_mut()
            && path == from
        {
            *path = to.to_path_buf();
        }
        Ok(())
    }

    /// Replace the buffer's text with `text` and write it. This is how a note
    /// with unsaved edits gets its links rewritten: through the buffer, not
    /// under it. A path nvim has no buffer for is left alone.
    pub fn set_lines(&mut self, path: &Path, text: &str) -> Result<()> {
        let Some(buffer) = self.buffer(path)? else {
            return Ok(());
        };
        let body = text.strip_suffix('\n').unwrap_or(text);
        let lines: Vec<Value> = if text.is_empty() {
            Vec::new()
        } else {
            body.split('\n').map(Value::from).collect()
        };
        self.bridge_call("set_lines", vec![buffer, Value::Array(lines)])?;
        Ok(())
    }

    /// Write the buffer for `path`, if there is one. Fails the way `:w` does,
    /// and when nvim stops to ask something (the file changed on disk since it
    /// was read) the call times out with the question still on nvim's screen.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        let Some(buffer) = self.buffer(path)? else {
            return Ok(());
        };
        self.bridge_call("save", vec![buffer])?;
        Ok(())
    }

    /// Drop the buffer for `path`, so that opening the note reads the file
    /// again. With `discard` unsaved edits go with it; without, they make this
    /// fail and the buffer stays.
    pub fn close(&mut self, path: &Path, discard: bool) -> Result<()> {
        let key = canonical(path);
        let Some(buffer) = self.buffer(path)? else {
            return Ok(());
        };
        self.bridge_call("close_buffer", vec![buffer, Value::from(discard)])?;
        self.modified.remove(&key);
        if self.shown.as_deref() == Some(path) {
            // nvim moved its window to some other buffer.
            self.shown = None;
        }
        Ok(())
    }

    /// The notes whose buffers have unsaved edits, whoever opened them.
    #[must_use]
    pub fn modified_notes(&self) -> Vec<PathBuf> {
        self.bridge_call("modified_notes", Vec::new())
            .ok()
            .and_then(|names| {
                names.as_array().map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(|name| canonical(Path::new(name))))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// The names of nvim's buffers, as nvim spells them.
    #[must_use]
    pub fn buffer_names(&self) -> Vec<PathBuf> {
        self.bridge_call("buffer_names", Vec::new())
            .ok()
            .and_then(|names| {
                names.as_array().map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(PathBuf::from))
                        .collect()
                })
            })
            .unwrap_or_default()
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

    /// Ends nvim without a clean quit, which would delete the swap files: the
    /// unsaved text is not lost on a path nobody confirmed. `:preserve` writes
    /// the swap of every buffer first, so that what was typed since nvim last
    /// wrote one is in it.
    pub fn leave_for_recovery(self) {
        let _ = self.request("nvim_command", vec![Value::from("preserve")]);
        // Dropping the session kills the process, which leaves the swap files.
    }

    /// The nvim buffer handle for `path`, as nvim has it right now: `None` when
    /// it holds no buffer for the file. Never remembered between calls (see
    /// `buffer_of` in `bridge.lua`).
    ///
    /// The handle is passed on as nvim sent it: a client that registered the
    /// API types gets `Buffer` as a msgpack extension, everything else as an
    /// integer, and handing back what nvim handed out works for both.
    fn buffer(&self, path: &Path) -> Result<Option<Value>> {
        let name = canonical(path).to_string_lossy().into_owned();
        let handle = self.bridge_call("buffer_of", vec![Value::from(name)])?;
        Ok((!handle.is_nil()).then_some(handle))
    }

    /// A call that has to be answered, with the session's timeout.
    fn request(&self, method: &str, args: Vec<Value>) -> Result<Value> {
        if self.exited.is_some() {
            return Err(anyhow!("nvim is not running"));
        }
        self.rpc.request(method, args, CALL)
    }

    /// A function of `bridge.lua`, called with `args`. Its errors come back as
    /// the text the script raised, without nvim's `Lua:` prefix and traceback.
    fn bridge_call(&self, function: &str, args: Vec<Value>) -> Result<Value> {
        self.request(
            "nvim_exec_lua",
            vec![
                Value::from(format!("return require('emeraldian').{function}(...)")),
                Value::Array(args),
            ],
        )
        .map_err(|err| {
            let text = err.to_string();
            let text = text.strip_prefix("Lua: ").unwrap_or(&text);
            let text = text.split("\nstack traceback").next().unwrap_or(text);
            anyhow!("{}", text.trim())
        })
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

/// Load the bridge script into nvim. It needs the channel nvim knows this
/// client by, to send its notifications to.
fn inject_bridge(rpc: &Rpc, vault_root: &Path, auto_save: bool) -> Result<()> {
    let info = rpc.request("nvim_get_api_info", Vec::new(), CALL)?;
    let channel = info
        .as_array()
        .and_then(|info| info.first())
        .cloned()
        .ok_or_else(|| anyhow!("nvim_get_api_info answered {info}"))?;
    // Buffer names are matched against this, and nvim names buffers by the
    // resolved path (see `canonical`).
    let args = bridge::inject_args(channel, &canonical(vault_root), auto_save);
    rpc.request("nvim_exec_lua", vec![Value::from(bridge::LUA), args], CALL)?;
    Ok(())
}

/// The path as nvim knows it: resolved, so that `/tmp` and a symlinked `/tmp`
/// are one buffer and not two. A file that is gone (a note that was just moved)
/// is resolved through its folder.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// Whether `version` (`"0.12"`: major, minor) is one the embedding works with.
fn version_supported(version: &str) -> bool {
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 10),
        _ => false,
    }
}

/// Refuses an nvim older than 0.10, which the grid events and the bridge
/// rely on. Before 0.9 there is no `vim.version()` to ask, and that counts.
fn require_supported_nvim(rpc: &Rpc) -> Result<()> {
    let version = rpc
        .request(
            "nvim_exec_lua",
            vec![
                Value::from("local v = vim.version(); return v.major .. '.' .. v.minor"),
                Value::Array(Vec::new()),
            ],
            CALL,
        )
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    if version_supported(&version) {
        Ok(())
    } else {
        let found = if version.is_empty() {
            "older"
        } else {
            &version
        };
        Err(anyhow!("nvim {found} is too old; 0.10 or newer is needed"))
    }
}

#[cfg(test)]
mod tests {
    use super::version_supported;

    #[test]
    fn nvim_before_0_10_is_not_supported() {
        for old in ["0.9", "0.5", "0.9.5", "garbage", ""] {
            assert!(!version_supported(old), "{old}");
        }
        for fine in ["0.10", "0.12", "0.10.1", "1.0", "1.2"] {
            assert!(version_supported(fine), "{fine}");
        }
    }
}
