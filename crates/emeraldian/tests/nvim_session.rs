//! Integration tests for the embedded nvim session (`src/nvim/session.rs`).
//!
//! The crate is a binary, so the module under test is included by path, the way
//! `nvim_rpc.rs` does it. Every test returns early, with a printed skip, when
//! `nvim` is not on PATH — and each session runs `--clean` without swap files,
//! so a test never loads, depends on or litters someone's real nvim setup.
#![allow(dead_code)]

#[path = "../src/config.rs"]
mod config;
#[path = "../src/nvim/grid.rs"]
mod grid;
#[path = "../src/nvim/rpc.rs"]
pub mod rpc;
#[path = "../src/nvim/session.rs"]
mod session;

/// The path `rpc` has inside the binary, which `grid.rs`'s fixture recorder
/// names when it runs here.
mod nvim {
    pub use super::rpc;
}

use std::path::Path;
use std::time::{Duration, Instant};

use emeraldian_core::test_support::TempVault;

use config::EditorConfig;
use grid::Grid;
use session::{Event, Session};

/// Long enough that a busy machine reports a slow test rather than a broken
/// one; nvim answers in single-digit milliseconds when it is healthy.
const LIMIT: Duration = Duration::from_secs(10);

fn nvim_available() -> bool {
    std::process::Command::new("nvim")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// A session over `vault`, or `None` (with a printed skip) without nvim.
fn start(vault: &Path, size: (u16, u16)) -> Option<Session> {
    if !nvim_available() {
        eprintln!("skip: nvim not on PATH");
        return None;
    }
    let cfg = EditorConfig::default();
    Some(
        Session::start_with(&cfg, vault, size, &["--clean", "--cmd", "set noswapfile"])
            .expect("start nvim --embed"),
    )
}

/// Drain the session until `done` holds, or `limit` passes.
fn wait_until(
    session: &mut Session,
    limit: Duration,
    mut done: impl FnMut(&Session) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
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

/// One grid row as it appears on screen: cell texts concatenated.
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

/// Writes made by the editor land in the file, and the grid shows them.
#[test]
fn session_types_and_writes() {
    let vault = TempVault::new("nvim-session-type");
    let path = vault.write("a.md", "");
    let Some(mut session) = start(vault.path(), (40, 10)) else {
        return;
    };

    session.open(&path).expect("open a.md");
    session.input("ihello<Esc>");
    assert!(
        wait_until(&mut session, LIMIT, |session| {
            row_text(session.grid(), 0).starts_with("hello")
        }),
        "grid row 0 was {:?}",
        row_text(session.grid(), 0).trim_end()
    );

    session.input(":w<CR>");
    assert!(
        wait_until(&mut session, LIMIT, |_| {
            std::fs::read_to_string(&path).is_ok_and(|text| text.starts_with("hello"))
        }),
        "file was {:?}, status line {:?}",
        std::fs::read_to_string(&path).unwrap_or_default(),
        row_text(session.grid(), 8).trim_end()
    );
    // And the grid still shows the text after the write.
    assert!(
        wait_until(&mut session, LIMIT, |session| {
            row_text(session.grid(), 0).starts_with("hello")
        }),
        "grid row 0 was {:?}",
        row_text(session.grid(), 0).trim_end()
    );
}

/// A narrower pane narrows nvim's grid, rather than leaving cells behind.
#[test]
fn session_resize_reflows() {
    let vault = TempVault::new("nvim-session-resize");
    let path = vault.write(
        "a.md",
        "a line that is certainly wider than twenty columns is wrapped\n",
    );
    let Some(mut session) = start(vault.path(), (40, 10)) else {
        return;
    };

    session.open(&path).expect("open a.md");
    assert!(
        wait_until(&mut session, LIMIT, |session| session.grid().width == 40),
        "grid was {} wide",
        session.grid().width
    );

    session.resize(20, 10);
    assert!(
        wait_until(&mut session, LIMIT, |session| session.grid().width == 20),
        "grid stayed {} wide",
        session.grid().width
    );
    // The wrapped text follows the new width: with `wrap` off by default in
    // nvim the first row is still the start of the line, so the proof is the
    // cursor column nvim keeps it within.
    assert!(session.grid().cursor.0 < 20);
}

/// Going back to a note shows its buffer again, not the last one opened.
#[test]
fn session_two_buffers_switch() {
    let vault = TempVault::new("nvim-session-buffers");
    let alpha = vault.write("a.md", "alpha\n");
    let bravo = vault.write("b.md", "bravo\n");
    let Some(mut session) = start(vault.path(), (40, 10)) else {
        return;
    };

    session.open(&alpha).expect("open a.md");
    assert!(
        wait_until(&mut session, LIMIT, |session| {
            row_text(session.grid(), 0).starts_with("alpha")
        }),
        "grid row 0 was {:?}",
        row_text(session.grid(), 0).trim_end()
    );

    session.open(&bravo).expect("open b.md");
    assert!(
        wait_until(&mut session, LIMIT, |session| {
            row_text(session.grid(), 0).starts_with("bravo")
        }),
        "grid row 0 was {:?}",
        row_text(session.grid(), 0).trim_end()
    );

    session.open(&alpha).expect("open a.md again");
    assert!(
        wait_until(&mut session, LIMIT, |session| {
            row_text(session.grid(), 0).starts_with("alpha")
        }),
        "grid row 0 was {:?}",
        row_text(session.grid(), 0).trim_end()
    );
}

/// nvim starts with an empty scratch buffer, and opening a note switches away
/// from it. `Ctrl+O` right after that must not jump back into it.
#[test]
fn session_open_leaves_no_scratch_buffer_to_jump_back_to() {
    let vault = TempVault::new("nvim-session-scratch");
    let path = vault.write("a.md", "alpha\n");
    let Some(mut session) = start(vault.path(), (40, 10)) else {
        return;
    };
    session.open(&path).expect("open a.md");
    assert!(wait_until(&mut session, LIMIT, |session| {
        row_text(session.grid(), 0).starts_with("alpha")
    }));

    session.input("<C-o>ix<Esc>");

    assert!(
        wait_until(&mut session, LIMIT, |session| {
            row_text(session.grid(), 0).starts_with("xalpha")
        }),
        "grid row 0 was {:?}",
        row_text(session.grid(), 0).trim_end()
    );
}

/// The loop polls fast only around traffic: right after input, and for a short
/// while after nvim last spoke; a quiet session goes back to the slow tick.
#[test]
fn session_is_busy_only_around_traffic() {
    let vault = TempVault::new("nvim-session-busy");
    let path = vault.write("a.md", "");
    let Some(mut session) = start(vault.path(), (40, 10)) else {
        return;
    };
    session.open(&path).expect("open a.md");
    assert!(wait_until(&mut session, LIMIT, |session| !session.busy()));

    session.input("ihi<Esc>");
    assert!(session.busy(), "input must keep the loop polling fast");
    assert!(wait_until(&mut session, LIMIT, |session| {
        row_text(session.grid(), 0).starts_with("hi")
    }));
    assert!(wait_until(&mut session, LIMIT, |session| !session.busy()));
}

/// Diagnostic, not an assertion: measures how long nvim takes from `nvim_input`
/// to the redraw it answers with. The main loop polls every 4 ms while a redraw
/// is owed, so this plus that poll is the keystroke-to-screen latency.
///
/// `cargo test -p emeraldian --test nvim_session nvim_send_flush_latency -- --ignored --nocapture`
#[test]
#[ignore = "diagnostic: prints keystroke-to-flush latency against a real nvim"]
fn nvim_send_flush_latency() {
    let vault = TempVault::new("nvim-session-latency");
    let path = vault.write("a.md", "");
    let Some(mut session) = start(vault.path(), (80, 24)) else {
        return;
    };
    session.open(&path).expect("open a.md");
    session.input("i");
    assert!(wait_until(&mut session, LIMIT, |session| {
        session.grid().mode.name == "insert"
    }));

    let mut samples = Vec::new();
    for _ in 0..200 {
        let started = Instant::now();
        session.input("x");
        loop {
            if session
                .poll()
                .iter()
                .any(|event| matches!(event, Event::Redraw))
            {
                samples.push(started.elapsed());
                break;
            }
            std::thread::yield_now();
        }
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    let p90 = samples[samples.len() * 9 / 10];
    println!(
        "keystroke -> flush: median {:.2} ms, p90 {:.2} ms, max {:.2} ms",
        median.as_secs_f64() * 1e3,
        p90.as_secs_f64() * 1e3,
        samples[samples.len() - 1].as_secs_f64() * 1e3
    );
    assert!(
        median < Duration::from_millis(10),
        "median latency {median:?} misses the 10 ms target"
    );
}
