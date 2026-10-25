//! Embedded Neovim as the note editor (`[editor] backend = "nvim"`).
//!
//! Design: `docs/superpowers/specs/2026-10-24-nvim-embed-design.md`.
//!
//! The main loop is synchronous, so there is no async runtime here: `rpc` owns
//! one reader thread per nvim process and the loop drains its channel.
//!
//! # Waking the main loop
//!
//! `Rpc::waker()` returns a [`rpc::Waker`], a shared flag the reader thread
//! raises whenever it queues a message (including `Exited`). crossterm's
//! `poll` cannot wait on a channel, so while nvim is active the loop polls
//! with a short timeout ([`NVIM_TICK`], 4 ms) and drains the session after each
//! poll. [`session::Session::poll`] takes the flag and stays "busy" for a short
//! window after any traffic, and [`poll_timeout`] is what the loop asks for the
//! timeout. The 33 ms tick stays for everything else.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use emeraldian_core::index::{NoteId, VaultIndex};
use ratatui::layout::Position;

use crate::app::{Action, App, Focus, Mode, SidePanel};

// Parts of the client and the grid model wait for the lifecycle work before
// anything outside the tests uses them.
pub mod bridge;
#[allow(dead_code)]
pub mod grid;
#[allow(dead_code)]
pub mod input;
#[cfg(test)]
mod lifecycle_tests;
pub mod render;
#[allow(dead_code)]
pub mod rpc;
#[allow(dead_code)]
pub mod session;

use bridge::{BridgeEvent, LinkTarget};
use grid::CursorShape;
use session::{Event, Session};

/// How long the main loop waits for input while nvim is busy: short enough that
/// a keystroke's redraw is on screen within a few milliseconds of nvim
/// producing it.
pub const NVIM_TICK: Duration = Duration::from_millis(4);

/// Drains the session, keeps it showing the open note at the pane's size and
/// starts it on first use. `true` when the screen needs to be redrawn.
pub fn poll(app: &mut App) -> bool {
    let editing = app.nvim_editing();
    let mut redraw = false;

    if editing && app.nvim.is_none() {
        redraw |= start(app);
    }

    let path = app
        .active_note()
        .and_then(|id| app.index.note(id))
        .map(|note| note.meta.path.clone());
    let area = app.regions.nvim;
    let Some(session) = app.nvim.as_mut() else {
        return redraw;
    };

    let mut open_error = None;
    if editing {
        if let Some(area) = area {
            session.resize(area.width, area.height);
        }
        if let Some(path) = path
            && let Err(err) = session.open(&path)
        {
            open_error = Some(err.to_string());
        }
    }
    // What `[[` completes to follows the index, which a write or a reload
    // changes.
    let revision = app.index.revision();
    if session.notes_revision() != Some(revision) {
        session.set_notes(revision, note_names(&app.index));
    }

    // Drained even while a note is being read: nvim keeps running behind the
    // reading view, and its messages must not pile up or its exit go unseen.
    let mut exit = None;
    let mut bridged = Vec::new();
    for event in session.poll() {
        match event {
            Event::Redraw => redraw |= editing,
            Event::Exited(code) => exit = Some(code),
            Event::Bridge(event) => bridged.push(event),
        }
    }

    if let Some(err) = open_error {
        app.error(format!("could not open the note in nvim: {err}"));
        redraw = true;
    }
    // After the session's borrow ends: what nvim reports changes the app.
    for event in bridged {
        redraw |= handle_bridge(app, event);
    }
    if let Some(code) = exit {
        // Not a reason to give up on nvim: the notes go back to the reading
        // view, and editing one again starts a fresh process.
        app.nvim = None;
        for tab in &mut app.tabs {
            tab.mode = Mode::Reading;
        }
        app.error(match code {
            Some(code) => format!("nvim ended (status {code}) — back to the reading view"),
            None => "nvim was killed — back to the reading view".to_string(),
        });
        redraw = true;
    }
    redraw
}

/// Acts on something nvim's bridge reported. `true` when the screen changed.
fn handle_bridge(app: &mut App, event: BridgeEvent) -> bool {
    match event {
        BridgeEvent::Follow(target) => {
            follow_link(app, target);
            true
        }
        BridgeEvent::Written(path) => reindex(app, &path),
        // A buffer going dirty or clean changes the mark on its tab.
        BridgeEvent::Modified(..) => true,
        BridgeEvent::Entered(path) => follow_nvim(app, &path),
        BridgeEvent::Action(name) => run_action(app, &name),
        BridgeEvent::LeaveEditing => leave_editing(app),
        BridgeEvent::Notice(text) => {
            app.info(text);
            true
        }
    }
}

/// A link followed in nvim. The note it leads to opens in the editor too:
/// `reading_mode` decides how a note opens from the explorer or the switcher,
/// but pressing `<CR>` on a link while editing should not throw the user out
/// of nvim into the reading view, with the jump to the heading lost.
fn follow_link(app: &mut App, target: LinkTarget) {
    let was_editing = app.nvim_editing();
    crate::actions::dispatch(app, Action::FollowLink(target.clone()));
    if !was_editing {
        return;
    }
    if let Some(tab) = app.active_mut()
        && tab.mode == Mode::Reading
    {
        tab.mode = Mode::Editing;
        // The jump was not sent while the note was only read.
        app.jump_to_anchor(&target);
    }
}

/// The note the index knows under this absolute path, whether the path was
/// spelled the way the vault is or as the file system resolves it.
fn note_for_path(app: &App, path: &Path) -> Option<NoteId> {
    let root = &app.index.vault.path;
    let rel = path
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .ok()
        .or_else(|| {
            let root = std::fs::canonicalize(root).ok()?;
            path.strip_prefix(root).map(Path::to_path_buf).ok()
        })?;
    app.index.id_of_rel(&rel.to_string_lossy())
}

/// A note was written from nvim: read it into the index again, so that
/// backlinks, the outline, tags and the graph show what is on disk. A file the
/// index does not know (a new one) is left to the next reload.
fn reindex(app: &mut App, path: &Path) -> bool {
    let Some(id) = note_for_path(app, path) else {
        return false;
    };
    match app.index.refresh_note(id) {
        Ok(()) => app.explorer.rebuild(&app.index),
        Err(err) => app.error(format!("could not read {} again: {err}", path.display())),
    }
    true
}

/// nvim moved to another note by itself (`:e`, `:bnext`, a jump back): the tabs
/// follow, so the history, the backlinks and the assistant's context are those
/// of the note on screen.
///
/// Only while a note is being edited: with the reading view up, a late report
/// of a buffer nvim left a moment ago must not drag the reading view along.
fn follow_nvim(app: &mut App, path: &Path) -> bool {
    if !app.nvim_editing() {
        return false;
    }
    let Some(id) = note_for_path(app, path) else {
        return false;
    };
    if app.active_note() == Some(id) {
        return false;
    }
    app.open_note(id);
    if let Some(tab) = app.active_mut() {
        tab.mode = Mode::Editing;
    }
    // nvim is already there: `open` must not switch to the buffer again.
    if let Some(session) = app.nvim.as_mut()
        && let Some(note) = app.index.note(id)
    {
        session.note_shown(&note.meta.path);
    }
    true
}

/// `:q`, `:wq`, `:x` on the last window: back to the reading view. nvim and its
/// buffers stay, so editing the note again picks up where it was left.
fn leave_editing(app: &mut App) -> bool {
    if !app.nvim_editing() {
        return false;
    }
    if let Some(tab) = app.active_mut() {
        tab.mode = Mode::Reading;
    }
    app.vim.reset();
    true
}

/// `:Emeraldian <action>`: the app actions nvim's mappings can reach.
fn run_action(app: &mut App, name: &str) -> bool {
    use crate::actions::dispatch;

    match name {
        "switcher" => dispatch(app, Action::OpenSwitcher),
        "search" => dispatch(app, Action::OpenSearch),
        "graph" => dispatch(app, Action::OpenGraph),
        "local_graph" => dispatch(app, Action::OpenLocalGraph),
        // A key that opens the assistant should bring it to the keyboard, not
        // close it on the second press while the focus is elsewhere.
        "chat" if app.config.ui.show_chat && app.focus != Focus::Chat => {
            dispatch(app, Action::FocusPane(Focus::Chat));
        }
        "chat" => dispatch(app, Action::ToggleChat),
        "explorer" | "focus_left" => dispatch(app, Action::FocusPane(Focus::Explorer)),
        "backlinks" => show_side_panel(app, SidePanel::Backlinks),
        "outline" => show_side_panel(app, SidePanel::Outline),
        "focus_right" if app.config.ui.show_right_sidebar => {
            dispatch(app, Action::FocusPane(Focus::Sidebar));
        }
        "focus_right" if app.config.ui.show_chat => dispatch(app, Action::FocusPane(Focus::Chat)),
        "focus_right" => app.info("no pane on the right"),
        "new" => dispatch(app, Action::NewNote),
        "daily" => dispatch(app, Action::DailyNote),
        other => app.error(format!("unknown Emeraldian action: {other}")),
    }
    true
}

/// Opens the right sidebar on `panel` and gives it the keyboard.
fn show_side_panel(app: &mut App, panel: SidePanel) {
    app.side_panel = panel;
    app.side_selected = 0;
    app.config.ui.show_right_sidebar = true;
    crate::actions::dispatch(app, Action::FocusPane(Focus::Sidebar));
}

/// The names a `[[` can be completed to: every note's file name and every
/// alias — what the index can resolve a link to — once each, sorted.
fn note_names(index: &VaultIndex) -> Vec<String> {
    let names: BTreeSet<&str> = index
        .notes()
        .iter()
        .flat_map(|note| {
            std::iter::once(note.meta.stem.as_str()).chain(note.aliases.iter().map(String::as_str))
        })
        .collect();
    names.into_iter().map(str::to_string).collect()
}

/// Starts nvim at the pane's size. Waits for the first frame, which is what
/// measures the pane. A program that cannot be started is reported once and
/// the built-in editor takes over for the rest of the run.
fn start(app: &mut App) -> bool {
    let Some(area) = app.regions.nvim else {
        return false;
    };
    let root = app.index.vault.path.clone();
    match Session::start(&app.config.editor, &root, (area.width, area.height)) {
        Ok(session) => app.nvim = Some(session),
        Err(err) => {
            app.nvim_failed = true;
            app.error(format!(
                "could not start {}: {err} — using the built-in editor",
                app.config.editor.nvim
            ));
        }
    }
    true
}

/// How long the main loop may wait for input: [`NVIM_TICK`] while nvim is
/// busy, `tick` otherwise.
#[must_use]
pub fn poll_timeout(app: &App, tick: Duration) -> Duration {
    if app.nvim.as_ref().is_some_and(Session::busy) {
        NVIM_TICK.min(tick)
    } else {
        tick
    }
}

/// Whether the keyboard, the clipboard and the caret belong to nvim right now.
///
/// Public because the key router needs the same answer: while this holds, the
/// legacy vim layers must stand down rather than act on the built-in editor
/// that nvim has replaced.
#[must_use]
pub fn pane_has_input(app: &App) -> bool {
    app.nvim_editing() && app.focus == Focus::Note && app.modal.is_none()
}

/// Sends a key to nvim when the note pane has the focus and the key is not one
/// emeraldian keeps (key policy A). `true` when the key was dealt with.
pub fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !pane_has_input(app) || input::is_reserved(&key, &app.keymap) {
        return false;
    }
    // A key nvim has no name for is dropped rather than handed on: the
    // built-in editor behind this pane would type it into a note nobody sees.
    if let Some(session) = app.nvim.as_mut()
        && let Some(keys) = input::key_to_nvim(&key)
    {
        session.input(&keys);
    }
    true
}

/// Sends a mouse event inside the nvim pane to nvim. `true` when it was.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.nvim_editing() || app.modal.is_some() {
        return false;
    }
    let Some(area) = app.regions.nvim else {
        return false;
    };
    if !area.contains(Position::new(mouse.column, mouse.row)) {
        return false;
    }
    // Hover would be a message per cell the pointer crosses, for a pane that
    // has nothing to show on hover.
    if mouse.kind == MouseEventKind::Moved {
        return false;
    }
    let Some(event) = input::mouse_to_nvim(&mouse, (area.x, area.y)) else {
        return false;
    };
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.focus = Focus::Note;
    }
    if let Some(session) = app.nvim.as_mut() {
        session.mouse(
            event.button,
            event.action,
            &event.modifier,
            event.row,
            event.col,
        );
    }
    true
}

/// Pastes into nvim when the note pane has the focus. `true` when it did.
pub fn handle_paste(app: &mut App, text: &str) -> bool {
    if !pane_has_input(app) {
        return false;
    }
    if let Some(session) = app.nvim.as_mut() {
        session.paste(text);
    }
    true
}

/// The terminal caret for nvim's current mode, while the pane has the focus.
#[must_use]
pub fn cursor_style(app: &App) -> Option<SetCursorStyle> {
    if !pane_has_input(app) {
        return None;
    }
    let session = app.nvim.as_ref()?;
    Some(match session.grid().mode.cursor {
        CursorShape::Block => SetCursorStyle::SteadyBlock,
        CursorShape::Vertical => SetCursorStyle::SteadyBar,
        CursorShape::Horizontal => SetCursorStyle::SteadyUnderScore,
    })
}

/// Before an assistant turn that may write: puts every buffer on disk, so that
/// the assistant works on what is on screen and the write tools find nothing
/// they have to refuse. Without auto-save the buffers stay as they are, and the
/// tools refuse the notes with unsaved edits.
pub fn before_agent_turn(app: &mut App) {
    if !app.config.agent.allow_writes || !app.config.editor.auto_save {
        return;
    }
    if let Some(session) = app.nvim.as_mut()
        && let Err(err) = session.save_all()
    {
        app.error(format!(
            "could not save the open notes before the assistant's turn: {err}"
        ));
    }
}

/// Ends the nvim session, after writing what auto-save asks for. Nothing is
/// ended when that fails: the session is where the unsaved edits are.
///
/// For leaving a vault, where the session cannot carry on. Without auto-save
/// the unsaved text goes with it, and its swap files too: the caller has
/// asked first ([`App::confirm_unsaved`]).
pub fn end_session(app: &mut App) -> Result<(), String> {
    if app.config.editor.auto_save
        && let Some(session) = app.nvim.as_mut()
    {
        session.save_all().map_err(|err| err.to_string())?;
    }
    if let Some(session) = app.nvim.take() {
        session.shutdown();
    }
    Ok(())
}

/// Ends the nvim process at exit, after writing what auto-save asks for.
///
/// The text that is left unsaved is lost only when the user said so: a quit
/// they confirmed (`app.quit`) without auto-save. Any other way out (the
/// terminal went away, the event loop failed) or a note that could not be
/// written leaves the swap files in place, which is how nvim gets it back.
pub fn shutdown(app: &mut App) {
    let auto_save = app.config.editor.auto_save;
    let discarded = app.quit && !auto_save;
    let Some(mut session) = app.nvim.take() else {
        return;
    };
    let saved = !auto_save || session.save_all().is_ok();
    let at_risk = if auto_save {
        !saved
    } else {
        !discarded && !session.modified_notes().is_empty()
    };
    if at_risk {
        session.leave_for_recovery();
    } else {
        session.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use emeraldian_core::test_support::TempVault;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::{Focus, Mode};
    use crate::config::{Config, EditorBackend};
    use crate::keys;
    use crate::nvim::session::Session;
    use crate::ui;

    pub(super) const LIMIT: Duration = Duration::from_secs(10);

    pub(super) fn nvim_available() -> bool {
        std::process::Command::new("nvim")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// The app over a temp vault with the nvim backend, `Alpha.md` open in it,
    /// and a clean nvim already attached (so no test reads or writes anyone's
    /// nvim config, shada or swap files). `None` without nvim.
    pub(super) fn nvim_app() -> Option<(TempVault, App)> {
        nvim_app_with(&[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")])
    }

    /// [`nvim_app`] over a vault made of `files`; `Alpha.md` must be one.
    pub(super) fn nvim_app_with(files: &[(&str, &str)]) -> Option<(TempVault, App)> {
        nvim_app_configured(files, |_| {})
    }

    /// [`nvim_app_with`] after `configure` changed the settings.
    pub(super) fn nvim_app_configured(
        files: &[(&str, &str)],
        configure: impl FnOnce(&mut Config),
    ) -> Option<(TempVault, App)> {
        if !nvim_available() {
            eprintln!("skip: nvim not on PATH");
            return None;
        }
        let vault = TempVault::new("nvim-wiring");
        for (name, text) in files {
            vault.write(name, text);
        }
        let mut config = Config::default();
        config.editor.backend = EditorBackend::Nvim;
        config.ui.reading_mode = false;
        configure(&mut config);
        let mut app = App::new(vault.vault(), config).expect("app");
        let alpha = app.index.id_of_rel("Alpha.md").expect("indexed");
        app.open_note(alpha);
        app.nvim = Some(
            Session::start_with(
                &app.config.editor,
                vault.path(),
                (40, 10),
                &["--clean", "--cmd", "set noswapfile"],
            )
            .expect("start nvim"),
        );
        Some((vault, app))
    }

    /// Draw one frame at `width` x `height`; returns the screen as text.
    pub(super) fn draw(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|frame| ui::draw(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|col| buffer.cell((col, row)).map_or("", |cell| cell.symbol()))
                    .collect::<String>()
            })
            .collect()
    }

    /// Poll (and redraw) until `done` holds, or fail after [`LIMIT`].
    pub(super) fn settle(
        app: &mut App,
        width: u16,
        height: u16,
        mut done: impl FnMut(&App) -> bool,
    ) {
        let deadline = Instant::now() + LIMIT;
        loop {
            poll(app);
            draw(app, width, height);
            if done(app) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "nvim never reached the state; row 0 is {:?}, row 1 {:?}",
                row(app, 0).trim_end(),
                row(app, 1).trim_end()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub(super) fn row(app: &App, row: usize) -> String {
        let grid = app.nvim.as_ref().expect("session").grid();
        let width = usize::from(grid.width);
        grid.cells[row * width..(row + 1) * width]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    pub(super) fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            keys::handle(app, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    #[test]
    fn nvim_pane_shows_the_active_note() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        draw(&mut app, 100, 30);

        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
        let screen = draw(&mut app, 100, 30).join("\n");
        assert!(screen.contains("# Alpha"), "screen was:\n{screen}");
    }

    #[test]
    fn nvim_pane_is_sized_to_its_area() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        draw(&mut app, 100, 30);
        let area = app.regions.nvim.expect("the nvim pane was laid out");
        settle(&mut app, 100, 30, |app| {
            let grid = app.nvim.as_ref().unwrap().grid();
            (grid.width, grid.height) == (area.width, area.height)
        });

        // A narrower terminal gives nvim a narrower screen.
        draw(&mut app, 80, 24);
        let smaller = app.regions.nvim.expect("the nvim pane was laid out");
        assert!(smaller.width < area.width);
        settle(&mut app, 80, 24, |app| {
            let grid = app.nvim.as_ref().unwrap().grid();
            (grid.width, grid.height) == (smaller.width, smaller.height)
        });
    }

    #[test]
    fn nvim_keys_reach_nvim_and_policy_a_keys_stay() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        // `Ctrl+O` opens the quick switcher everywhere else; here it is nvim's
        // jump-back, so it must not reach emeraldian.
        keys::handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        );
        assert!(app.modal.is_none(), "Ctrl+O opened a modal");

        // Plain typing is nvim's, umlauts included.
        type_text(&mut app, "iä ");
        keys::handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        settle(&mut app, 100, 30, |app| {
            row(app, 0).starts_with("ä # Alpha")
        });
        assert!(
            app.tabs[0].editor.is_none(),
            "the built-in editor stayed unused"
        );

        // `Ctrl+E` is emeraldian's: back to the reading view.
        keys::handle(
            &mut app,
            KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Reading));
    }

    /// `Ctrl+Shift+G` is the local graph in every view. In the nvim pane it
    /// used to reach nvim as `<C-S-g>`, which nothing maps, so nothing happened.
    #[test]
    fn ctrl_shift_g_opens_the_local_graph_from_the_nvim_pane() {
        use crate::app::View;

        for (ch, label) in [('g', "Ctrl+Shift+g"), ('G', "Ctrl+Shift+G")] {
            let Some((_vault, mut app)) = nvim_app() else {
                return;
            };
            app.focus = Focus::Note;
            draw(&mut app, 100, 30);
            settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

            // `Ctrl+G` alone is nvim's file info and must stay there.
            keys::handle(
                &mut app,
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            );
            assert_eq!(app.view, View::Notes, "plain Ctrl+G opened the graph");

            keys::handle(
                &mut app,
                KeyEvent::new(
                    KeyCode::Char(ch),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            );

            assert_eq!(app.view, View::Graph, "{label} did not open the graph");
            let alpha = app.index.id_of_rel("Alpha.md").expect("indexed");
            let graph = app.graph.as_ref().expect("graph built");
            assert_eq!(
                graph.local_root,
                Some(alpha),
                "{label} did not open the local graph of the open note"
            );
        }
    }

    #[test]
    fn nvim_click_moves_the_cursor() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Explorer;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
        let area = app.regions.nvim.expect("laid out");

        let handled = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 4,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );

        assert!(handled);
        assert_eq!(app.focus, Focus::Note, "a click gives the pane the focus");
        settle(&mut app, 100, 30, |app| {
            app.nvim.as_ref().unwrap().grid().cursor == (4, 0)
        });
    }

    #[test]
    fn nvim_click_outside_the_pane_is_not_nvims() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        draw(&mut app, 100, 30);
        let area = app.regions.nvim.expect("laid out");

        let handled = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x.saturating_sub(1),
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );

        assert!(!handled);
    }

    #[test]
    fn nvim_paste_arrives_as_one_block() {
        let Some((vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        type_text(&mut app, "i");
        assert!(handle_paste(&mut app, "one\ntwo "));

        settle(&mut app, 100, 30, |app| {
            row(app, 1).starts_with("two # Alpha")
        });
        assert!(row(&app, 0).starts_with("one"));
        let path = vault.path().join("Alpha.md");
        let text = app.nvim.as_ref().unwrap().buffer_text(&path);
        assert_eq!(text.as_deref(), Some("one\ntwo # Alpha\n\nbody\n"));
    }

    #[test]
    fn nvim_caret_follows_the_mode() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        assert_eq!(cursor_style(&app), Some(SetCursorStyle::SteadyBlock));

        type_text(&mut app, "i");
        settle(&mut app, 100, 30, |app| {
            cursor_style(app) == Some(SetCursorStyle::SteadyBar)
        });

        keys::handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        settle(&mut app, 100, 30, |app| {
            cursor_style(app) == Some(SetCursorStyle::SteadyBlock)
        });

        // Without the focus the pane leaves the terminal's caret alone.
        app.focus = Focus::Explorer;
        assert_eq!(cursor_style(&app), None);
    }

    #[test]
    fn nvim_switching_tabs_switches_buffers() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        let beta = app.index.id_of_rel("Beta.md").expect("indexed");
        app.open_note(beta);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Beta"));

        app.cycle_tab(-1);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
    }

    #[test]
    fn nvim_busy_loop_polls_fast_and_idle_loop_does_not() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        let tick = Duration::from_millis(33);
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| !app.nvim.as_ref().unwrap().busy());
        assert_eq!(poll_timeout(&app, tick), tick);

        app.focus = Focus::Note;
        type_text(&mut app, "i");
        assert_eq!(poll_timeout(&app, tick), NVIM_TICK);
    }

    #[test]
    fn nvim_exit_returns_to_reading_and_a_new_edit_restarts() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        type_text(&mut app, ":qa!");
        keys::handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        settle(&mut app, 100, 30, |app| app.nvim.is_none());

        assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Reading));
        assert!(app.status.is_error, "status was {:?}", app.status.text);
        assert!(
            !app.nvim_failed,
            "an exit is not a reason to give up on nvim"
        );
    }

    #[test]
    fn nvim_missing_program_falls_back_to_the_builtin_editor() {
        let vault = TempVault::new("nvim-missing");
        vault.write("Alpha.md", "# Alpha\n");
        let mut config = Config::default();
        config.editor.backend = EditorBackend::Nvim;
        config.editor.nvim = "/nonexistent/nvim".to_string();
        config.ui.reading_mode = false;
        let mut app = App::new(vault.vault(), config).expect("app");
        let alpha = app.index.id_of_rel("Alpha.md").expect("indexed");
        app.open_note(alpha);

        draw(&mut app, 100, 30);
        poll(&mut app);

        assert!(app.nvim.is_none());
        assert!(app.nvim_failed);
        assert!(app.status.is_error, "status was {:?}", app.status.text);
        assert!(!app.nvim_editing(), "the built-in editor takes over");
        // And typing reaches it.
        app.focus = Focus::Note;
        type_text(&mut app, "x");
        assert!(app.tabs[0].is_modified());
    }

    #[test]
    fn shutdown_saves_with_auto_save_and_ends_nvim() {
        let Some((vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
        type_text(&mut app, "ihi ");
        keys::handle(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        settle(&mut app, 100, 30, |app| {
            row(app, 0).starts_with("hi # Alpha")
        });

        shutdown(&mut app);

        assert!(app.nvim.is_none());
        let text = std::fs::read_to_string(vault.path().join("Alpha.md")).expect("read note");
        assert!(text.starts_with("hi # Alpha"), "file was {text:?}");
    }

    /// The title of the note the active tab shows.
    pub(super) fn active_title(app: &App) -> String {
        app.active_note()
            .map(|id| app.note_title(id))
            .unwrap_or_default()
    }

    pub(super) fn press(app: &mut App, code: KeyCode) {
        keys::handle(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn bridge_action_switcher() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
        assert!(app.modal.is_none());

        type_text(&mut app, ":Emeraldian switcher");
        press(&mut app, KeyCode::Enter);

        settle(&mut app, 100, 30, |app| app.modal.is_some());
    }

    #[test]
    fn bridge_write_reindexes() {
        let Some((vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
        let beta = app.index.id_of_rel("Beta.md").expect("indexed");
        assert!(app.index.backlinks(beta).is_empty());

        // A new link, written with `:w`.
        type_text(&mut app, "Go[[Beta]]");
        press(&mut app, KeyCode::Esc);
        type_text(&mut app, ":w");
        press(&mut app, KeyCode::Enter);

        settle(&mut app, 100, 30, |app| {
            !app.index.backlinks(beta).is_empty()
        });
        let alpha = app.index.id_of_rel("Alpha.md").expect("indexed");
        assert_eq!(app.index.backlinks(beta)[0].source, alpha);
        let text = std::fs::read_to_string(vault.path().join("Alpha.md")).expect("read");
        assert!(text.contains("[[Beta]]"), "file was {text:?}");
    }

    #[test]
    fn bridge_follow_opens_the_linked_note() {
        let Some((_vault, mut app)) =
            nvim_app_with(&[("Alpha.md", "see [[Beta]] now\n"), ("Beta.md", "# Beta\n")])
        else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| {
            row(app, 0).starts_with("see [[Beta]]")
        });

        type_text(&mut app, "f[");
        press(&mut app, KeyCode::Enter);

        settle(&mut app, 100, 30, |app| active_title(app) == "Beta");
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Beta"));
        assert_eq!(app.tabs.len(), 2, "the link opened a second tab");
        assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Editing));
    }

    #[test]
    fn bridge_follow_from_the_editor_stays_in_the_editor_and_jumps() {
        let mut beta = String::from("# Beta\n");
        for i in 0..40 {
            beta.push_str(&format!("line {i}\n"));
        }
        beta.push_str("## Deep\ntext\n");
        let Some((_vault, mut app)) =
            nvim_app_with(&[("Alpha.md", "see [[Beta#Deep]] now\n"), ("Beta.md", &beta)])
        else {
            return;
        };
        // The default: notes open in the reading view. A link followed while
        // editing must not throw the user out of nvim into it.
        app.config.ui.reading_mode = true;
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| {
            row(app, 0).starts_with("see [[Beta#Deep]]")
        });

        type_text(&mut app, "f[");
        press(&mut app, KeyCode::Enter);

        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("## Deep"));
        assert_eq!(active_title(&app), "Beta");
        assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Editing));
    }

    #[test]
    fn bridge_entered_syncs_tab() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        type_text(&mut app, ":e Beta.md");
        press(&mut app, KeyCode::Enter);

        settle(&mut app, 100, 30, |app| active_title(app) == "Beta");
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Beta"));
        assert_eq!(app.tabs.len(), 2);
        // And going back through the app lands on the first note again.
        app.cycle_tab(-1);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
        assert_eq!(active_title(&app), "Alpha");
    }

    #[test]
    fn bridge_entered_is_ignored_while_reading() {
        let Some((vault, mut app)) = nvim_app() else {
            return;
        };
        app.active_mut().expect("tab").mode = Mode::Reading;

        let changed = handle_bridge(&mut app, BridgeEvent::Entered(vault.path().join("Beta.md")));

        assert!(!changed);
        assert_eq!(active_title(&app), "Alpha");
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn bridge_leave_returns_to_reading_and_keeps_nvim() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        app.focus = Focus::Note;
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        type_text(&mut app, ":q");
        press(&mut app, KeyCode::Enter);

        settle(&mut app, 100, 30, |app| {
            app.active().map(|tab| tab.mode) == Some(Mode::Reading)
        });
        assert!(
            app.nvim.is_some(),
            "nvim stays alive behind the reading view"
        );
        assert!(!app.status.is_error, "status was {:?}", app.status.text);
    }

    #[test]
    fn follow_heading_anchor_jumps() {
        let mut beta = String::from("# Beta\n");
        for i in 0..40 {
            beta.push_str(&format!("line {i}\n"));
        }
        beta.push_str("## Deep\ntext\n");
        let Some((_vault, mut app)) =
            nvim_app_with(&[("Alpha.md", "# Alpha\n"), ("Beta.md", &beta)])
        else {
            return;
        };
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        crate::actions::dispatch(
            &mut app,
            crate::app::Action::FollowLink(bridge::LinkTarget {
                note: "Beta".to_string(),
                anchor: Some(bridge::Anchor::Heading("Deep".to_string())),
                source: None,
            }),
        );

        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("## Deep"));
        assert_eq!(active_title(&app), "Beta");
    }

    #[test]
    fn follow_anchor_in_the_open_note_jumps_without_a_second_tab() {
        let mut alpha = String::from("# Alpha\n");
        for i in 0..40 {
            alpha.push_str(&format!("line {i}\n"));
        }
        alpha.push_str("the block ^blk1\nend\n");
        let Some((_vault, mut app)) = nvim_app_with(&[("Alpha.md", &alpha), ("Beta.md", "# B\n")])
        else {
            return;
        };
        draw(&mut app, 100, 30);
        settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

        crate::actions::dispatch(
            &mut app,
            crate::app::Action::FollowLink(bridge::LinkTarget {
                note: "Alpha".to_string(),
                anchor: Some(bridge::Anchor::Block("blk1".to_string())),
                source: None,
            }),
        );

        settle(&mut app, 100, 30, |app| {
            row(app, 0).starts_with("the block")
        });
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn nvim_is_told_the_note_names_once_per_index_revision() {
        let Some((_vault, mut app)) = nvim_app() else {
            return;
        };
        draw(&mut app, 100, 30);

        settle(&mut app, 100, 30, |app| {
            app.nvim.as_ref().unwrap().notes_revision() == Some(app.index.revision())
        });
    }

    #[test]
    fn note_names_are_stems_and_aliases_without_duplicates() {
        let vault = TempVault::new("nvim-names");
        vault.write("Alpha.md", "---\naliases: [Al, Beta]\n---\n# A\n");
        vault.write("sub/Beta.md", "# B\n");
        vault.write("Gamma.md", "# G\n");
        let app = App::new(vault.vault(), Config::default()).expect("app");

        assert_eq!(note_names(&app.index), ["Al", "Alpha", "Beta", "Gamma"]);
    }

    /// A bare app, no nvim: the actions `:Emeraldian` can name.
    fn plain_app() -> (TempVault, App) {
        let vault = TempVault::new("nvim-actions");
        vault.write("Alpha.md", "# Alpha\n");
        let app = App::new(vault.vault(), Config::default()).expect("app");
        (vault, app)
    }

    fn action(app: &mut App, name: &str) -> bool {
        handle_bridge(app, BridgeEvent::Action(name.to_string()))
    }

    #[test]
    fn bridge_actions_open_the_matching_views() {
        let (_vault, mut app) = plain_app();

        assert!(action(&mut app, "switcher"));
        assert!(app.modal.is_some());

        app.modal = None;
        action(&mut app, "search");
        assert!(app.modal.is_some());

        app.modal = None;
        action(&mut app, "graph");
        assert_eq!(app.view, crate::app::View::Graph);
    }

    #[test]
    fn bridge_actions_focus_panes_and_side_panels() {
        let (_vault, mut app) = plain_app();
        app.config.ui.show_left_sidebar = true;
        app.config.ui.show_right_sidebar = false;
        app.focus = Focus::Note;

        action(&mut app, "explorer");
        assert_eq!(app.focus, Focus::Explorer);

        // Backlinks opens the right sidebar on its backlinks tab and focuses it.
        action(&mut app, "backlinks");
        assert!(app.config.ui.show_right_sidebar);
        assert_eq!(app.side_panel, crate::app::SidePanel::Backlinks);
        assert_eq!(app.focus, Focus::Sidebar);

        action(&mut app, "outline");
        assert_eq!(app.side_panel, crate::app::SidePanel::Outline);

        action(&mut app, "focus_left");
        assert_eq!(app.focus, Focus::Explorer);
        action(&mut app, "focus_right");
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn bridge_chat_action_focuses_an_open_chat_instead_of_closing_it() {
        let (_vault, mut app) = plain_app();
        app.config.ui.show_chat = false;
        app.focus = Focus::Note;

        action(&mut app, "chat");
        assert!(app.config.ui.show_chat);
        assert_eq!(app.focus, Focus::Chat);

        // Chat open, focus elsewhere: the key brings the focus back.
        app.focus = Focus::Note;
        action(&mut app, "chat");
        assert!(app.config.ui.show_chat);
        assert_eq!(app.focus, Focus::Chat);
    }

    #[test]
    fn bridge_new_and_daily_actions_run() {
        let (_vault, mut app) = plain_app();

        action(&mut app, "new");
        assert!(matches!(app.modal, Some(crate::modal::Modal::Prompt(_))));

        app.modal = None;
        action(&mut app, "daily");
        assert!(
            app.status.text.contains("created"),
            "status was {:?}",
            app.status.text
        );
    }

    #[test]
    fn bridge_every_listed_action_is_handled() {
        for name in bridge::ACTIONS {
            let (_vault, mut app) = plain_app();
            action(&mut app, name);
            assert!(
                !app.status.is_error,
                "`{name}` was not handled: {:?}",
                app.status.text
            );
        }
    }

    #[test]
    fn bridge_unknown_action_is_reported() {
        let (_vault, mut app) = plain_app();

        action(&mut app, "does_not_exist");

        assert!(app.status.is_error);
    }

    #[test]
    fn bridge_written_for_an_unknown_file_is_harmless() {
        let (vault, mut app) = plain_app();

        let changed = handle_bridge(&mut app, BridgeEvent::Written(vault.path().join("nope.md")));

        assert!(!changed);
        assert!(!app.status.is_error);
    }
}
