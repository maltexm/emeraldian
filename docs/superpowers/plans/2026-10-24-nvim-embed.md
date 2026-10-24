# Embedded Neovim Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Also load the `emeraldian` skill (setup, pitfalls, live-test recipe).

**Goal:** emeraldian's edit mode runs inside an embedded `nvim --embed` instance with a slim `nvim-notes` profile, link following and index sync, and `notes` starts in it.

**Architecture:** New module `crates/emeraldian/src/nvim/` holds a synchronous msgpack-RPC client (reader thread → `mpsc`), a pure grid model fed by `ext_linegrid` redraw events, a renderer into the note pane, an input translator and an injected `bridge.lua`. `main.rs` polls it like `agent::poll`. Everything is behind `[editor] backend = "nvim"`; `"builtin"` stays the default and the fallback.

**Tech Stack:** Rust (workspace, edition as in `Cargo.toml`), ratatui 0.30, crossterm 0.29, `rmpv` (msgpack values), Neovim ≥ 0.10 (local 0.12.6), Lua for the bridge and the profile, lazy.nvim for the profile.

**Spec:** `docs/superpowers/specs/2026-10-24-nvim-embed-design.md` (read it first; its "Calls made after the builder check" section is binding).

## Global Constraints

- No async runtime: no tokio, no `nvim-rs`. One reader thread per nvim process, channel drained from the main loop.
- Only new crate dependency: `rmpv` (plus what it pulls). Release build stays pure Rust.
- `[editor] backend = "builtin" | "nvim"` (default `"builtin"`), `[editor] nvim = "nvim"`, `[editor] nvim_appname = ""` (empty = nvim's default config). Missing nvim or `nvim --version` < 0.10 → status error + builtin editor, never a panic.
- Key policy A: in the nvim pane, emeraldian keeps only `Ctrl+Q`, `Ctrl+E`, F1, F2, F3, F5, F8, `Alt+←/→`, `Ctrl+Tab`/`Ctrl+Shift+Tab`, `Ctrl+PgUp/PgDn`; `[keys]` remaps apply there only if they use Alt or an F-key. Everything else goes to nvim.
- Leader in the profile is Space (`vim.g.mapleader = " "`).
- Never lose edits (spec anti-goals). Never write `~/Vault` in tests: use temp vaults.
- Do not push, do not touch upstream remotes. Commit on the task branch only.
- Resource hygiene on the ThinkPad: `nice -n10`, `CARGO_BUILD_JOBS=4`, `--test-threads=2` for full suites.

## Review Focus

1. Multibyte and wide text (umlauts, emoji, CJK) in `grid_line` and in input: cells must line up and the cursor must land on the right column. Pinned in Task 3 (`grid_wide_chars_take_two_cells`) and Task 4 (`input_umlaut_and_emoji_pass_through`).
2. Pane resize while nvim has pending input or a float open: no stale cells, no panic on a grid shrinking below the cursor. Pinned in Task 3 (`grid_resize_smaller_clamps_cursor`) and Task 5 (live resize check).
3. nvim exiting or crashing mid-session (`:qa!` typed via `:Emeraldian`-free path, kill -9): pane falls back, no hang on the RPC channel, next edit restarts. Pinned in Task 2 (`rpc_child_exit_closes_channel`) and Task 7 (`nvim_crash_falls_back_to_reading`).
4. Typing `<`, `\`, `|`, and Ctrl combos that map to the same byte (Ctrl+I/Tab, Ctrl+M/Enter, Ctrl+[/Esc): exact nvim notation. Pinned in Task 4 table test.
5. Assistant writes while a buffer is dirty, and a rename of the open note: no silent overwrite either way. Pinned in Task 7 (`agent_write_refuses_dirty_buffer`, `rename_open_note_moves_buffer`).

---

### Task 1: Fork hygiene — tty-probe race fix and editor backend config

**Files:**
- Modify: `crates/emeraldian/src/main.rs:303-360` (startup), `crates/emeraldian/src/images.rs:145-170`
- Modify: `crates/emeraldian/src/config.rs` (`EditorConfig`)
- Test: inline `#[cfg(test)]` in `config.rs`

**Interfaces:**
- Produces: `config::EditorBackend { Builtin, Nvim }`, fields `EditorConfig.backend: EditorBackend`, `EditorConfig.nvim: String`, `EditorConfig.nvim_appname: String` (serde defaults `Builtin`, `"nvim"`, `""`; unknown backend string → `Builtin` plus a status error at startup, same style as the `images.protocol` warning).

- [ ] Step 1: Failing tests `config_backend_defaults_to_builtin`, `config_backend_parses_nvim`, `config_unknown_backend_falls_back` (toml strings in, struct values out).
- [ ] Step 2: Implement the fields; run `cargo test -p emeraldian config_` → PASS.
- [ ] Step 3: Fix the race: after `ratatui::init`/raw mode is entered, re-assert raw mode if a late `Picker::from_query_stdio` thread restored cooked termios. Approach left to you (e.g. re-enable raw mode once the probe's 2 s window has passed and whenever `tcgetattr` shows `ICANON` during the first 3 s); no change to image detection results.
- [ ] Step 4: Verify in tmux (recipe in the `emeraldian` skill) with the plain binary, *not* the `notes` wrapper: `tmux new-session -d -s t -x 160 -y 45 './target/release/emeraldian $TMPDIR/v'`, wait 4 s, `stty -a -F <pty>` shows `-icanon`, `Ctrl+O` opens the switcher. Record the output in the commit message body.
- [ ] Step 5: Commit `fix: keep raw mode after a late image-protocol reply; add editor backend config`.

### Task 2: msgpack-RPC client

**Files:**
- Create: `crates/emeraldian/src/nvim/mod.rs`, `crates/emeraldian/src/nvim/rpc.rs`
- Modify: `Cargo.toml` (workspace dep `rmpv = "1"`), `crates/emeraldian/Cargo.toml`, `crates/emeraldian/src/main.rs` (`mod nvim;`)
- Test: `crates/emeraldian/tests/nvim_rpc.rs` (integration; each test returns early with a printed skip when `nvim` is not on PATH)

**Interfaces:**
- Produces:
  - `pub struct Rpc` with `Rpc::spawn(cmd: &str, appname: &str, cwd: &Path, extra_args: &[&str]) -> anyhow::Result<Rpc>` (adds `--embed`; sets `NVIM_APPNAME` only when non-empty)
  - `fn request(&self, method: &str, args: Vec<rmpv::Value>, timeout: Duration) -> anyhow::Result<rmpv::Value>` (RPC error → `Err` with nvim's message)
  - `fn notify(&self, method: &str, args: Vec<rmpv::Value>) -> anyhow::Result<()>`
  - `fn try_recv(&self) -> Option<Incoming>`; `pub enum Incoming { Notification { method: String, params: Vec<rmpv::Value> }, Request { id: u64, method: String, params: Vec<rmpv::Value> }, Exited(Option<i32>) }`
  - `fn respond(&self, id: u64, result: Result<rmpv::Value, String>) -> anyhow::Result<()>`
  - `fn waker(&self) -> std::sync::mpsc::Receiver<()>`-style or equivalent hook so the main loop can shorten its poll while traffic is pending (Task 5 consumes it; document the exact type you choose in `mod.rs`).
  - `fn shutdown(self, timeout: Duration)` (`nvim_command("qall!")`, wait, then kill).

- [ ] Step 1: Failing tests `rpc_eval_returns_value` (`nvim_eval("1+41")` → 42 with `nvim --clean`), `rpc_error_is_err` (`nvim_command("bogus")` → Err containing `E492`), `rpc_notification_arrives` (`nvim_exec_lua("vim.rpcnotify(<channel id from nvim_get_api_info>, 'ping', 7)")` → `Incoming::Notification{method:"ping", params:[7]}`), `rpc_request_from_nvim_is_answered` (nvim `rpcrequest(1,'ask')` via `nvim_exec_lua` in a timer; test answers `"pong"`; nvim gets it), `rpc_child_exit_closes_channel` (`qall!` → `Incoming::Exited(Some(0))` within 2 s, later `request` → Err, no hang).
- [ ] Step 2: Run `cargo test -p emeraldian --test nvim_rpc` → FAIL (module missing).
- [ ] Step 3: Implement. Reader thread decodes with `rmpv::decode::read_value` from child stdout; responses routed by msgid to waiting `request` via per-call oneshot channels; stderr drained to a ring buffer for error messages.
- [ ] Step 4: Tests PASS; `cargo clippy -p emeraldian --all-targets -- -D warnings` clean.
- [ ] Step 5: Commit `feat(nvim): synchronous msgpack-RPC client`.

### Task 3: Grid model (ext_linegrid)

**Files:**
- Create: `crates/emeraldian/src/nvim/grid.rs`
- Test: inline tests + fixtures `crates/emeraldian/tests/fixtures/nvim/*.msgpack` recorded from a real nvim (record with a tiny helper test behind `#[ignore]`, commit the fixtures)

**Interfaces:**
- Consumes: `rmpv::Value` redraw batches from Task 2 (`Incoming::Notification{method:"redraw"}`).
- Produces: `pub struct Grid { pub width: u16, pub height: u16, pub cells: Vec<Cell>, pub cursor: (u16,u16), pub hl: HashMap<u64, HlAttr>, pub default: Colors, pub mode: ModeInfo, pub busy: bool }`, `pub struct Cell { pub text: String, pub hl: u64 }` (wide char = text in first cell, `""` in the next), `pub struct HlAttr { fg, bg, sp: Option<u32>, bold, italic, underline, undercurl, strikethrough, reverse: bool }`, `pub struct ModeInfo { pub name: String, pub cursor: CursorShape }`, `pub enum CursorShape { Block, Vertical, Horizontal }`; `impl Grid { pub fn apply(&mut self, batch: &[rmpv::Value]) -> bool /* true when a flush was seen */ }`. Only grid 1 is modelled; events for other grids are ignored.

- [ ] Step 1: Failing tests: `grid_line_repeat_and_hl_carry` (`["grid_line",[1,0,0,[["a",1],["b"],[" ",2,3]]]]` → `"ab   "` with hl 1,1,2,2,2), `grid_scroll_up_region` (rows 0-4, scroll by 2 inside cols 0-10 moves text, leaves stale rows for nvim to redraw), `grid_clear_resets_cells`, `grid_wide_chars_take_two_cells` (fixture typing `äöü 😀 漢字`), `grid_resize_smaller_clamps_cursor`, `grid_mode_change_sets_cursor_shape` (insert → Vertical), `grid_fixture_hello_matches_text` (recorded `ihello<Esc>` at 40×10 → row 0 starts with `hello`).
- [ ] Step 2: FAIL. Step 3: implement `apply`. Step 4: PASS + clippy.
- [ ] Step 5: Commit `feat(nvim): ext_linegrid grid model`.

### Task 4: Input translation

**Files:**
- Create: `crates/emeraldian/src/nvim/input.rs`
- Test: inline

**Interfaces:**
- Produces: `pub fn key_to_nvim(key: &crossterm::event::KeyEvent) -> Option<String>`, `pub fn mouse_to_nvim(ev: &crossterm::event::MouseEvent, origin: (u16,u16)) -> Option<MouseInput>` with `pub struct MouseInput { pub button: &'static str, pub action: &'static str, pub modifier: String, pub row: i64, pub col: i64 }` (args for `nvim_input_mouse`), `pub fn is_reserved(key: &KeyEvent, keymap: &crate::keymap::Keymap) -> bool` implementing key policy A.

- [ ] Step 1: Failing table test `key_notation_table`: `a`→`a`, `<`→`<lt>`, `\`→`<Bslash>`, `|`→`<Bar>`, Ctrl+a→`<C-a>`, Alt+x→`<M-x>`, Shift+Tab→`<S-Tab>`, Ctrl+Shift+x→`<C-S-x>`, F12→`<F12>`, Esc→`<Esc>`, Enter→`<CR>`, Backspace→`<BS>`, Ctrl+i (kitty protocol, distinct from Tab)→`<C-i>`, Tab→`<Tab>`, arrows→`<Up>`…, Home/End/PageUp→`<Home>`/`<End>`/`<PageUp>`, Release events → `None`. Test `input_umlaut_and_emoji_pass_through` (`ä`, `😀`). Test `reserved_keys_policy_a` (Ctrl+Q/Ctrl+E/F1/F2/F3/F5/F8/Alt+Left/Ctrl+Tab reserved; Ctrl+O/Ctrl+R/Ctrl+W/Ctrl+D/Ctrl+N/Ctrl+P/Ctrl+]/Tab/Space not; a `[keys]` remap `alt+e` reserved, `ctrl+y` not). Test `mouse_left_click_relative` (click at pane origin+ (3,2) → row 2 col 3, `left`/`press`).
- [ ] Step 2-4: FAIL → implement → PASS + clippy.
- [ ] Step 5: Commit `feat(nvim): key and mouse translation, key policy A`.

### Task 5: Session, rendering and main-loop wiring (first usable nvim pane)

**Files:**
- Create: `crates/emeraldian/src/nvim/session.rs`, `crates/emeraldian/src/nvim/render.rs`
- Modify: `crates/emeraldian/src/app.rs` (`App.nvim: Option<nvim::Session>`, tab ↔ buffer map), `crates/emeraldian/src/ui/note.rs:181` (`Mode::Editing` → `render::draw` when backend is nvim), `crates/emeraldian/src/keys.rs:45` (route to nvim when Focus::Note + editing + nvim backend, after `is_reserved`), `crates/emeraldian/src/main.rs:496-520` (poll, wakeup, cursor style, paste, mouse)
- Test: `crates/emeraldian/tests/nvim_session.rs`

**Interfaces:**
- Consumes: `Rpc` (Task 2), `Grid::apply` (Task 3), `key_to_nvim`/`mouse_to_nvim`/`is_reserved` (Task 4), `EditorConfig` (Task 1).
- Produces: `pub struct Session` with `Session::start(cfg: &EditorConfig, vault_root: &Path, size: (u16,u16)) -> anyhow::Result<Session>` (spawn, `nvim_ui_attach(w,h,{ext_linegrid:true, rgb:true})`, inject bridge — empty stub until Task 6), `fn open(&mut self, path: &Path) -> anyhow::Result<()>` (`:edit` or `:buffer`), `fn resize(&mut self, w: u16, h: u16)`, `fn input(&mut self, keys: &str)`, `fn mouse(&mut self, m: MouseInput)`, `fn paste(&mut self, text: &str)`, `fn poll(&mut self) -> Vec<Event>` (drains RPC, applies redraw, returns bridge events for Task 6; `pub enum Event { Redraw, Exited(Option<i32>), Bridge(BridgeEvent) }` with `BridgeEvent` defined here as an empty-for-now enum Task 6 fills), `fn grid(&self) -> &Grid`, `fn is_modified(&self, path: &Path) -> bool`, `fn buffer_text(&self, path: &Path) -> Option<String>`, `fn save_all(&mut self) -> anyhow::Result<()>`, `fn shutdown(self)`. `pub fn nvim::poll(app: &mut App) -> bool` (main-loop hook, like `agent::poll`).
- `render::draw(frame, area: Rect, grid: &Grid, palette: &Palette, focused: bool)`; colours: `rgb` from nvim, `default_colors_set` for unset fg/bg.

- [ ] Step 1: Failing integration tests (skip without nvim): `session_types_and_writes` (temp vault, open `a.md`, input `ihello<Esc>:w<CR>`, poll until flush, file content starts with `hello`, grid row 0 starts with `hello`), `session_resize_reflows` (resize 40→20 cols, grid width 20 after flush), `session_two_buffers_switch` (open a.md, b.md, open a.md again → grid shows a.md text).
- [ ] Step 2: FAIL. Step 3: implement Session + render + wiring. Start mode: when `reading_mode = false` and backend nvim, opening a note enters nvim directly. Wakeup: while nvim traffic is pending, `event::poll` timeout ≤ 4 ms; otherwise TICK.
- [ ] Step 4: Tests PASS, clippy clean, and live check in tmux + kitty (skill recipe) with a temp vault and `[editor] backend="nvim"`, `nvim_appname=""` (Malte's normal nvim): type, `ciw`, `:w`, resize the tmux window, cursor shape changes Normal↔Insert. Note the observed keystroke→flush latency (timestamp nvim_input vs flush in a debug log) in the commit body; target < 10 ms median.
- [ ] Step 5: Commit `feat(nvim): embedded nvim session drawn in the note pane`.

### Task 6: bridge.lua — links, sync, app actions, autosave

**Files:**
- Create: `crates/emeraldian/src/nvim/bridge.lua` (embedded with `include_str!`), `crates/emeraldian/src/nvim/bridge.rs` (Rust side)
- Modify: `crates/emeraldian/src/app.rs:234` (`Action::FollowLink(String)` → `Action::FollowLink(LinkTarget)`; `open_or_create` takes it), `crates/emeraldian/src/actions.rs:267`, call sites in `keys.rs:702`, `keys.rs:1143`
- Test: `crates/emeraldian/tests/nvim_bridge.rs`, Lua tests `crates/emeraldian/src/nvim/bridge_spec.lua` run by `nvim --headless --clean -l` from a Rust test

**Interfaces:**
- Consumes: `Session` (Task 5).
- Produces: `pub struct LinkTarget { pub note: String, pub anchor: Option<Anchor>, pub source: Option<PathBuf> }`, `pub enum Anchor { Heading(String), Block(String) }`; `BridgeEvent::{Follow(LinkTarget), Written(PathBuf), Modified(PathBuf, bool), Entered(PathBuf), Action(String), LeaveEditing}`; nvim → emeraldian request `emeraldian.notes` (returns note titles + aliases for completion). Bridge Lua API: `require("emeraldian").follow()`, user command `:Emeraldian <action>` with actions `switcher search graph local_graph chat explorer backlinks outline focus_left focus_right new daily`.
- Lua behaviour: buffer-local maps in vault markdown buffers for `<CR>` (normal), `gf`, `<C-]>` → `follow()`, falling back to the key's default when no link is under the cursor; autocmds `BufWritePost`, `BufModifiedSet`, `BufEnter`, `QuitPre`/cmdline abbreviation for `:q`, `:wq`, `:x` on the last window → `LeaveEditing`; autosave (when emeraldian passes `auto_save=true` at inject time): debounced 1000 ms `silent! update` on `TextChanged`/`InsertLeave`, immediate on `BufLeave`/`FocusLost`.

- [ ] Step 1: Failing Lua spec cases for the link parser at a cursor column: `[[Alpha]]`, `[[Alpha|alias]]`, `[[Folder/Alpha#Heading]]`, `[[Alpha#^blk1]]`, `[text](../Beta.md)`, `[text](Beta%20Two.md)`, `[ext](https://x)` (→ no link, fallback), cursor on `]]` edge, two links on one line picks the one under the cursor.
- [ ] Step 2: Failing Rust tests: `bridge_follow_notifies` (`[[Alpha]]`, cursor on it, input `<CR>` → `BridgeEvent::Follow(note:"Alpha")`), `bridge_write_reindexes` (`:w` → `Written`, app index sees new link → backlinks of target updated), `bridge_leave_on_q` (`:q` → `LeaveEditing`, nvim still alive), `bridge_action_switcher` (`:Emeraldian switcher` → `Action("switcher")` → modal opens), `follow_relative_md_link_resolves_from_source_folder`, `follow_heading_anchor_jumps` (after open, cursor line = heading line), `bridge_entered_syncs_tab` (`:e Beta.md` in nvim → emeraldian active tab is Beta), `autosave_after_insert_leave` (file updated ≤ 1.5 s after `<Esc>`).
- [ ] Step 3: FAIL → implement → PASS + clippy.
- [ ] Step 4: Commit `feat(nvim): bridge for links, index sync, app actions and autosave`.

### Task 7: Lifecycle and safety

**Files:**
- Modify: `crates/emeraldian/src/app.rs` (`close_tab`, `save_tab`, quit flow, `Tab::is_modified` via session), `crates/emeraldian/src/actions.rs` (Quit, CloseTab, ToggleMode, RenameNote, Refresh), `crates/emeraldian/src/tools.rs:194-215` (write tools), `crates/emeraldian/src/agent.rs:241` (`include_active_note`), `crates/emeraldian/src/nvim/session.rs`
- Test: `crates/emeraldian/tests/nvim_lifecycle.rs`

**Interfaces:**
- Consumes: Task 5 `Session`, Task 6 `BridgeEvent`.
- Produces: `Session::checktime(&mut self)`, `Session::rename(&mut self, from: &Path, to: &Path)`, `Session::set_lines(&mut self, path: &Path, text: &str) -> anyhow::Result<()>` (writes a rewritten note through the buffer + `:update`).

- [ ] Step 1: Failing tests: `ctrl_e_saves_with_autosave` (edit, Ctrl+E → file written, mode Reading), `close_dirty_tab_prompts_without_autosave`, `quit_writes_all_with_autosave_and_exits_nvim` (nvim process gone ≤ 2 s), `agent_write_refuses_dirty_buffer` (dirty buffer → tool result error text `note has unsaved edits in the editor`, file unchanged), `agent_write_then_checktime_reloads_clean_buffer`, `include_active_note_reads_unsaved_buffer`, `rename_open_note_moves_buffer` (F2 rename → buffer name = new path, no stray old file), `rename_rewrites_links_in_dirty_note_via_buffer`, `nvim_crash_falls_back_to_reading` (kill -9 child → status error, mode Reading, next Ctrl+E starts a new session), `missing_nvim_falls_back_to_builtin` (`nvim = "/nonexistent"` → builtin editor + status error).
- [ ] Step 2: FAIL → implement → PASS; full `cargo test -p emeraldian -- --test-threads=2` green; clippy clean.
- [ ] Step 3: Commit `feat(nvim): lifecycle, conflict and crash safety`.

### Task 8: `nvim-notes` profile and Malte's setup

**Files (outside the fork, chezmoi-managed in `~/dotfiles`):**
- Create: `~/.config/nvim-notes/init.lua`, `~/.config/nvim-notes/lua/notes/{options,keymaps,completion}.lua`, `~/.config/nvim-notes/lua/plugins/*.lua`; add with `chezmoi add` (no commit/push of dotfiles unless Malte asks)
- Modify: `~/.config/emeraldian/config.toml` (`[editor] backend = "nvim"`, `nvim_appname = "nvim-notes"`, `[ui] reading_mode = false`), `~/.local/bin/notes` only if the binary path changes (see Step 4)

**Interfaces:**
- Consumes: `:Emeraldian <action>` and request `emeraldian.notes` (Task 6).
- Profile contents (spec "The notes nvim profile"): lazy.nvim bootstrap; `vim.g.mapleader = " "`; options `wrap linebreak conceallevel=2 autoread swapfile laststatus=0 showtabline=0 termguicolors`, `cmdheight=1`; Malte's Omarchy theme + `omarchy-theme-hotreload` (reuse his `~/.config/nvim/lua/plugins/{theme,omarchy-theme-hotreload,all-themes,theme-fallback}.lua` specs); `render-markdown.nvim` with his current spec; treesitter `markdown markdown_inline lua` only; `blink.cmp` with a custom `[[` source calling `rpcrequest(chan,'emeraldian.notes')`. Leader maps: `<leader>o` switcher, `<leader>/` search, `<leader>g` graph, `<leader>G` local_graph, `<leader>a` chat, `<leader>e` explorer, `<leader>b` backlinks, `<leader>n` new, `<leader>d` daily. No LSP, Mason, formatters, DAP, git UI, file tree, dashboard, statusline.

- [ ] Step 1: Headless check `NVIM_APPNAME=nvim-notes nvim --headless "+Lazy! sync" +qa` installs without errors; `NVIM_APPNAME=nvim-notes nvim --headless -c 'lua print(vim.g.mapleader == " ")' -c qa` prints `true`; startup time `nvim --startuptime` < 80 ms.
- [ ] Step 2: Embedded check with the fork binary against a temp vault: `[[` shows note completion, `<Space>o` opens the switcher, theme matches the current Omarchy theme.
- [ ] Step 3: Switch Malte's config to the nvim backend (values above). Re-check after any test instance ran (instances rewrite config.toml, see skill).
- [ ] Step 4: Install: `cargo build --release` then copy `target/release/emeraldian` to `~/.local/bin/emeraldian` only after Malte approves on the card (keep the 0.8.0 binary as `~/.local/bin/emeraldian-0.8.0`).
- [ ] Step 5: `chezmoi add ~/.config/nvim-notes`; commit in the fork only the docs pointer (README section "Embedded Neovim").

### Task 9: End-to-end verification, docs, skill

**Files:**
- Modify: `README.md` (section "Embedded Neovim": config keys, key policy, bridge commands), `CHANGELOG.md` (Unreleased), `~/Projects/hermes-config/skills/note-taking/emeraldian/SKILL.md` (fork usage, new pitfalls)

- [ ] Step 1: Live run with `notes` in kitty (off-screen recipe) on a temp copy of a few real notes: open → lands in nvim, edit, `<CR>` on `[[link]]` follows, `Alt+←` back, backlinks updated after `:w`, `<Space>a` opens the assistant and it sees the unsaved edit, `:q` returns to reading, `Ctrl+Q` quits with nvim gone (`pgrep -f 'nvim --embed'` empty).
- [ ] Step 2: Same in tmux (race fix from Task 1 holds without the wrapper workaround).
- [ ] Step 3: Latency: median keystroke→flush < 10 ms over 200 keys; report numbers.
- [ ] Step 4: Docs + skill updated; commit `docs: embedded Neovim editor`.
