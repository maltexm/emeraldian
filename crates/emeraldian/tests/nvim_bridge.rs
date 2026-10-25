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

/// `]l` / `[l` move the cursor from link to link, wrapping, and `<CR>` on the
/// landed link still follows it. Insert mode does not have the map.
#[test]
fn bracket_l_jumps_between_links() {
    let vault = TempVault::new("nvim-bridge-jump");
    let path = vault.write(
        "a.md",
        "see [[Alpha]] first\n\nthen [there](Beta.md) and [[Gamma]]\n",
    );
    let Some(mut session) = start(vault.path(), false) else {
        return;
    };
    session.open(&path).expect("open");
    collect(&mut session, |_, seen| {
        seen.iter()
            .any(|event| matches!(event, BridgeEvent::Entered(_)))
    })
    .expect("note entered");

    session.input("]l");
    let landed = collect(&mut session, |session, _| {
        row_text(session.grid(), session.grid().cursor.1).contains("Alpha")
    });
    let landed = landed.expect("cursor on Alpha");
    let row = session.grid().cursor.1;
    assert!(
        row_text(session.grid(), row).contains("Alpha"),
        "first ]l should land on Alpha, row {row}: {:?}",
        row_text(session.grid(), row)
    );

    session.input("]l<CR>");
    let seen = collect(&mut session, |_, seen| !follows(seen).is_empty())
        .expect("Follow after the second ]l");
    assert!(
        matches!(
            follows(&seen).first(),
            Some(BridgeEvent::Follow(target)) if target.note.contains("Beta")
        ),
        "second ]l should land on the markdown link, saw {landed:?}"
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

// ---------------------------------------------------------------------------
// Maps the user had before the bridge took `<CR>`, `gf` and `<C-]>` over
// ---------------------------------------------------------------------------

/// `start`, plus user config that is there before the bridge and before any
/// note is opened, the way the maps of an init file are: each entry is run as
/// an Ex command through `--cmd`.
fn start_configured(vault: &Path, config: &[&str]) -> Option<Session> {
    if !nvim_available() {
        eprintln!("skip: nvim not on PATH");
        return None;
    }
    let mut args = vec!["--clean", "--cmd", "set noswapfile"];
    for command in config {
        args.push("--cmd");
        args.push(command);
    }
    Some(
        Session::start_with(&EditorConfig::default(), vault, (60, 12), &args)
            .expect("start nvim --embed"),
    )
}

/// The row of the marker `A!<Esc>`, if one is on screen.
fn marker_row(grid: &Grid) -> Option<u16> {
    (0..grid.height).find(|&row| row_text(grid, row).contains('!'))
}

/// Types `keys` and then a marker behind them. The marker lands where the
/// cursor is once `keys` are done, so this answers where they left it, and
/// that they are done: a key that has not taken effect yet cannot be told from
/// one that never will.
fn row_after(session: &mut Session, keys: &str) -> (u16, Vec<BridgeEvent>) {
    session.input(&format!("{keys}A!<Esc>"));
    let mut row = None;
    let seen = collect(session, |session, _| {
        row = marker_row(session.grid());
        row.is_some()
    });
    let seen = seen.unwrap_or_else(|seen| seen);
    (
        row.unwrap_or_else(|| panic!("no marker after {keys:?}; events were {seen:?}")),
        seen,
    )
}

const THREE_LINES: &str = "one\ntwo\nthree\n";
const FIVE_LINES: &str = "l0\nl1\nl2\nl3\nl4\n";

/// Opens `text` as a note after `config` and types `keys`: the row the cursor
/// ends on, and what the bridge reported.
fn keys_with_config(config: &[&str], text: &str, keys: &str) -> Option<(u16, Vec<BridgeEvent>)> {
    let vault = TempVault::new("nvim-bridge-premap");
    let path = vault.write("Expr.md", text);
    let mut session = start_configured(vault.path(), config)?;
    session.open(&path).expect("open");
    Some(row_after(&mut session, keys))
}

/// An expression map the user had made for the key stays theirs when no link
/// is under the cursor: a Lua callback or an expression string, on each of the
/// three keys. (`G` is the same, typed without any map: row 2.)
#[test]
fn bridge_keeps_a_users_expression_map_without_a_link() {
    let cases: [(&str, &str); 6] = [
        (
            "lua vim.keymap.set('n', 'gf', function() return 'G' end, { expr = true })",
            "gg0gf",
        ),
        ("nnoremap <expr> gf 'G'", "gg0gf"),
        (
            "lua vim.keymap.set('n', '<CR>', function() return 'G' end, { expr = true })",
            "gg0<CR>",
        ),
        ("nnoremap <expr> <CR> 'G'", "gg0<CR>"),
        (
            "lua vim.keymap.set('n', '<C-]>', function() return 'G' end, { expr = true })",
            "gg0<C-]>",
        ),
        ("nnoremap <expr> <C-]> 'G'", "gg0<C-]>"),
    ];
    for (config, keys) in cases {
        let Some((row, seen)) = keys_with_config(&[config], THREE_LINES, keys) else {
            return;
        };
        assert_eq!(row, 2, "{config} then {keys}");
        assert!(follows(&seen).is_empty(), "{config}: events were {seen:?}");
    }
}

/// What an expression map returns is typed as nvim types it for the map
/// itself: keycodes are translated, a count typed before the key still counts,
/// and the result is remapped only when the map is a remappable one.
#[test]
fn bridge_replays_expression_maps_as_nvim_would() {
    let cases: [(&[&str], &str, u16, &str); 8] = [
        (
            &[
                "lua vim.keymap.set('n', 'gf', function() return '<Down><Down>' end, { expr = true })",
            ],
            "gf",
            2,
            "lua expression, key notation in the result",
        ),
        (
            &[r#"nnoremap <expr> gf "\<Down>\<Down>""#],
            "gf",
            2,
            "expression string, real keycodes in the result",
        ),
        (
            &[
                "lua vim.keymap.set('n', 'gf', function() return vim.api.nvim_replace_termcodes('<Down><Down>', true, false, true) end, { expr = true, replace_keycodes = false })",
            ],
            "gf",
            2,
            "lua expression that already translated its keys",
        ),
        (
            &["nnoremap <expr> gf 'j'"],
            "3gf",
            3,
            "a count typed before the key",
        ),
        (
            &[
                "lua vim.keymap.set('n', 'gf', function() return vim.v.count .. 'j' end, { expr = true })",
            ],
            "2gf",
            // `2` typed, then the result `2j`: the digits join to `22j`, as for the
            // map itself, and the buffer ends at the last row.
            4,
            "v:count seen by the callback",
        ),
        (
            &["nnoremap <F9> G", "nmap <expr> gf '<F9>'"],
            "gg0gf",
            4,
            "a remappable expression map's result is remapped",
        ),
        (
            &["nnoremap <F9> G", "nnoremap <expr> gf '<F9>'"],
            "gg0gf",
            0,
            "a noremap expression map's result is not",
        ),
        (
            &[
                "nnoremap <F9> G",
                "lua vim.keymap.set('n', 'gf', function() return '<F9>' end, { expr = true, remap = true })",
            ],
            "gg0gf",
            4,
            "a remappable lua expression map's result is remapped",
        ),
    ];
    for (config, keys, want, what) in cases {
        let Some((row, _)) = keys_with_config(config, FIVE_LINES, keys) else {
            return;
        };
        assert_eq!(row, want, "{what}: {config:?} then {keys}");
    }
}

/// Runs the user script `script` (`name` says whether it is Lua or Vim script)
/// before the bridge, opens `text` as a note and types `keys`. `{calls}` in the
/// script is a file it counts its evaluations in. Returns the row the cursor
/// ended on and that count.
///
/// A script that loops has to stop itself: every counting script here ends the
/// loop at 3000 evaluations (the "fuse"), so a regression shows as a wrong
/// count rather than a hang. `row_after` is the watchdog on top of that: it
/// gives up after `LIMIT` and `Session` kills nvim when the test unwinds.
fn keys_with_script(name: &str, script: &str, text: &str, keys: &str) -> Option<(u16, usize)> {
    let vault = TempVault::new("nvim-bridge-script");
    let note = vault.write("Loop.md", text);
    let calls = vault.path().join("calls.txt");
    let file = vault.write(
        name,
        &script.replace("{calls}", &calls.display().to_string()),
    );
    let command = format!(
        "{} {}",
        if name.ends_with(".lua") {
            "luafile"
        } else {
            "source"
        },
        file.display()
    );
    let mut session = start_configured(vault.path(), &[&command])?;
    session.open(&note).expect("open");
    let (row, _) = row_after(&mut session, keys);
    let count = std::fs::read_to_string(&calls)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0);
    Some((row, count))
}

/// A remappable expression map whose result starts with its own key: nvim
/// evaluates it once and then runs the key's default, whatever
/// `maxmapdepth` says. The bridge must not turn that into a loop.
#[test]
fn bridge_expression_map_returning_its_own_key_runs_once() {
    let lua = "vim.o.maxmapdepth = 50\n\
        local calls = 0\n\
        vim.keymap.set('n', '<CR>', function()\n\
          calls = calls + 1\n\
          vim.fn.writefile({ tostring(calls) }, '{calls}')\n\
          if calls >= 3000 then return '' end\n\
          return '<CR>'\n\
        end, { expr = true, remap = true })\n";
    let vim = "set maxmapdepth=50\n\
        let g:calls = 0\n\
        function! Bump()\n\
          let g:calls += 1\n\
          call writefile([g:calls], '{calls}')\n\
          if g:calls >= 3000 | return '' | endif\n\
          return \"\\<CR>\"\n\
        endfunction\n\
        nmap <expr> <CR> Bump()\n";
    let cases = [
        ("lua.lua", lua, "gg0<CR>", 1),
        ("lua.lua", lua, "gg03<CR>", 3),
        ("string.vim", vim, "gg0<CR>", 1),
        ("string.vim", vim, "gg02<CR>", 2),
        // Typed ahead, each press is its own evaluation and its own default.
        ("lua.lua", lua, "gg0<CR><CR>", 2),
        ("string.vim", vim, "gg0<CR><CR>", 2),
    ];
    for (name, script, keys, want_row) in cases {
        let Some((row, calls)) = keys_with_script(name, script, FIVE_LINES, keys) else {
            return;
        };
        assert_eq!(
            calls,
            keys.matches("<CR>").count(),
            "{name} then {keys}: evaluations"
        );
        assert_eq!(row, want_row, "{name} then {keys}: the key's default ran");
    }
}

/// A chain of maps that comes back to the key is cut by `maxmapdepth`, as it is
/// without the bridge (E223), and the editor stays usable.
#[test]
fn bridge_cuts_a_chain_of_maps_that_returns_to_the_key() {
    let lua = "vim.o.maxmapdepth = 50\n\
        local calls = 0\n\
        vim.keymap.set('n', 'gf', function()\n\
          calls = calls + 1\n\
          vim.fn.writefile({ tostring(calls) }, '{calls}')\n\
          if calls >= 3000 then return '' end\n\
          return 'x'\n\
        end, { expr = true, remap = true })\n\
        vim.keymap.set('n', 'x', 'gf', { remap = true })\n";
    let Some((row, calls)) = keys_with_script("chain.lua", lua, THREE_LINES, "gg0gf") else {
        return;
    };
    assert_eq!(row, 0, "nothing moved");
    assert!(
        (2..=60).contains(&calls),
        "the chain ran {calls} times, maxmapdepth is 50"
    );
}

/// The same for a plain string right-hand side, which starts with its own key
/// (`nmap <CR> <CR>j`: the default, then `j`) or comes back to it.
#[test]
fn bridge_plain_map_returning_to_its_own_key_ends() {
    let cases: [(&[&str], &str, u16, &str); 3] = [
        (
            &["nmap <CR> <CR>j"],
            "gg0<CR>",
            2,
            "remappable, starts with the key",
        ),
        (
            &["nnoremap <CR> <CR>j"],
            "gg0<CR>",
            2,
            "noremap, starts with the key",
        ),
        (
            &["set maxmapdepth=50", "nmap gf x", "nmap x gf"],
            "gg0gf",
            0,
            "a chain back to the key",
        ),
    ];
    for (config, keys, want, what) in cases {
        let Some((row, _)) = keys_with_config(config, FIVE_LINES, keys) else {
            return;
        };
        assert_eq!(row, want, "{what}: {config:?} then {keys}");
    }
}

/// `<SID>` in a user's map means the script it was written in: an expression
/// string that calls a script function, and a plain right-hand side that goes
/// through a `<SID>` map.
#[test]
fn bridge_replays_maps_that_use_sid() {
    let expr = "function! s:Last()\n  return 'G'\nendfunction\n\
        nnoremap <expr> gf <SID>Last()\n";
    let plain = "nnoremap <SID>last G\nnmap gf <SID>last\n";
    for (script, what) in [(expr, "expression"), (plain, "plain")] {
        let Some((row, _)) = keys_with_script("sid.vim", script, FIVE_LINES, "gg0gf") else {
            return;
        };
        assert_eq!(row, 4, "{what}");
    }
}

/// The two kinds of map that already worked still do: a Lua callback that is
/// no expression, and a plain string for the right-hand side.
#[test]
fn bridge_keeps_a_users_plain_maps_without_a_link() {
    let cases: [(&str, &str); 3] = [
        (
            "lua vim.keymap.set('n', '<CR>', function() vim.api.nvim_win_set_cursor(0, { 3, 0 }) end)",
            "gg0<CR>",
        ),
        ("nnoremap gf G", "gg0gf"),
        ("nmap <C-]> G", "gg0<C-]>"),
    ];
    for (config, keys) in cases {
        let Some((row, seen)) = keys_with_config(&[config], THREE_LINES, keys) else {
            return;
        };
        assert_eq!(row, 2, "{config} then {keys}");
        assert!(follows(&seen).is_empty(), "{config}: events were {seen:?}");
    }
}

/// A key the user mapped nothing to keeps its default, whatever else they
/// mapped: `<CR>` still goes down a line.
#[test]
fn bridge_default_stays_next_to_an_expression_map_on_another_key() {
    let Some((row, seen)) = keys_with_config(&["nnoremap <expr> gf 'G'"], THREE_LINES, "gg0<CR>")
    else {
        return;
    };
    assert_eq!(row, 1);
    assert!(follows(&seen).is_empty(), "events were {seen:?}");
}

/// A link under the cursor is followed, and the user's map does not run: on
/// every key, for an expression map as for the others.
#[test]
fn bridge_link_wins_over_a_users_map() {
    let text = "see [[Alpha]] now\ntwo\nthree\n";
    let cases: [(&str, &str); 4] = [
        (
            "lua vim.keymap.set('n', 'gf', function() return 'G' end, { expr = true })",
            "gf",
        ),
        ("nnoremap <expr> <CR> 'G'", "<CR>"),
        ("nnoremap <expr> <C-]> 'G'", "<C-]>"),
        ("nnoremap gf G", "gf"),
    ];
    for (config, key) in cases {
        let Some((row, seen)) = keys_with_config(&[config], text, &format!("f[{key}")) else {
            return;
        };
        assert_eq!(
            follows(&seen),
            [&BridgeEvent::Follow(LinkTarget {
                note: "Alpha".to_string(),
                anchor: None,
                source: None,
            })],
            "{config} then {key}"
        );
        assert_eq!(row, 0, "{config}: the map ran on a link");
    }
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
