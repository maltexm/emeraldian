//! Tests for the injected bridge (`src/nvim/bridge.lua`, `src/nvim/bridge.rs`)
//! against a real `nvim --embed`.
//!
//! The crate is a binary, so the modules under test are included by path, the
//! way `nvim_session.rs` does it. Every test that needs nvim returns early,
//! with a printed skip, when `nvim` is not on PATH, and each session runs
//! `--clean` without swap files so no test touches anyone's nvim setup.
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

/// The paths `rpc` and `bridge` have inside the binary, which `grid.rs`'s
/// fixture recorder and `session.rs` name when they run here.
mod nvim {
    pub use super::rpc;
}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use emeraldian_core::test_support::TempVault;

use bridge::{Anchor, BridgeEvent, LinkTarget};
use config::EditorConfig;
use grid::Grid;
use session::{Event, Session};

/// Long enough that a busy machine reports a slow test rather than a broken
/// one; nvim answers in single-digit milliseconds when it is healthy.
const LIMIT: Duration = Duration::from_secs(10);

fn nvim_available() -> bool {
    Command::new("nvim")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// A session over `vault` with `auto_save` as given, or `None` (with a printed
/// skip) without nvim. `--clean` keeps it away from anyone's config.
fn start(vault: &Path, auto_save: bool) -> Option<Session> {
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
            (60, 12),
            &["--clean", "--cmd", "set noswapfile"],
        )
        .expect("start nvim --embed"),
    )
}

/// Drain the session until `done` holds for the bridge events seen so far (in
/// order), or `LIMIT` passes. Returns what was seen either way.
fn collect(
    session: &mut Session,
    mut done: impl FnMut(&Session, &[BridgeEvent]) -> bool,
) -> Result<Vec<BridgeEvent>, Vec<BridgeEvent>> {
    let deadline = Instant::now() + LIMIT;
    let mut seen = Vec::new();
    loop {
        for event in session.poll() {
            if let Event::Bridge(event) = event {
                seen.push(event);
            }
        }
        if done(session, &seen) {
            return Ok(seen);
        }
        if Instant::now() >= deadline {
            return Err(seen);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize")
}

/// One grid row as it appears on screen.
fn row_text(grid: &Grid, row: u16) -> String {
    let width = grid.width as usize;
    (0..width)
        .map(|col| {
            grid.cells
                .get(row as usize * width + col)
                .map(|cell| cell.text.clone())
                .unwrap_or_default()
        })
        .collect()
}

fn follows(seen: &[BridgeEvent]) -> Vec<&BridgeEvent> {
    seen.iter()
        .filter(|event| matches!(event, BridgeEvent::Follow(_)))
        .collect()
}

fn actions(seen: &[BridgeEvent]) -> Vec<&str> {
    seen.iter()
        .filter_map(|event| match event {
            BridgeEvent::Action(name) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

/// The pure parts of `bridge.lua` (link parser, anchor lookup), run by nvim
/// itself.
#[test]
fn lua_spec_passes() {
    if !nvim_available() {
        eprintln!("skip: nvim not on PATH");
        return;
    }
    let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/nvim/bridge_spec.lua");
    let out = Command::new("nvim")
        .args(["--headless", "--clean", "-l"])
        .arg(&spec)
        .output()
        .expect("run nvim -l");
    assert!(
        out.status.success(),
        "bridge_spec.lua failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `<CR>` on a wikilink tells emeraldian where it leads.
#[test]
fn bridge_follow_notifies() {
    let vault = TempVault::new("nvim-bridge-follow");
    let path = vault.write("a.md", "see [[Alpha]] now\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    // `f[` puts the cursor on the link, `<CR>` follows it.
    session.input("f[<CR>");
    let seen = collect(&mut session, |_, seen| !follows(seen).is_empty()).expect("a Follow event");

    assert_eq!(
        follows(&seen),
        [&BridgeEvent::Follow(LinkTarget {
            note: "Alpha".to_string(),
            anchor: None,
            source: None,
        })]
    );
}

/// A markdown link carries the note it was written in, so that a relative
/// path can be resolved from that note's folder.
#[test]
fn bridge_follow_markdown_link_names_its_source() {
    let vault = TempVault::new("nvim-bridge-follow-md");
    let path = vault.write("sub/a.md", "go [there](../Beta%20Two.md#Sec) now\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input("f[<CR>");
    let seen = collect(&mut session, |_, seen| !follows(seen).is_empty()).expect("a Follow event");

    assert_eq!(
        follows(&seen),
        [&BridgeEvent::Follow(LinkTarget {
            note: "../Beta Two.md".to_string(),
            anchor: Some(Anchor::Heading("Sec".to_string())),
            source: Some(canonical(&path)),
        })]
    );
}

/// With no link under the cursor `<CR>` keeps its meaning (down a line), and
/// nothing is reported. An external link is no link either.
#[test]
fn bridge_enter_without_a_link_keeps_its_default() {
    let vault = TempVault::new("nvim-bridge-nolink");
    let path = vault.write("a.md", "plain text\n[ext](https://example.org)\nlast\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    // <CR> on plain text goes down to the link line, which starts with the
    // external link; <CR> on that goes down again. (No `f[` here: a failed
    // motion flushes the keys typed after it.)
    session.input("<CR><CR>");
    let seen = collect(&mut session, |session, _| session.grid().cursor.1 == 2);

    let seen = seen.expect("the cursor to reach the last line");
    assert!(follows(&seen).is_empty(), "events were {seen:?}");
}

/// `:w` reports the written note.
#[test]
fn bridge_write_notifies() {
    let vault = TempVault::new("nvim-bridge-write");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input("ahello<Esc>:w<CR>");
    let seen = collect(&mut session, |_, seen| {
        seen.iter()
            .any(|event| matches!(event, BridgeEvent::Written(_)))
    })
    .expect("a Written event");

    assert!(seen.contains(&BridgeEvent::Written(canonical(&path))));
    assert!(seen.contains(&BridgeEvent::Modified(canonical(&path), true)));
    assert!(std::fs::read_to_string(&path).unwrap().contains("hello"));
}

/// `:q` on the last window means "leave editing"; nvim stays alive and usable.
#[test]
fn bridge_leave_on_q() {
    let vault = TempVault::new("nvim-bridge-leave");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input(":q<CR>");
    let seen = collect(&mut session, |_, seen| {
        seen.contains(&BridgeEvent::LeaveEditing)
    })
    .expect("LeaveEditing");
    assert!(seen.contains(&BridgeEvent::LeaveEditing));

    // Still there, still taking keys.
    session.input("ix<Esc>");
    assert!(
        collect(&mut session, |session, _| {
            row_text(session.grid(), 0).starts_with("xone")
        })
        .is_ok(),
        "nvim did not take input after :q; row 0 was {:?}",
        row_text(session.grid(), 0)
    );
    assert!(!session.exited());
}

/// `:wq` saves, then leaves.
#[test]
fn bridge_leave_on_wq_writes_first() {
    let vault = TempVault::new("nvim-bridge-wq");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input("ix<Esc>:wq<CR>");
    let seen = collect(&mut session, |_, seen| {
        seen.contains(&BridgeEvent::LeaveEditing)
    })
    .expect("LeaveEditing");

    assert!(seen.contains(&BridgeEvent::LeaveEditing));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xone\n");
    assert!(!session.exited());
}

/// `:q` with a split open closes the split, and is nvim's own.
#[test]
fn bridge_q_with_a_split_closes_the_split() {
    let vault = TempVault::new("nvim-bridge-split");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input(":split<CR>:q<CR>ix<Esc>");
    let seen = collect(&mut session, |session, _| {
        row_text(session.grid(), 0).starts_with("xone")
    })
    .expect("nvim to take input after closing the split");
    assert!(
        !seen.contains(&BridgeEvent::LeaveEditing),
        "events were {seen:?}"
    );
    assert!(!session.exited());
}

/// `:q!` is nvim's own: it ends the process, which emeraldian then treats as
/// any exit it did not ask for.
#[test]
fn bridge_quit_with_bang_stays_nvims() {
    let vault = TempVault::new("nvim-bridge-qbang");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input(":q!<CR>");
    let end = Instant::now() + LIMIT;
    while !session.exited() && Instant::now() < end {
        let _ = session.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(session.exited(), "`:q!` should have ended nvim");
}

/// `:Emeraldian <action>` reaches emeraldian; an unknown name does not.
#[test]
fn bridge_actions_are_forwarded() {
    let vault = TempVault::new("nvim-bridge-action");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input(":Emeraldian bogus<CR>:Emeraldian switcher<CR>:Emeraldian local_graph<CR>");
    let seen = collect(&mut session, |_, seen| {
        seen.contains(&BridgeEvent::Action("local_graph".to_string()))
    })
    .expect("both actions");

    assert_eq!(actions(&seen), ["switcher", "local_graph"]);
}

/// Every action the Rust side knows is accepted by the command.
#[test]
fn bridge_every_listed_action_is_accepted() {
    let vault = TempVault::new("nvim-bridge-all-actions");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    for name in bridge::ACTIONS {
        session.input(&format!(":Emeraldian {name}<CR>"));
    }
    let seen = collect(&mut session, |_, seen| {
        actions(seen).len() == bridge::ACTIONS.len()
    })
    .expect("all actions");

    assert_eq!(actions(&seen), bridge::ACTIONS);
}

/// Opening another note inside nvim is reported, so emeraldian can follow.
#[test]
fn bridge_entered_reports_native_navigation() {
    let vault = TempVault::new("nvim-bridge-entered");
    let a = vault.write("a.md", "one\n");
    let b = vault.write("b.md", "two\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&a).expect("open");

    session.input(":e b.md<CR>");
    let seen = collect(&mut session, |_, seen| {
        seen.contains(&BridgeEvent::Entered(canonical(&b)))
    })
    .expect("Entered b.md");
    assert!(seen.contains(&BridgeEvent::Entered(canonical(&b))));
}

/// Leaving insert mode saves after the debounce, with auto-save on.
#[test]
fn autosave_after_insert_leave() {
    let vault = TempVault::new("nvim-bridge-autosave");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), true) else {
        return;
    };
    session.open(&path).expect("open");

    session.input("Ahello<Esc>");
    let started = Instant::now();
    let saved = loop {
        let _ = session.poll();
        if std::fs::read_to_string(&path).is_ok_and(|text| text.contains("hello")) {
            break true;
        }
        if started.elapsed() > Duration::from_millis(1500) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(saved, "file was {:?}", std::fs::read_to_string(&path));
    // Not before the debounce: a save per keystroke is what it exists to avoid.
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "saved after only {:?}",
        started.elapsed()
    );
}

/// Without auto-save nothing is written behind the user's back.
#[test]
fn no_autosave_when_it_is_off() {
    let vault = TempVault::new("nvim-bridge-no-autosave");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");

    session.input("Ahello<Esc>");
    let until = Instant::now() + Duration::from_millis(1600);
    while Instant::now() < until {
        let _ = session.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");
    assert!(session.is_modified(&path));
}

/// Switching to another buffer saves the one left at once, not after the
/// debounce.
#[test]
fn autosave_on_leaving_the_buffer() {
    let vault = TempVault::new("nvim-bridge-autosave-leave");
    let a = vault.write("a.md", "one\n");
    let b = vault.write("b.md", "two\n");
    let Some(mut session) = start(vault.path(), true) else {
        return;
    };
    session.open(&a).expect("open a");

    session.input("Ahello<Esc>");
    session.open(&b).expect("open b");
    let started = Instant::now();
    let saved = loop {
        let _ = session.poll();
        if std::fs::read_to_string(&a).is_ok_and(|text| text.contains("hello")) {
            break started.elapsed();
        }
        assert!(started.elapsed() < LIMIT, "a.md was never saved");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(
        saved < Duration::from_millis(500),
        "saved after {saved:?}, i.e. by the debounce rather than the switch"
    );
}

/// nvim asks emeraldian for the note names (completion), and gets them.
#[test]
fn bridge_notes_request_is_answered() {
    let vault = TempVault::new("nvim-bridge-notes");
    let path = vault.write("a.md", "one\n");
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.set_notes(1, vec!["Alpha".to_string(), "Beta".to_string()]);
    session.open(&path).expect("open");

    let out = vault.path().join("names.txt");
    session.input(&format!(
        ":lua vim.fn.writefile(require('emeraldian').notes(), '{}')<CR>",
        out.display()
    ));
    let end = Instant::now() + LIMIT;
    while !out.exists() && Instant::now() < end {
        let _ = session.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read_to_string(&out).unwrap_or_default(),
        "Alpha\nBeta\n"
    );
}

/// After a link is followed, the cursor goes to the heading.
#[test]
fn follow_heading_anchor_jumps() {
    let vault = TempVault::new("nvim-bridge-anchor");
    let mut text = String::from("# Top\n");
    for i in 0..30 {
        text.push_str(&format!("line {i}\n"));
    }
    text.push_str("## Target\nbody\n");
    for i in 0..10 {
        text.push_str(&format!("tail {i}\n"));
    }
    let a = vault.write("a.md", &text);
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&a).expect("open");

    session.goto(&a, Anchor::Heading("target".to_string()));
    assert!(
        collect(&mut session, |session, _| {
            row_text(session.grid(), 0).starts_with("## Target")
        })
        .is_ok(),
        "row 0 was {:?}",
        row_text(session.grid(), 0)
    );
}

/// An anchor for a note that is not showing yet waits for it to be opened.
#[test]
fn block_anchor_applies_when_the_note_is_opened() {
    let vault = TempVault::new("nvim-bridge-block");
    let a = vault.write("a.md", "first\n");
    let mut text = String::new();
    for i in 0..30 {
        text.push_str(&format!("line {i}\n"));
    }
    text.push_str("the block ^blk1\nafter\n");
    let b = vault.write("b.md", &text);
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&a).expect("open a");

    session.goto(&b, Anchor::Block("blk1".to_string()));
    session.open(&b).expect("open b");
    assert!(
        collect(&mut session, |session, _| {
            row_text(session.grid(), 0).starts_with("the block ^blk1")
        })
        .is_ok(),
        "row 0 was {:?}",
        row_text(session.grid(), 0)
    );
}
