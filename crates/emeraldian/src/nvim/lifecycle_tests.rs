//! What happens to the notes in nvim when the app around them acts: leaving the
//! editor, closing a tab, quitting, the assistant writing, renaming, deleting,
//! and nvim dying. Plan Task 7; `tests/nvim_lifecycle.rs` has the session-level
//! half.
//!
//! Every test returns early, with a printed skip, when `nvim` is not on PATH.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use emeraldian_agent::ToolCall;
use serde_json::json;

use super::tests::{
    active_title, draw, nvim_app, nvim_app_configured, nvim_app_with, press, row, settle, type_text,
};
use super::*;
use crate::actions;
use crate::app::{Action, App, Focus, Mode};
use crate::keys;
use crate::modal::{Modal, Prompt, PromptIntent};

fn ctrl_e() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)
}

/// Types `text` into the open note and waits until nvim shows a first row that
/// starts with `shows`, back in normal mode.
fn edit(app: &mut App, text: &str, shows: &str) {
    app.focus = Focus::Note;
    draw(app, 100, 30);
    settle(app, 100, 30, |app| row(app, 0).starts_with("# "));
    type_text(app, &format!("i{text}"));
    press(app, KeyCode::Esc);
    settle(app, 100, 30, |app| row(app, 0).starts_with(shows));
}

fn tool(app: &mut App, name: &str, arguments: serde_json::Value) -> emeraldian_agent::ToolOutcome {
    crate::tools::execute(
        app,
        &ToolCall {
            id: "t".into(),
            name: name.into(),
            arguments,
        },
    )
}

fn path_of(app: &App, rel: &str) -> PathBuf {
    let id = app.index.id_of_rel(rel).expect("indexed");
    app.index.note(id).expect("note").meta.path.clone()
}

/// Whether nvim has a buffer for the note.
fn has_buffer(app: &App, rel: &str) -> bool {
    let path = path_of(app, rel);
    app.nvim
        .as_ref()
        .is_some_and(|session| session.buffer_text(&path).is_some())
}

fn buffer_text(app: &App, rel: &str) -> Option<String> {
    let path = path_of(app, rel);
    app.nvim.as_ref()?.buffer_text(&path)
}

fn confirm_modal(app: &mut App) {
    match app.modal.take() {
        Some(Modal::Confirm(confirm)) => actions::confirm(app, confirm.action),
        other => panic!("expected a confirmation, found {other:?}"),
    }
}

fn rename_active(app: &mut App, to: &str) {
    actions::submit_prompt(app, Prompt::new("Rename", to, PromptIntent::RenameNote));
}

/// Open `rel` in a tab and wait until nvim shows its first line.
fn show(app: &mut App, rel: &str, first_line: &str) {
    let id = app.index.id_of_rel(rel).expect("indexed");
    app.open_note(id);
    settle(app, 100, 30, |app| row(app, 0).starts_with(first_line));
}

// ---- leaving the editor ----------------------------------------------------

#[test]
fn ctrl_e_saves_with_autosave() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    keys::handle(&mut app, ctrl_e());

    assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Reading));
    // At once, not after the debounce of the autosave timer.
    assert!(
        vault.read("Alpha.md").starts_with("hi # Alpha"),
        "file was {:?}",
        vault.read("Alpha.md")
    );
    let alpha = app.index.id_of_rel("Alpha.md").unwrap();
    assert!(
        app.index.read(alpha).unwrap().starts_with("hi # Alpha"),
        "the reading view reads the index, which must know the new text"
    );
}

#[test]
fn ctrl_e_without_autosave_keeps_the_edits_unsaved() {
    let Some((vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    keys::handle(&mut app, ctrl_e());

    assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Reading));
    assert_eq!(vault.read("Alpha.md"), "# Alpha\n\nbody\n");
    assert!(app.tab_modified(0), "the edits are still pending");
}

#[test]
fn ctrl_e_stays_in_the_editor_when_the_note_cannot_be_written() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    let path = vault.path().join("Alpha.md");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o444);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    edit(&mut app, "hi ", "hi # Alpha");

    keys::handle(&mut app, ctrl_e());

    permissions.set_mode(0o644);
    std::fs::set_permissions(&path, permissions).unwrap();
    assert_eq!(
        app.active().map(|tab| tab.mode),
        Some(Mode::Editing),
        "unsaved edits must not be put behind the reading view without a word"
    );
    assert!(app.status.is_error, "status was {:?}", app.status.text);
    assert_eq!(vault.read("Alpha.md"), "# Alpha\n\nbody\n");
}

#[test]
fn the_tab_bar_marks_a_note_with_unsaved_edits() {
    let Some((_vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");
    let path = path_of(&app, "Alpha.md");

    // The bridge reports the change; the next frame shows it.
    settle(&mut app, 100, 30, |app| {
        app.nvim.as_ref().unwrap().shows_modified(&path)
    });
    let screen = draw(&mut app, 100, 30).join("\n");
    assert!(
        screen.contains(crate::ui::icons::MODIFIED),
        "screen was:\n{screen}"
    );
}

// ---- closing a tab ---------------------------------------------------------

#[test]
fn close_dirty_tab_prompts_without_autosave() {
    let Some((vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    actions::dispatch(&mut app, Action::CloseTab);

    assert!(
        matches!(app.modal, Some(Modal::Confirm(_))),
        "modal was {:?}",
        app.modal
    );
    assert_eq!(app.tabs.len(), 1, "nothing is closed before the answer");
    assert!(app.tab_modified(0));

    // No: the tab and its edits stay.
    app.modal = None;
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(
        buffer_text(&app, "Alpha.md").as_deref(),
        Some("hi # Alpha\n\nbody\n")
    );

    // Yes: the tab goes, and the file keeps what it had.
    actions::dispatch(&mut app, Action::CloseTab);
    confirm_modal(&mut app);
    assert!(app.tabs.is_empty());
    assert!(
        !has_buffer(&app, "Alpha.md"),
        "the buffer went with the tab"
    );
    assert_eq!(vault.read("Alpha.md"), "# Alpha\n\nbody\n");
}

#[test]
fn close_dirty_tab_saves_with_autosave() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    actions::dispatch(&mut app, Action::CloseTab);

    assert!(app.modal.is_none(), "autosave closes without asking");
    assert!(app.tabs.is_empty());
    assert!(
        vault.read("Alpha.md").starts_with("hi # Alpha"),
        "file was {:?}",
        vault.read("Alpha.md")
    );
}

#[test]
fn a_closed_tab_leaves_no_stale_buffer_behind() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
    show(&mut app, "Beta.md", "# Beta");

    actions::dispatch(&mut app, Action::CloseTab);
    assert_eq!(app.tabs.len(), 1);
    assert!(!has_buffer(&app, "Beta.md"));

    // The file changes while the note is closed; opening it shows the file.
    vault.write("Beta.md", "# Beta changed\n");
    show(&mut app, "Beta.md", "# Beta changed");
}

// ---- quitting --------------------------------------------------------------

fn process_is_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    !Path::new(&format!("/proc/{pid}")).exists()
}

#[test]
fn quit_writes_all_with_autosave_and_exits_nvim() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");
    let pid = app.nvim.as_ref().expect("session").pid();

    actions::dispatch(&mut app, Action::Quit);
    assert!(
        matches!(&app.modal, Some(Modal::Confirm(c)) if c.message.contains("1 note has unsaved")),
        "modal was {:?}",
        app.modal
    );
    confirm_modal(&mut app);
    assert!(app.quit);
    shutdown(&mut app);

    assert!(vault.read("Alpha.md").starts_with("hi # Alpha"));
    assert!(process_is_gone(pid, Duration::from_secs(2)), "nvim is left");
}

#[test]
fn quit_does_not_leave_while_a_note_cannot_be_written() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    let path = vault.path().join("Alpha.md");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o444);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    edit(&mut app, "hi ", "hi # Alpha");

    actions::dispatch(&mut app, Action::Quit);
    confirm_modal(&mut app);

    permissions.set_mode(0o644);
    std::fs::set_permissions(&path, permissions).unwrap();
    assert!(!app.quit, "quitting would have thrown the edits away");
    assert!(app.status.is_error, "status was {:?}", app.status.text);
    assert_eq!(
        buffer_text(&app, "Alpha.md").as_deref(),
        Some("hi # Alpha\n\nbody\n")
    );
}

#[test]
fn quit_counts_unsaved_buffers_without_autosave() {
    let Some((vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    actions::dispatch(&mut app, Action::Quit);
    assert!(
        matches!(&app.modal, Some(Modal::Confirm(c)) if c.message.contains("1 note has unsaved")),
        "modal was {:?}",
        app.modal
    );
    confirm_modal(&mut app);
    shutdown(&mut app);

    // The prompt said so: without autosave the edits are not kept.
    assert_eq!(vault.read("Alpha.md"), "# Alpha\n\nbody\n");
}

#[test]
fn switching_vault_ends_the_session_and_saves_first() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    let other = emeraldian_core::test_support::TempVault::new("nvim-other-vault");
    other.write("Other.md", "# Other\n");
    edit(&mut app, "hi ", "hi # Alpha");
    let pid = app.nvim.as_ref().expect("session").pid();

    app.open_vault(other.path().to_path_buf());

    assert!(vault.read("Alpha.md").starts_with("hi # Alpha"));
    assert!(app.nvim.is_none(), "the session belongs to the old vault");
    assert!(process_is_gone(pid, Duration::from_secs(2)));
}

// ---- the assistant ---------------------------------------------------------

#[test]
fn agent_write_refuses_dirty_buffer() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    for (name, arguments) in [
        ("append_note", json!({ "name": "Alpha", "text": "agent" })),
        (
            "replace_note",
            json!({ "name": "Alpha", "content": "agent\n" }),
        ),
        ("link_notes", json!({ "from": "Alpha", "to": "Beta" })),
        (
            "rename_note",
            json!({ "name": "Alpha", "new_name": "Gamma" }),
        ),
        ("delete_note", json!({ "name": "Alpha" })),
    ] {
        let outcome = tool(&mut app, name, arguments);
        assert!(outcome.is_error, "{name} went through: {}", outcome.content);
        assert!(
            outcome
                .content
                .contains("note has unsaved edits in the editor"),
            "{name}: {}",
            outcome.content
        );
        assert_eq!(vault.read("Alpha.md"), "# Alpha\n\nbody\n", "{name}");
    }
    assert_eq!(
        buffer_text(&app, "Alpha.md").as_deref(),
        Some("hi # Alpha\n\nbody\n")
    );
    assert!(vault.exists("Alpha.md"));
}

#[test]
fn agent_write_then_checktime_reloads_clean_buffer() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));

    let outcome = tool(
        &mut app,
        "replace_note",
        json!({ "name": "Alpha", "content": "# Agent wrote this\n" }),
    );

    assert!(!outcome.is_error, "{}", outcome.content);
    assert_eq!(vault.read("Alpha.md"), "# Agent wrote this");
    settle(&mut app, 100, 30, |app| {
        row(app, 0).starts_with("# Agent wrote this")
    });
    assert!(!app.tab_modified(0));
}

#[test]
fn agent_write_to_a_note_that_is_not_open_still_goes_through() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    let outcome = tool(
        &mut app,
        "append_note",
        json!({ "name": "Beta", "text": "from the agent" }),
    );

    assert!(!outcome.is_error, "{}", outcome.content);
    assert!(vault.read("Beta.md").contains("from the agent"));
}

#[test]
fn include_active_note_reads_unsaved_buffer() {
    let Some((_vault, mut app)) = nvim_app_configured(
        &[
            ("Alpha.md", "---\ntags: [x]\n---\n# Alpha\n\nbody\n"),
            ("Beta.md", "# Beta\n"),
        ],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("---"));
    type_text(&mut app, "gg3jIunsaved ");
    press(&mut app, KeyCode::Esc);
    settle(&mut app, 100, 30, |app| {
        row(app, 3).starts_with("unsaved # Alpha")
    });

    let (rel, body) = crate::agent::active_note_text(&app).expect("an active note");

    assert_eq!(rel, "Alpha.md");
    assert!(body.contains("unsaved # Alpha"), "body was {body:?}");
    assert!(
        !body.contains("tags:"),
        "the frontmatter stays out, as it does when the file is read: {body:?}"
    );
}

#[test]
fn an_agent_turn_that_may_write_saves_the_buffers_first() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");
    app.config.agent.allow_writes = true;

    before_agent_turn(&mut app);

    assert!(vault.read("Alpha.md").starts_with("hi # Alpha"));
}

#[test]
fn an_agent_turn_that_cannot_write_leaves_the_buffers_alone() {
    let Some((vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");
    app.config.agent.allow_writes = true;

    before_agent_turn(&mut app);

    assert_eq!(vault.read("Alpha.md"), "# Alpha\n\nbody\n");
}

#[test]
fn reloading_the_vault_reloads_clean_buffers() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
    vault.write("Alpha.md", "# Alpha from outside\n");

    actions::dispatch(&mut app, Action::Refresh);

    settle(&mut app, 100, 30, |app| {
        row(app, 0).starts_with("# Alpha from outside")
    });
}

// ---- renaming and deleting -------------------------------------------------

#[test]
fn rename_open_note_moves_buffer() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    app.focus = Focus::Note;
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
    let old = path_of(&app, "Alpha.md");

    rename_active(&mut app, "Gamma");

    assert_eq!(active_title(&app), "Gamma");
    let new = path_of(&app, "Gamma.md");
    assert!(new.exists() && !old.exists());
    let names = app.nvim.as_ref().unwrap().buffer_names();
    let resolved = std::fs::canonicalize(&new).unwrap();
    assert!(names.contains(&resolved), "buffers: {names:?}");
    assert!(
        !names.iter().any(|name| name.ends_with("Alpha.md")),
        "the old name lingers: {names:?}"
    );
    // What is typed now goes to the new file, and the old one stays gone.
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
    type_text(&mut app, "Ax");
    press(&mut app, KeyCode::Esc);
    type_text(&mut app, ":w");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, 100, 30, |_| {
        vault.read("Gamma.md").contains("# Alphax")
    });
    assert!(!vault.exists("Alpha.md"), "the old file came back");
}

#[test]
fn rename_that_reorders_notes_keeps_the_active_note() {
    // Ids are positions in the sorted list: `Control` -> `Zulu` moves the note
    // behind `Omega`, and every id after it shifts.
    let Some((vault, mut app)) = nvim_app_with(&[
        ("Alpha.md", "# Alpha\n"),
        ("Control.md", "Control.md BODY\n"),
        ("Omega.md", "# Omega\n"),
    ]) else {
        return;
    };
    app.focus = Focus::Note;
    draw(&mut app, 100, 30);
    show(&mut app, "Control.md", "Control.md BODY");
    show(&mut app, "Omega.md", "# Omega");
    show(&mut app, "Control.md", "Control.md BODY");
    assert_eq!(app.tabs.len(), 3);

    rename_active(&mut app, "Zulu");

    assert_eq!(active_title(&app), "Zulu");
    let titles: Vec<String> = app.tabs.iter().map(|t| app.note_title(t.note)).collect();
    assert_eq!(
        titles,
        ["Alpha", "Zulu", "Omega"],
        "the tabs kept their notes"
    );
    settle(&mut app, 100, 30, |app| {
        row(app, 0).starts_with("Control.md BODY")
    });
    // An edit and a write land in Zulu.md, and Omega.md is untouched.
    type_text(&mut app, "Ax");
    press(&mut app, KeyCode::Esc);
    type_text(&mut app, ":w");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, 100, 30, |_| {
        vault.read("Zulu.md") == "Control.md BODYx\n"
    });
    assert_eq!(vault.read("Omega.md"), "# Omega\n");
    assert!(!vault.exists("Control.md"));
    assert_eq!(active_title(&app), "Zulu");
}

#[test]
fn rename_of_a_diagram_file_keeps_the_active_note() {
    let Some((vault, mut app)) = nvim_app_with(&[
        ("Alpha.md", "# Alpha\n"),
        ("Move.mmd", "flowchart TD\n  X[[Old]] --> Y[Keep]\n"),
        ("Other.mermaid", "flowchart TD\n  P --> Q\n"),
    ]) else {
        return;
    };
    app.focus = Focus::Note;
    draw(&mut app, 100, 30);
    show(&mut app, "Move.mmd", "flowchart TD");
    show(&mut app, "Other.mermaid", "flowchart TD");
    show(&mut app, "Move.mmd", "flowchart TD");

    rename_active(&mut app, "Renamed");

    assert_eq!(
        app.active_note()
            .map(|id| app.index.note(id).unwrap().meta.rel.clone())
            .as_deref(),
        Some("Renamed.mmd")
    );
    assert_eq!(
        vault.read("Renamed.mmd"),
        "flowchart TD\n  X[[Old]] --> Y[Keep]\n",
        "a diagram's source is not rewritten as if it were links"
    );
    assert!(!vault.exists("Move.mmd"));
    assert_eq!(
        buffer_text(&app, "Renamed.mmd").as_deref(),
        Some("flowchart TD\n  X[[Old]] --> Y[Keep]\n")
    );
    type_text(&mut app, "GAx");
    press(&mut app, KeyCode::Esc);
    type_text(&mut app, ":w");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, 100, 30, |_| {
        vault.read("Renamed.mmd").contains('x')
    });
    assert_eq!(vault.read("Other.mermaid"), "flowchart TD\n  P --> Q\n");
}

#[test]
fn rename_rewrites_links_in_dirty_note_via_buffer() {
    let Some((vault, mut app)) = nvim_app_with(&[
        ("Alpha.md", "see [[Beta]] and [[Beta|b]]\n"),
        ("Beta.md", "# Beta\n"),
    ]) else {
        return;
    };
    app.focus = Focus::Note;
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| {
        row(app, 0).starts_with("see [[Beta]]")
    });
    // Unsaved edit in Alpha, which links to the note about to be renamed.
    type_text(&mut app, "Ax");
    press(&mut app, KeyCode::Esc);
    settle(&mut app, 100, 30, |app| {
        row(app, 0).trim_end().ends_with("[[Beta|b]]x")
    });
    show(&mut app, "Beta.md", "# Beta");

    rename_active(&mut app, "Gamma");

    // The unsaved text was rewritten through the buffer and kept.
    assert_eq!(
        buffer_text(&app, "Alpha.md").as_deref(),
        Some("see [[Gamma]] and [[Gamma|b]]x\n")
    );
    assert_eq!(vault.read("Alpha.md"), "see [[Gamma]] and [[Gamma|b]]x\n");
    let alpha = path_of(&app, "Alpha.md");
    assert!(!app.nvim.as_ref().unwrap().is_modified(&alpha));
    // And Alpha still takes a plain `:w`, with no question from nvim.
    show(&mut app, "Alpha.md", "see [[Gamma]]");
    type_text(&mut app, "Ay");
    press(&mut app, KeyCode::Esc);
    type_text(&mut app, ":w");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, 100, 30, |_| {
        vault.read("Alpha.md").ends_with("xy\n")
    });
}

#[test]
fn rename_reloads_clean_buffers_whose_links_were_rewritten() {
    let Some((vault, mut app)) =
        nvim_app_with(&[("Alpha.md", "see [[Beta]]\n"), ("Beta.md", "# Beta\n")])
    else {
        return;
    };
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| {
        row(app, 0).starts_with("see [[Beta]]")
    });
    show(&mut app, "Beta.md", "# Beta");

    rename_active(&mut app, "Gamma");

    assert_eq!(vault.read("Alpha.md"), "see [[Gamma]]\n");
    assert_eq!(
        buffer_text(&app, "Alpha.md").as_deref(),
        Some("see [[Gamma]]\n"),
        "nvim still shows the old link"
    );
}

#[test]
fn rename_of_a_dirty_note_keeps_its_edits() {
    let Some((vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.auto_save = false,
    ) else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    rename_active(&mut app, "Gamma");

    assert!(!vault.exists("Alpha.md"));
    assert_eq!(vault.read("Gamma.md"), "hi # Alpha\n\nbody\n");
    assert_eq!(
        buffer_text(&app, "Gamma.md").as_deref(),
        Some("hi # Alpha\n\nbody\n")
    );
}

#[test]
fn deleting_the_open_note_closes_its_buffer_and_keeps_the_other_tabs() {
    let Some((vault, mut app)) = nvim_app_with(&[
        ("Alpha.md", "# Alpha\n"),
        ("Beta.md", "# Beta\n"),
        ("Gamma.md", "# Gamma\n"),
    ]) else {
        return;
    };
    draw(&mut app, 100, 30);
    show(&mut app, "Beta.md", "# Beta");
    show(&mut app, "Gamma.md", "# Gamma");
    show(&mut app, "Alpha.md", "# Alpha");

    actions::dispatch(&mut app, Action::DeleteNote);
    confirm_modal(&mut app);

    assert!(!vault.exists("Alpha.md"));
    assert!(!has_buffer(&app, "Beta.md") || app.tabs.len() == 2);
    let titles: Vec<String> = app.tabs.iter().map(|t| app.note_title(t.note)).collect();
    assert_eq!(titles, ["Beta", "Gamma"], "the other tabs kept their notes");
    let names = app.nvim.as_ref().unwrap().buffer_names();
    assert!(
        !names.iter().any(|name| name.ends_with("Alpha.md")),
        "the deleted note's buffer could write it back: {names:?}"
    );
}

#[test]
fn deleting_a_note_with_unsaved_edits_does_not_write_it_back() {
    let Some((vault, mut app)) = nvim_app() else {
        return;
    };
    edit(&mut app, "hi ", "hi # Alpha");

    actions::dispatch(&mut app, Action::DeleteNote);
    confirm_modal(&mut app);
    // The autosave timer is a second; give it the chance to misbehave.
    std::thread::sleep(Duration::from_millis(1300));
    for _ in 0..5 {
        poll(&mut app);
    }

    assert!(!vault.exists("Alpha.md"), "autosave brought the note back");
}

// ---- nvim going away -------------------------------------------------------

/// A stand-in for `nvim` that starts it clean, so that a restart in a test
/// never reads anybody's config, shada or swap files.
fn clean_nvim() -> String {
    let dir = std::env::temp_dir().join(format!("emeraldian-nvim-clean-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("wrapper dir");
    let script = dir.join("nvim");
    std::fs::write(
        &script,
        "#!/bin/sh\nexec nvim --clean --cmd 'set noswapfile' \"$@\"\n",
    )
    .expect("wrapper");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script.to_string_lossy().into_owned()
}

#[test]
fn nvim_crash_falls_back_to_reading() {
    let wrapper = clean_nvim();
    let Some((_vault, mut app)) = nvim_app_configured(
        &[("Alpha.md", "# Alpha\n\nbody\n"), ("Beta.md", "# Beta\n")],
        |config| config.editor.nvim = wrapper.clone(),
    ) else {
        return;
    };
    app.focus = Focus::Note;
    draw(&mut app, 100, 30);
    settle(&mut app, 100, 30, |app| row(app, 0).starts_with("# Alpha"));
    let pid = app.nvim.as_ref().unwrap().pid();

    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
    settle(&mut app, 100, 30, |app| app.nvim.is_none());

    assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Reading));
    assert!(app.status.is_error, "status was {:?}", app.status.text);
    assert!(
        !app.nvim_failed,
        "a crash is not a reason to give up on nvim"
    );

    // The next Ctrl+E starts a fresh nvim, with the note in it.
    keys::handle(&mut app, ctrl_e());
    assert_eq!(app.active().map(|tab| tab.mode), Some(Mode::Editing));
    settle(&mut app, 100, 30, |app| {
        app.nvim.is_some() && row(app, 0).starts_with("# Alpha")
    });
    assert_ne!(app.nvim.as_ref().unwrap().pid(), pid);
}
