# Embedded Neovim as the note editor — design

Status: approved by Malte 2026-10-24 (key policy A, leader = Space). Fork of emeraldian v0.8.0, branch `nvim-embed`
in `~/Projects/emeraldian`.

## Intent

emeraldian's vim mode is a 3600-line re-implementation (`crates/emeraldian/src/vim.rs`
over `editor.rs`). It covers the common motions but knows nothing about links, plugins or
the user's muscle memory beyond the basics. Malte uses Neovim 0.12 with LazyVim daily and
wants *that* editor inside emeraldian's note pane — not a separate nvim window, and not the
emulation. The rest of emeraldian (explorer, backlinks, outline, graph, assistant, tabs,
history, reading view) stays as it is and keeps owning navigation.

Success: `notes` opens straight into an editable note inside a real, embedded nvim that
feels like Malte's nvim (keys, theme, render-markdown), and following a `[[link]]` from
nvim opens it the emeraldian way (tab, history, backlinks update).

## Goals

- Editing mode of the note pane is rendered by an embedded `nvim --embed` instance
  (msgpack-RPC, `ext_linegrid` UI), drawn by emeraldian into the pane.
- A slim, notes-tuned nvim profile (separate `NVIM_APPNAME`) derived from Malte's config.
- Link following from nvim (`<CR>`, `gf`, `<C-]>` on `[[wikilink]]`, `[[note|alias]]`,
  `[[note#heading]]`, `[text](note.md)`) goes through emeraldian's `Action::FollowLink`.
- Writes from nvim update emeraldian's index (backlinks, graph, tags) immediately.
- `notes` starts in editing mode (= nvim) by default.

## Non-goals

- Reading view stays emeraldian's renderer (images, callouts, math). No nvim there.
- No `ext_multigrid` / per-window grids; nvim composes splits, floats, popup menu and
  cmdline into one grid. Splits inside nvim are allowed but not integrated.
- No image rendering inside the nvim pane.
- The built-in editor and its vim emulation are not removed; they are the fallback.

## Anti-goals (failure even if it "works")

- Opening a separate terminal/nvim process window, or suspending the TUI to run nvim.
- Losing edits: any path where a buffer change is silently discarded (tab close, quit,
  external change, agent write, crash of nvim).
- Typing latency noticeably worse than nvim in kitty.
- emeraldian shortcuts silently stealing keys nvim users rely on (`<C-o>`, `<C-r>`,
  `<C-w>`, `<C-d>`, `<C-n>/<C-p>`, …).

## Constraints

- emeraldian's main loop is synchronous: `event::poll(TICK)` + `event::read()` in
  `main.rs:run`; no async runtime. The nvim client must fit that: a reader thread decodes
  msgpack from nvim's stdout into an `mpsc` channel, the loop drains it every tick and
  redraws when a `flush` arrived. Do not pull in tokio (`nvim-rs`) for this.
- Rendering is ratatui 0.30 / crossterm 0.29; the pane is a `Rect` in `ui/note.rs`
  (`draw_editing`). The nvim grid is written into the ratatui `Buffer` cell by cell.
- Pure Rust build stays (release profile, no C deps). New deps limited to a msgpack crate
  (`rmpv` or `rmp` + `rmp-serde`).
- Requires `nvim` ≥ 0.10 on PATH (Malte: 0.12.6). Missing/old nvim → fallback to the
  built-in editor with a status message, never a crash.
- Stays upstreamable: everything behind `[editor] backend = "nvim"`; upstream default
  remains `"builtin"`.

## Approaches considered

1. **`nvim --embed` as a UI client (picked).** emeraldian attaches with `nvim_ui_attach`
   (`ext_linegrid = true`, `rgb = true`) and draws the grid itself. Native colours via
   `hl_attr_define`, exact cursor/mode info, resize via `nvim_ui_try_resize`, no terminal
   emulation layer. Most work (redraw protocol), most elegant and robust.
2. nvim in a PTY parsed by a vt100 crate (`tui-term`/`vt100`). Faster to build, but
   inherits terminal-emulation quirks (true colour, kitty keyboard protocol, cursor shape,
   resizes) and still needs an RPC socket for integration.
3. Extend the built-in vim emulation (`gf`, link selection). Cheapest, but not Malte's nvim.

Malte picked 1, editing mode only, editing as the default start mode.

## Design

### Components (new module `crates/emeraldian/src/nvim/`)

- `rpc.rs` — spawns `nvim --embed --cmd <bootstrap>` with `NVIM_APPNAME` from config,
  owns stdin writer + reader thread, request/response matching by msgid, notifications to
  a channel. Synchronous `request()` with timeout for the few calls that need a reply
  (`nvim_get_api_info`, `nvim_buf_get_lines`, `nvim_get_option_value`).
- `grid.rs` — model of grid 1: cells (text + hl id), hl table, default colours, cursor,
  mode info. Applies `grid_resize`, `grid_line` (with repeat + hl carry-over),
  `grid_clear`, `grid_cursor_goto`, `grid_scroll`, `hl_attr_define`,
  `default_colors_set`, `mode_info_set`, `mode_change`, `busy_start/stop`, `flush`.
  Pure data, unit-tested against recorded redraw batches.
- `render.rs` — paints the grid into the pane `Rect`; wide chars (empty continuation
  cell), reverse/bold/italic/underline/undercurl/strikethrough, cursor shape per mode
  (block/bar/underline via crossterm `SetCursorStyle`) when the pane has focus.
- `input.rs` — crossterm `KeyEvent` → nvim key notation (`<C-x>`, `<M-x>`, `<S-Tab>`,
  `<lt>`, F-keys, keypad, kitty-protocol extras) → `nvim_input`; mouse → `nvim_input_mouse`
  (pane-relative coordinates); paste (bracketed) → `nvim_paste`.
- `bridge.lua` — integration Lua that emeraldian injects at startup via `nvim_exec_lua`,
  independent of the user's profile:
  - `follow()`: resolves the link under the cursor (wikilink incl. alias/heading/block
    ref, markdown link to a local `.md`); if found → `rpcnotify(chan, "emeraldian.follow",
    target)`, else falls back to the key's normal meaning. Mapped buffer-locally for
    markdown buffers in the vault on `<CR>` (normal mode), `gf`, `<C-]>`.
  - autocmds → notifications: `BufWritePost` (`written`, path), `BufModifiedSet`
    (`modified`, path, bool), `ModeChanged` (for the status bar).
  - `:Emeraldian <action>` user command → `rpcnotify(chan, "emeraldian.action", name)`
    so nvim mappings can drive app actions (switcher, search, graph, chat, sidebars…).
  - autosave when `[editor] auto_save` is on: `InsertLeave`/`TextChanged` debounced
    (~1 s) `silent! update`, plus on `FocusLost`/buffer leave.

### Lifecycle and buffers

- One nvim instance per emeraldian process, started lazily on first edit (or at start
  when the start mode is editing). Working dir = vault root.
- Each emeraldian tab in editing mode maps to one nvim buffer (`:edit <abs path>`); the
  active tab's buffer is shown with `:buffer`. Tabs keep `Tab.editor = None` in nvim mode;
  `Tab::is_modified` reads the per-path `modified` flag kept from notifications.
- `Ctrl+E` (back to reading) and tab switches first `:update` the buffer when auto_save is
  on, otherwise keep it modified (as today). Closing a tab with a modified buffer asks,
  like today; on confirm → `:bwipeout!` or `:write`.
- Quit: with auto_save `:wall`, otherwise the existing "unsaved changes" prompt; then
  `:qall!` and wait for exit (timeout → kill).
- nvim dies unexpectedly → status error, pane falls back to the reading view, the next
  edit restarts nvim; swap files (`'swapfile'` on in the profile) make recovery possible.

### Keeping emeraldian in sync

- `written` → re-index that note (`emeraldian-core` index update path used by Save today),
  refresh backlinks/graph/outline.
- External writes (assistant tools with `allow_writes`, Obsidian, rename rewriting links):
  after emeraldian or its agent writes a file, it calls `checktime` in nvim; the profile sets
  `autoread`. Before an assistant turn that may write, emeraldian runs `:wall` (auto_save)
  so the agent never works on stale disk content. A rename of the open note → `:saveas`/
  `:file` + `:bwipeout` of the old path, done by emeraldian through RPC.
- `include_active_note` for the assistant reads the buffer via `nvim_buf_get_lines`, not the
  file, so unsaved edits are seen.

### Keys while the nvim pane has focus

Everything goes to nvim, except a small reserved set emeraldian keeps:
`Ctrl+Q` (quit), `Ctrl+E` (back to reading), F-keys emeraldian uses (F1 help, F2 rename,
F3 close tab, F5 reload, F8 focus), `Alt+←/→` (history), `Ctrl+Tab`/`Ctrl+PgUp/PgDn` (tabs).
`[keys]` remaps are honoured inside the nvim pane only if they use `Alt` or an F-key;
plain `Ctrl` remaps go to nvim there. All other app
actions are reached from nvim through `:Emeraldian <action>` mappings in the profile.
`mapleader` is **Space** (Malte's decision). Mappings: `<leader>o` switcher, `<leader>/` vault search, `<leader>g` / `<leader>G`
graph / local graph, `<leader>a` assistant, `<leader>e` explorer focus, `<leader>b`
backlinks). `Ctrl+O`, `Ctrl+R`, `Ctrl+W`, `Ctrl+D`, `Ctrl+N/P`, `Ctrl+]` therefore keep
their nvim meaning. `Tab` goes to nvim; pane switching from nvim via `<C-w>h`/`<C-w>l`
when there is no nvim window in that direction (bridge falls through to
`emeraldian.action focus_left/right`).

### The notes nvim profile

- `NVIM_APPNAME=nvim-notes` → `~/.config/nvim-notes`, managed in chezmoi (`~/dotfiles`),
  not in the fork. emeraldian config: `[editor] backend = "nvim"`, `nvim = "nvim"`,
  `nvim_appname = "nvim-notes"`.
- lazy.nvim, no LazyVim distro import: Malte's options/keymaps that matter for prose,
  his Omarchy theme + `omarchy-theme-hotreload` (so the pane matches the desktop theme),
  `render-markdown.nvim` (his current spec), treesitter with markdown/markdown_inline/lua
  only, `blink.cmp` with a `[[` completion source fed by emeraldian (bridge request
  `emeraldian.notes` → note titles/aliases). No LSP, Mason, formatters, DAP, git UI,
  file explorer, dashboard, statusline (emeraldian draws its own status bar), no
  `cmdheight=0` surprises (cmdline must stay visible in the grid).
- Profile options: `wrap`, `linebreak`, `conceallevel=2`, `spell` optional, `autoread`,
  `swapfile`, `laststatus=0`, `showtabline=0`, `termguicolors`.

### Start mode

`notes` starts in editing mode: Malte's config sets `[ui] reading_mode = false` (existing
option, `app.rs:702`) and `[editor] backend = "nvim"`. With no note given, emeraldian keeps
its start screen; the first note opened goes straight into nvim. (Builder may add a
`--edit` flag; not required.)

## Testing

- Unit: `grid.rs` against recorded redraw batches (resize, scroll regions, wide chars,
  hl carry-over); `input.rs` key translation table; link parsing in `bridge.lua` via
  `nvim --headless -l` tests.
- Integration (skipped when `nvim` is missing): spawn `nvim --clean --embed`, attach a
  40×10 UI, type `ihello<Esc>`, assert grid text and cursor; `:w` → `written`
  notification; `follow()` on `[[Alpha]]` → `emeraldian.follow` notification.
- Live: tmux + kitty remote-control runs like the ones used to diagnose 0.8.0 (open note,
  `ciw`, `:w`, follow link, back with `Alt+←`, assistant reads unsaved edit), plus a latency
  check (keystroke → flush round trip) against plain nvim.

## Decided by the builder / Hermes (marked as calls, not Malte's decisions)

- msgpack crate choice, module layout, thread model details.
- Exact bridge Lua and the proposed `<leader>` mappings (Malte can veto).
- Profile location in chezmoi as `~/.config/nvim-notes` and its plugin list above.
- Keeping the 0.8.0 tty-probe race fix in the `notes` wrapper until it is fixed in the
  fork's `main.rs` (fixing it in the fork is in scope as a small separate commit).

## Calls made after the builder check (Hermes, vetoable)

- **Navigation ownership:** emeraldian tabs stay authoritative, nvim follows. A native
  `:edit`/`:bnext`/`gf` to another vault note (`BufEnter` notification) opens or activates
  the matching emeraldian tab, so history, backlinks and assistant context stay right. With
  splits, the focused nvim window's buffer is the active tab. `:q`/`:wq` on the last
  window is intercepted by the bridge (`QuitPre` + cmdline abbreviation) and means "leave
  editing" (back to reading), not "kill nvim". An nvim exit emeraldian did not request is a
  crash.
- **Dirty buffer conflicts:** assistant write tools refuse to touch a note whose buffer is
  modified ("note has unsaved edits in the editor") and tell the model to retry after a
  save. With auto_save the window for this is about 1 s. External changes to a dirty buffer
  keep nvim's own W12 prompt. A rename that rewrites links in a dirty note writes that
  buffer through nvim (`nvim_buf_set_lines` + `:update`) instead of the file.
- **Crash recovery:** swap files plus auto_save are enough. No separate journal. After an
  nvim crash, reopening the note shows nvim's normal swap-recovery prompt.
- **Link targets:** `FollowLink` gets a structured target (path, optional heading or
  block anchor, source note). Relative Markdown links resolve against the source note's
  folder. After opening, the anchor jumps through nvim (`search` for the heading or
  `^block`).
- **Latency:** the event loop may change. While nvim is active it waits on whichever comes
  first, terminal input or an nvim `flush` (the reader thread wakes the loop; crossterm
  poll with a short timeout of about 4 ms while nvim is busy). The 33 ms tick stays for
  everything else.

## Open

- Key policy: decided, option A (reserved set + `<leader>` = Space mappings via
  `:Emeraldian`).
- Whether to upstream: later, Malte's decision.
