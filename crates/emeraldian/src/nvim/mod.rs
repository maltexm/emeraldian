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

use std::time::Duration;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::app::{App, Focus, Mode};

// Parts of the client and the grid model wait for the bridge and lifecycle
// work before anything outside the tests uses them.
#[allow(dead_code)]
pub mod grid;
#[allow(dead_code)]
pub mod input;
pub mod render;
#[allow(dead_code)]
pub mod rpc;
#[allow(dead_code)]
pub mod session;

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

    // Drained even while a note is being read: nvim keeps running behind the
    // reading view, and its messages must not pile up or its exit go unseen.
    let mut exit = None;
    for event in session.poll() {
        match event {
            Event::Redraw => redraw |= editing,
            Event::Exited(code) => exit = Some(code),
            Event::Bridge(event) => match event {},
        }
    }

    if let Some(err) = open_error {
        app.error(format!("could not open the note in nvim: {err}"));
        redraw = true;
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

/// Writes what auto-save asks for and ends the nvim process.
pub fn shutdown(app: &mut App) {
    if let Some(mut session) = app.nvim.take() {
        if app.config.editor.auto_save {
            let _ = session.save_all();
        }
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

    const LIMIT: Duration = Duration::from_secs(10);

    fn nvim_available() -> bool {
        std::process::Command::new("nvim")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// The app over a temp vault with the nvim backend, `Alpha.md` open in it,
    /// and a clean nvim already attached (so no test reads or writes anyone's
    /// nvim config, shada or swap files). `None` without nvim.
    fn nvim_app() -> Option<(TempVault, App)> {
        if !nvim_available() {
            eprintln!("skip: nvim not on PATH");
            return None;
        }
        let vault = TempVault::new("nvim-wiring");
        vault.write("Alpha.md", "# Alpha\n\nbody\n");
        vault.write("Beta.md", "# Beta\n");
        let mut config = Config::default();
        config.editor.backend = EditorBackend::Nvim;
        config.ui.reading_mode = false;
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
    fn draw(app: &mut App, width: u16, height: u16) -> Vec<String> {
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
    fn settle(app: &mut App, width: u16, height: u16, mut done: impl FnMut(&App) -> bool) {
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

    fn row(app: &App, row: usize) -> String {
        let grid = app.nvim.as_ref().expect("session").grid();
        let width = usize::from(grid.width);
        grid.cells[row * width..(row + 1) * width]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    fn type_text(app: &mut App, text: &str) {
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
}
