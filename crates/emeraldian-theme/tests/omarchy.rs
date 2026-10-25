//! The Omarchy-following palette, against copies of the real `colors.toml`
//! files: every stock theme under `/usr/share/omarchy/themes` plus the live
//! Aether theme of the machine this was written on. Fixtures are copies, so the
//! tests never read the machine they run on.

use std::fs;
use std::path::{Path, PathBuf};

use emeraldian_theme::color::{contrast, to_hex};
use emeraldian_theme::contrast::PAIRS;
use emeraldian_theme::omarchy::{self, Colors, Poll, Source};
use emeraldian_theme::{Palette, Theme};
use ratatui::style::Color;

fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omarchy");
    let mut found: Vec<_> = fs::read_dir(&dir)
        .expect("fixture directory")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .map(|e| {
            let name = e.path().file_stem().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(e.path()).expect("fixture"))
        })
        .collect();
    found.sort();
    found
}

fn fixture(name: &str) -> String {
    fixtures()
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no fixture {name}"))
        .1
}

fn derived(name: &str) -> (Colors, Theme, Palette) {
    let colors = Colors::parse(&fixture(name)).expect("fixture parses");
    let theme = omarchy::theme(&colors);
    let palette = Palette::from(&theme);
    (colors, theme, palette)
}

fn hex(color: Color) -> String {
    to_hex(color).expect("true colour")
}

#[test]
fn there_are_fixtures_for_every_stock_theme_and_the_live_one() {
    let names: Vec<_> = fixtures().into_iter().map(|(n, _)| n).collect();
    for wanted in [
        "aether-live",
        "tokyo-night",
        "catppuccin-latte",
        "gruvbox",
        "white",
    ] {
        assert!(names.iter().any(|n| n == wanted), "missing {wanted}");
    }
    assert!(names.len() >= 24, "only {} fixtures", names.len());
}

#[test]
fn every_fixture_parses() {
    for (name, text) in fixtures() {
        Colors::parse(&text).unwrap_or_else(|err| panic!("{name}: {err}"));
    }
}

#[test]
fn parse_reads_roles_and_mode() {
    let colors = Colors::parse(&fixture("catppuccin-latte")).expect("parses");
    assert!(!colors.dark);
    assert_eq!(hex(colors.background), "#eff1f5");
    assert_eq!(hex(colors.foreground), "#4c4f69");
    assert_eq!(hex(colors.accent), "#1e66f5");
    assert_eq!(hex(colors.muted), "#acb0be");
    let colors = Colors::parse(&fixture("aether-live")).expect("parses");
    assert!(colors.dark);
    assert_eq!(hex(colors.lighter_background), "#111118");
}

#[test]
fn parse_derives_mode_from_the_background_when_it_is_missing() {
    let text = "background = \"#ffffff\"\nforeground = \"#000000\"\naccent = \"#0000ff\"\n";
    assert!(!Colors::parse(text).expect("parses").dark);
    let text = "background = \"#101010\"\nforeground = \"#eeeeee\"\naccent = \"#0000ff\"\n";
    assert!(Colors::parse(text).expect("parses").dark);
}

#[test]
fn parse_needs_background_foreground_and_accent() {
    for missing in ["background", "foreground", "accent"] {
        let text: String = ["background", "foreground", "accent"]
            .iter()
            .filter(|k| **k != missing)
            .map(|k| format!("{k} = \"#123456\"\n"))
            .collect();
        let err = Colors::parse(&text).expect_err("partial file is refused");
        assert!(err.contains(missing), "{err}");
    }
}

#[test]
fn parse_refuses_garbage_and_torn_files() {
    assert!(Colors::parse("").is_err());
    assert!(Colors::parse("this is not toml").is_err());
    // A write caught half way: the quote is not closed.
    assert!(Colors::parse("background = \"#00").is_err());
    let bad = "background = \"nope\"\nforeground = \"#000000\"\naccent = \"#0000ff\"\n";
    assert!(Colors::parse(bad).is_err(), "an unreadable required colour");
}

#[test]
fn parse_ignores_an_unreadable_optional_colour_and_falls_back() {
    let text = "background = \"#101010\"\nforeground = \"#eeeeee\"\naccent = \"#0000ff\"\n\
                red = \"#ff0000\"\nyellow = \"#ffff00\"\norange = \"oops\"\n";
    let colors = Colors::parse(text).expect("parses");
    assert_eq!(colors.orange, colors.yellow, "orange falls back to yellow");
    let theme = omarchy::theme(&colors);
    assert!(!theme.h1.is_empty());
}

#[test]
fn headings_use_omarchys_obsidian_hues() {
    // obsidian.css.tpl: H1 red, H2 green, H3 yellow, H4 blue, H5 and H6
    // magenta. Checked on the live theme, where nothing needs adjusting.
    let (colors, _, palette) = derived("aether-live");
    assert_eq!(palette.h1, colors.red);
    assert_eq!(palette.h2, colors.green);
    assert_eq!(palette.h3, colors.yellow);
    assert_eq!(palette.h4, colors.blue);
    assert_eq!(palette.h5, colors.magenta);
    assert_eq!(palette.h6, colors.magenta);
    assert_eq!(palette.link, colors.blue);
    assert_eq!(palette.accent, colors.accent);
    assert_eq!(palette.code_fg, colors.cyan);
    assert_eq!(palette.quote_bar, colors.accent);
}

#[test]
fn the_page_and_body_text_stay_the_terminals_own() {
    // Kitty may be translucent and Omarchy re-themes it live, so the page and
    // the body text are left to the terminal; everything else is explicit.
    let (_, theme, palette) = derived("aether-live");
    assert_eq!(theme.name, "terminal");
    assert_eq!(palette.bg_primary, Color::Reset);
    assert_eq!(palette.text_normal, Color::Reset);
}

#[test]
fn muted_text_differs_from_body_text_and_from_faint_text() {
    for (name, _) in fixtures() {
        let (colors, _, palette) = derived(&name);
        assert_ne!(
            palette.text_muted, colors.foreground,
            "{name}: muted == body"
        );
        assert_ne!(
            palette.text_muted, palette.text_faint,
            "{name}: muted == faint"
        );
        assert_ne!(palette.quote_fg, colors.foreground, "{name}: quote == body");
    }
}

#[test]
fn code_blocks_and_callouts_get_a_subtle_panel() {
    for (name, _) in fixtures() {
        let (colors, _, palette) = derived(&name);
        assert_ne!(palette.code_bg, Color::Reset, "{name}");
        assert_ne!(palette.code_bg, colors.background, "{name}: no code panel");
        assert_ne!(palette.callout_bg, Color::Reset, "{name}");
        assert_ne!(
            palette.callout_bg, colors.background,
            "{name}: no callout panel"
        );
        // Subtle: much closer to the page than to the text.
        let page = contrast(palette.code_bg, colors.background).expect("rgb");
        assert!(
            page < 2.0,
            "{name}: the panel is a different surface ({page:.2}:1)"
        );
    }
}

#[test]
fn every_reading_view_pair_reaches_its_contrast() {
    // The table test: all fixtures, every pair `PAIRS` lists. A failure prints
    // the whole row so the number to fix is in the message.
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, _) in fixtures() {
        let (colors, _, palette) = derived(&name);
        for check in palette.contrast_report(colors.background, colors.foreground) {
            checked += 1;
            let ratio = check.ratio.expect("every Omarchy colour is measurable");
            if ratio + 1e-9 < check.pair.min {
                failures.push(format!(
                    "{name}: {} on {} = {ratio:.2}:1, needs {}",
                    check.pair.fg, check.pair.bg, check.pair.min
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked}:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked > 24 * 40, "only {checked} pairs were measured");
}

#[test]
fn body_text_reaches_4_5_and_every_other_pair_3() {
    for pair in PAIRS {
        if pair.fg == "text_normal" && pair.bg == "bg_primary" {
            assert!(pair.min >= 4.5, "body text");
        } else {
            assert!(pair.min >= 3.0, "{} on {}", pair.fg, pair.bg);
        }
    }
    let wanted = [
        "tag_fg",
        "callout_note",
        "callout_danger",
        "link",
        "text_muted",
        "text_faint",
        "text_on_accent",
        "text_highlight_fg",
    ];
    for fg in wanted {
        assert!(PAIRS.iter().any(|p| p.fg == fg), "no pair for {fg}");
    }
    assert!(PAIRS.iter().any(|p| p.bg == "bg_selection"));
}

#[test]
fn the_latte_tag_chip_is_readable() {
    // Before: magenta #ea76cb on muted #acb0be, 1.22:1.
    let (_, _, palette) = derived("catppuccin-latte");
    let ratio = contrast(palette.tag_fg, palette.tag_bg).expect("rgb");
    assert!(ratio >= 4.5, "tag chip {ratio:.2}:1");
}

#[test]
fn a_dark_and_a_light_theme_keep_their_character() {
    let (_, _, light) = derived("catppuccin-latte");
    let (_, _, dark) = derived("tokyo-night");
    assert!(!light.dark);
    assert!(dark.dark);
    // The light theme's accent colour is the theme's own, not magenta.
    assert_eq!(hex(light.accent), "#1e66f5");
}

#[test]
fn theme_dir_follows_the_override_then_home() {
    assert_eq!(
        omarchy::theme_dir_from(Some("/tmp/x"), Some("/home/m")),
        Some(PathBuf::from("/tmp/x"))
    );
    assert_eq!(
        omarchy::theme_dir_from(Some(""), Some("/home/m")),
        Some(PathBuf::from("/home/m/.local/state/omarchy/current/theme"))
    );
    assert_eq!(
        omarchy::theme_dir_from(None, Some("/home/m")),
        Some(PathBuf::from("/home/m/.local/state/omarchy/current/theme"))
    );
    assert_eq!(omarchy::theme_dir_from(None, None), None);
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("emeraldian-omarchy-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn a_missing_theme_dir_is_quietly_missing() {
    let dir = scratch("missing");
    let mut source = Source::new(dir.join("nothing-here"));
    assert!(matches!(source.poll(), Poll::Missing));
    assert!(matches!(source.poll(), Poll::Missing), "and it stays so");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn polling_loads_once_then_reports_unchanged() {
    let dir = scratch("once");
    fs::write(dir.join("colors.toml"), fixture("gruvbox")).expect("write");
    let mut source = Source::new(dir.clone());
    let Poll::Loaded(colors) = source.poll() else {
        panic!("first poll loads")
    };
    assert_eq!(hex(colors.background), "#282828");
    assert!(matches!(source.poll(), Poll::Unchanged));
    assert!(matches!(source.poll(), Poll::Unchanged));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_theme_switch_in_place_is_noticed() {
    let dir = scratch("switch");
    let file = dir.join("colors.toml");
    fs::write(&file, fixture("gruvbox")).expect("write");
    let mut source = Source::new(dir.clone());
    assert!(matches!(source.poll(), Poll::Loaded(_)));

    // Omarchy rewrites the file. Same inode, new content and length.
    fs::write(&file, fixture("catppuccin-latte")).expect("rewrite");
    let Poll::Loaded(colors) = source.poll() else {
        panic!("rewrite is noticed")
    };
    assert_eq!(hex(colors.background), "#eff1f5");
    fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn a_theme_switch_by_symlink_swap_is_noticed() {
    // Omarchy can also swap what `current/theme` points at: a different inode
    // behind the same path, possibly with the same mtime and length.
    let root = scratch("symlink");
    let (a, b) = (root.join("a"), root.join("b"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let same_length = fixture("gruvbox");
    fs::write(a.join("colors.toml"), &same_length).unwrap();
    fs::write(
        b.join("colors.toml"),
        same_length.replace("#282828", "#292929"),
    )
    .unwrap();
    let link = root.join("theme");
    std::os::unix::fs::symlink(&a, &link).unwrap();

    let mut source = Source::new(link.clone());
    let Poll::Loaded(first) = source.poll() else {
        panic!("loads")
    };
    assert_eq!(hex(first.background), "#282828");

    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&b, &link).unwrap();
    let Poll::Loaded(second) = source.poll() else {
        panic!("swap is noticed")
    };
    assert_eq!(hex(second.background), "#292929");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn a_file_caught_mid_write_is_retried_not_trusted() {
    let dir = scratch("torn");
    let file = dir.join("colors.toml");
    fs::write(&file, fixture("gruvbox")).expect("write");
    let mut source = Source::new(dir.clone());
    assert!(matches!(source.poll(), Poll::Loaded(_)));

    // The writer truncated and has written half a line.
    fs::write(&file, "background = \"#00").expect("torn write");
    assert!(matches!(source.poll(), Poll::Failed(_)));
    assert!(
        matches!(source.poll(), Poll::Failed(_)),
        "a failure is not remembered as 'seen': the next poll tries again"
    );

    // The writer finishes.
    fs::write(&file, fixture("tokyo-night")).expect("finish");
    let Poll::Loaded(colors) = source.poll() else {
        panic!("the finished file loads")
    };
    assert_eq!(hex(colors.background), "#1a1b26");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_vanished_file_is_missing_and_a_returning_one_loads() {
    let dir = scratch("vanish");
    let file = dir.join("colors.toml");
    fs::write(&file, fixture("gruvbox")).expect("write");
    let mut source = Source::new(dir.clone());
    assert!(matches!(source.poll(), Poll::Loaded(_)));

    fs::remove_file(&file).expect("remove");
    assert!(matches!(source.poll(), Poll::Missing));

    fs::write(&file, fixture("gruvbox")).expect("restore");
    assert!(
        matches!(source.poll(), Poll::Loaded(_)),
        "same bytes again, but after a gap: reload"
    );
    fs::remove_dir_all(&dir).ok();
}
