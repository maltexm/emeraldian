//! Integration tests for what the embedded nvim session does around a note's
//! life: saving, moving, reloading and closing its buffer (`src/nvim/session.rs`
//! and `bridge.lua`).
//!
//! Like `nvim_session.rs`, the session is included by path, every test returns
//! early with a printed skip when `nvim` is not on PATH, and each session runs
//! `--clean` without swap files. The tests that need the `App` (tabs, the
//! assistant's tools, quitting) live next to it, in `src/nvim/lifecycle_tests.rs`.
#![allow(dead_code)]

#[path = "../src/nvim/bridge.rs"]
pub mod bridge;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/nvim/grid.rs"]
mod grid;
#[path = "../src/nvim/rpc.rs"]
pub mod rpc;
#[path = "../src/nvim/session.rs"]
mod session;

mod nvim {
    pub use super::rpc;
}

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use emeraldian_core::test_support::TempVault;

use config::EditorConfig;
use session::Session;

const LIMIT: Duration = Duration::from_secs(10);

fn nvim_available() -> bool {
    std::process::Command::new("nvim")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn start(vault: &Path) -> Option<Session> {
    start_with(vault, true)
}

fn start_with(vault: &Path, auto_save: bool) -> Option<Session> {
    if !nvim_available() {
        eprintln!("skip: nvim not on PATH");
        return None;
    }
    let cfg = EditorConfig {
        auto_save,
        ..EditorConfig::default()
    };
    Some(
        Session::start_with(
            &cfg,
            vault,
            (40, 10),
            &["--clean", "--cmd", "set noswapfile"],
        )
        .expect("start nvim --embed"),
    )
}

/// Drain the session until `done` holds, or `LIMIT` passes.
fn wait_until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) -> bool {
    let deadline = Instant::now() + LIMIT;
    loop {
        let _ = session.poll();
        if done(session) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn on_disk(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Opens `path` and waits until nvim shows it.
fn open(session: &mut Session, path: &Path) {
    session.open(path).expect("open");
    assert!(
        wait_until(session, |s| s.buffer_text(path).is_some()
            && s.grid().cells.iter().any(|c| !c.text.trim().is_empty())),
        "nvim never showed {}",
        path.display()
    );
}

/// The name of a note, as nvim spells it (resolved).
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("resolve")
}

#[test]
fn rename_moves_a_clean_buffer_to_the_new_path() {
    let vault = TempVault::new("nvim-life-rename");
    let from = vault.write("A.md", "alpha\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &from);
    let to = vault.path().join("B.md");
    std::fs::rename(&from, &to).expect("move on disk");

    session.rename(&from, &to).expect("rename the buffer");

    assert_eq!(session.buffer_text(&to).as_deref(), Some("alpha\n"));
    assert_eq!(session.buffer_text(&from), None, "nothing is left of A");
    let names = session.buffer_names();
    assert!(names.contains(&resolved(&to)), "buffers: {names:?}");
    assert!(
        !names.iter().any(|name| name.ends_with("A.md")),
        "the old name must not linger as a buffer: {names:?}"
    );
    // Typing and writing go to the new file, and the old one stays gone.
    session.input("ix<Esc>:w<CR>");
    assert!(
        wait_until(&mut session, |_| on_disk(&to) == "xalpha\n"),
        "B.md was {:?}",
        on_disk(&to)
    );
    assert!(!from.exists(), "the old file came back");
    assert!(!session.is_modified(&to));
}

#[test]
fn rename_keeps_unsaved_edits_and_writes_them_to_the_new_path() {
    let vault = TempVault::new("nvim-life-rename-dirty");
    let from = vault.write("A.md", "alpha\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &from);
    session.input("ix<Esc>");
    assert!(wait_until(&mut session, |s| s.is_modified(&from)));
    let to = vault.path().join("B.md");
    std::fs::rename(&from, &to).expect("move on disk");

    session.rename(&from, &to).expect("rename the buffer");
    // The buffer follows the note, and nothing the user typed is lost: the
    // edits were written to the new file.
    assert_eq!(session.buffer_text(&to).as_deref(), Some("xalpha\n"));
    assert_eq!(on_disk(&to), "xalpha\n");
    assert!(!from.exists());
    assert!(!session.is_modified(&to));
    // A rewrite of the links goes through the buffer too.
    session
        .set_lines(&to, "xalpha [[B]]\n")
        .expect("write the moved buffer");
    assert_eq!(on_disk(&to), "xalpha [[B]]\n");
    // And a later edit saves with a plain `:w`, with no "file exists" in the
    // way.
    session.input("Aq<Esc>:w<CR>");
    assert!(
        wait_until(&mut session, |_| on_disk(&to) == "xalpha [[B]]q\n"),
        "B.md was {:?}",
        on_disk(&to)
    );
}

#[test]
fn a_clean_buffer_saves_with_a_plain_write_after_a_rename() {
    // `:file` marks a buffer "not edited", and `:w` then refuses to overwrite
    // the file that is there (E13). The rename must leave no such trap.
    let vault = TempVault::new("nvim-life-rename-e13");
    let from = vault.write("A.md", "alpha\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &from);
    let to = vault.path().join("B.md");
    std::fs::rename(&from, &to).expect("move on disk");
    session.rename(&from, &to).expect("rename the buffer");

    session.input("Aq<Esc>:w<CR>");

    assert!(
        wait_until(&mut session, |_| on_disk(&to) == "alphaq\n"),
        "B.md was {:?}",
        on_disk(&to)
    );
}

#[test]
fn set_lines_replaces_the_text_and_writes_it_over_a_changed_file() {
    let vault = TempVault::new("nvim-life-set-lines");
    let path = vault.write("A.md", "see [[Old]]\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &path);
    session.input("Gox<Esc>");
    assert!(wait_until(&mut session, |s| s.is_modified(&path)));
    // A rename rewrote the same file on disk, behind the buffer's back.
    std::fs::write(&path, "see [[New]] and more text than before\n").expect("rewrite");

    session
        .set_lines(&path, "see [[New]]\nx\n")
        .expect("write through the buffer");

    assert_eq!(on_disk(&path), "see [[New]]\nx\n");
    assert_eq!(
        session.buffer_text(&path).as_deref(),
        Some("see [[New]]\nx\n")
    );
    assert!(!session.is_modified(&path));
}

#[test]
fn checktime_reloads_a_clean_buffer() {
    let vault = TempVault::new("nvim-life-checktime");
    let path = vault.write("A.md", "alpha\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &path);
    std::fs::write(&path, "alpha changed by the assistant\n").expect("write");

    session.checktime();

    assert_eq!(
        session.buffer_text(&path).as_deref(),
        Some("alpha changed by the assistant\n")
    );
    assert!(!session.is_modified(&path));
}

#[test]
fn checktime_leaves_unsaved_edits_alone() {
    let vault = TempVault::new("nvim-life-checktime-dirty");
    let path = vault.write("A.md", "alpha\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &path);
    session.input("ix<Esc>");
    assert!(wait_until(&mut session, |s| s.is_modified(&path)));
    std::fs::write(&path, "alpha changed on disk\n").expect("write");

    session.checktime();

    assert_eq!(session.buffer_text(&path).as_deref(), Some("xalpha\n"));
    assert!(session.is_modified(&path));
    // nvim is not left waiting for an answer to a question nobody can see: it
    // still takes keys.
    session.input("Ay<Esc>");
    assert!(wait_until(&mut session, |s| {
        s.buffer_text(&path).as_deref() == Some("xalphay\n")
    }));
}

#[test]
fn save_writes_the_buffer() {
    let vault = TempVault::new("nvim-life-save");
    let path = vault.write("A.md", "alpha\n");
    let Some(mut session) = start_with(vault.path(), false) else {
        return;
    };
    open(&mut session, &path);
    session.input("ix<Esc>");
    assert!(wait_until(&mut session, |s| s.is_modified(&path)));

    session.save(&path).expect("save");

    assert_eq!(on_disk(&path), "xalpha\n");
    assert!(!session.is_modified(&path));
}

#[test]
fn close_drops_a_clean_buffer_and_refuses_a_dirty_one() {
    let vault = TempVault::new("nvim-life-close");
    let a = vault.write("A.md", "alpha\n");
    let b = vault.write("B.md", "bravo\n");
    let Some(mut session) = start_with(vault.path(), false) else {
        return;
    };
    open(&mut session, &a);
    open(&mut session, &b);

    session.close(&b, false).expect("a clean buffer closes");
    assert_eq!(session.buffer_text(&b), None);
    assert!(
        !session
            .buffer_names()
            .iter()
            .any(|name| name.ends_with("B.md"))
    );

    // Reopening reads the file again, so a change made meanwhile is seen.
    std::fs::write(&b, "bravo changed\n").expect("write");
    open(&mut session, &b);
    assert_eq!(session.buffer_text(&b).as_deref(), Some("bravo changed\n"));

    session.input("ix<Esc>");
    assert!(wait_until(&mut session, |s| s.is_modified(&b)));
    let err = session.close(&b, false).expect_err("unsaved edits stay");
    assert_eq!(err.to_string(), "the note has unsaved changes");
    assert_eq!(session.buffer_text(&b).as_deref(), Some("xbravo changed\n"));

    session
        .close(&b, true)
        .expect("discarding closes it anyway");
    assert_eq!(session.buffer_text(&b), None);
    assert_eq!(on_disk(&b), "bravo changed\n", "the file keeps what it had");
}

#[test]
fn modified_reports_arrive_without_asking() {
    let vault = TempVault::new("nvim-life-modified");
    let path = vault.write("A.md", "alpha\n");
    let Some(mut session) = start_with(vault.path(), false) else {
        return;
    };
    open(&mut session, &path);
    assert!(!session.shows_modified(&path));

    session.input("ix<Esc>");
    assert!(
        wait_until(&mut session, |s| s.shows_modified(&path)),
        "the dirty flag never arrived"
    );

    session.input(":w<CR>");
    assert!(
        wait_until(&mut session, |s| !s.shows_modified(&path)),
        "the clean flag never arrived"
    );
}

#[test]
fn a_killed_nvim_is_reported_and_nothing_hangs() {
    let vault = TempVault::new("nvim-life-kill");
    let path = vault.write("A.md", "alpha\n");
    let Some(mut session) = start(vault.path()) else {
        return;
    };
    open(&mut session, &path);

    let status = std::process::Command::new("kill")
        .args(["-9", &session.pid().to_string()])
        .status()
        .expect("kill");
    assert!(status.success());

    let mut exited = false;
    assert!(wait_until(&mut session, |s| {
        exited |= s.exited();
        exited
    }));
    // Every call now answers at once instead of waiting out a timeout.
    let started = Instant::now();
    assert!(!session.is_modified(&path));
    assert!(session.save(&path).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn shutdown_ends_the_process() {
    let vault = TempVault::new("nvim-life-shutdown");
    let Some(session) = start(vault.path()) else {
        return;
    };
    let pid = session.pid();
    session.shutdown();

    let deadline = Instant::now() + Duration::from_secs(2);
    while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "nvim is still there"
    );
}
