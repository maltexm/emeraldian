//! Following the active Omarchy theme.
//!
//! Omarchy keeps the colors of the theme in use in
//! `~/.local/state/omarchy/current/theme/colors.toml` and renders every
//! application's theme from them. [`Colors`] reads that file, [`theme`] turns it
//! into an emeraldian [`Theme`] by role (Omarchy's own Obsidian template decides
//! which hue a heading or link gets), and [`Source`] notices when the file
//! changes, so a running emeraldian can recolour when the theme is switched.
//!
//! The page background and body text are left as `reset`: the terminal paints
//! them (and may be translucent), and Omarchy recolours it in the same switch.
//! Every other role is an explicit color, because the terminal's 16 colors have
//! no slot for them: Omarchy maps its `foreground` to ANSI 7, so "muted" text
//! drawn with an ANSI name comes out as body text.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;

use ratatui::style::Color;

use crate::color::{blend, luminance, parse, to_hex};
use crate::contrast::best_on;
use crate::seed;
use crate::theme::Theme;

/// Environment variable that points at a directory holding a `colors.toml`,
/// used instead of Omarchy's own. For tests and for trying a theme without
/// switching to it.
pub const THEME_DIR_ENV: &str = "EMERALDIAN_OMARCHY_THEME_DIR";

/// The roles of an Omarchy theme that emeraldian reads, all true-color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colors {
    pub dark: bool,
    pub background: Color,
    pub dark_background: Color,
    pub darker_background: Color,
    pub lighter_background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub selection: Color,
    pub muted: Color,
    pub red: Color,
    pub orange: Color,
    pub yellow: Color,
    pub green: Color,
    pub cyan: Color,
    pub blue: Color,
    pub magenta: Color,
    pub bright_blue: Color,
}

impl Colors {
    /// Reads the text of a `colors.toml`.
    ///
    /// `background`, `foreground` and `accent` are required. Every other role
    /// that is missing or unreadable is derived from those or from a
    /// neighbouring hue (`orange` is missing in a few stock themes). A file
    /// that is not TOML, or lacks a required color, is an error: that is also
    /// what a file caught half way through being written looks like.
    pub fn parse(text: &str) -> Result<Self, String> {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| format!("colors.toml: {}", e.message()))?;
        let rgb = |key: &str| {
            table
                .get(key)
                .and_then(toml::Value::as_str)
                .map(parse)
                .filter(|color| matches!(color, Color::Rgb(..)))
        };
        let required = |key: &str| rgb(key).ok_or_else(|| format!("no usable `{key}` color"));

        let background = required("background")?;
        let foreground = required("foreground")?;
        let accent = required("accent")?;

        let dark = match table.get("mode").and_then(toml::Value::as_str) {
            Some("light") => false,
            Some("dark") => true,
            _ => luminance(background).is_none_or(|l| l < 0.5),
        };

        let near = |t: f32| blend(background, foreground, t);
        let hue = |key: &str| rgb(key).unwrap_or(accent);
        let yellow = hue("yellow");
        let blue = hue("blue");
        Ok(Self {
            dark,
            background,
            dark_background: rgb("dark_background").unwrap_or(background),
            darker_background: rgb("darker_background").unwrap_or(background),
            lighter_background: rgb("lighter_background").unwrap_or_else(|| near(0.08)),
            foreground,
            accent,
            selection: rgb("selection").unwrap_or_else(|| near(0.15)),
            muted: rgb("muted").unwrap_or_else(|| near(0.45)),
            red: hue("red"),
            orange: rgb("orange").unwrap_or(yellow),
            yellow,
            green: hue("green"),
            cyan: hue("cyan"),
            blue,
            magenta: hue("magenta"),
            bright_blue: rgb("bright_blue").unwrap_or(blue),
        })
    }
}

fn hex(color: Color) -> String {
    to_hex(color).unwrap_or_else(|| "reset".into())
}

/// How far toward the text color the code panel and the callout panel sit from
/// the page. A few percent: a surface you notice only when it is missing.
const CODE_PANEL: f32 = 0.08;
const CALLOUT_PANEL: f32 = 0.06;

/// The emeraldian theme for an Omarchy palette, named `terminal`: it replaces
/// the plain-ANSI theme of that name while Omarchy's colors are available.
///
/// Roles follow Omarchy's own `obsidian.css.tpl`: H1 red, H2 green, H3 yellow,
/// H4 blue, H5 and H6 magenta, links blue, accent and quote bar `accent`, code
/// and tags cyan. Muted and faint text are the foreground at 70% and 55%, as
/// the template mixes them, since Omarchy has no mid-grey text role. Code
/// blocks and callouts sit on a surface a few percent toward the foreground.
/// The result is passed through [`Theme::enforce_contrast`].
#[must_use]
pub fn theme(c: &Colors) -> Theme {
    let (bg, fg) = (c.background, c.foreground);
    let muted_text = hex(blend(fg, bg, 0.30));
    let faint_text = hex(blend(fg, bg, 0.45));
    let hover = hex(blend(bg, c.selection, 0.5));
    let selection = hex(c.selection);
    let (black, white) = (Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255));

    let mut t = Theme::from_seed(&seed::TERMINAL);
    t.name = "terminal".into();
    t.dark = c.dark;

    // Surfaces. The page stays `reset`; see the module docs.
    t.bg_hover = hover.clone();
    t.bg_active = selection.clone();
    t.bg_selection = selection.clone();
    t.border = hex(c.muted);
    t.border_focus = hex(c.accent);
    t.code_bg = hex(blend(bg, fg, CODE_PANEL));
    t.callout_bg = hex(blend(bg, fg, CALLOUT_PANEL));

    // Chrome.
    t.ribbon_icon = faint_text.clone();
    t.ribbon_icon_active = hex(c.accent);
    t.tab_inactive_fg = faint_text.clone();
    t.tab_modified = hex(c.accent);
    t.titlebar_fg = muted_text.clone();
    t.statusbar_fg = muted_text.clone();
    t.scrollbar_thumb = hex(c.muted);
    t.line_number = faint_text.clone();
    t.line_number_active = muted_text.clone();

    // Text. `text_normal` stays `reset`, the terminal's own foreground.
    t.text_muted = muted_text.clone();
    t.text_faint = faint_text.clone();
    t.text_accent = hex(c.accent);
    t.text_on_accent = hex(best_on(c.accent, &[black, white, bg, fg]));
    t.text_error = hex(c.red);
    t.text_warning = hex(c.orange);
    t.text_success = hex(c.green);
    t.text_info = hex(c.blue);
    t.text_highlight_bg = hex(c.yellow);
    t.text_highlight_fg = hex(best_on(c.yellow, &[black, white, bg]));
    t.accent = hex(c.accent);
    t.accent_hover = hex(c.magenta);

    // Markdown.
    t.h1 = hex(c.red);
    t.h2 = hex(c.green);
    t.h3 = hex(c.yellow);
    t.h4 = hex(c.blue);
    t.h5 = hex(c.magenta);
    t.h6 = hex(c.magenta);
    t.strikethrough = faint_text.clone();
    t.link = hex(c.blue);
    t.link_unresolved = muted_text.clone();
    t.link_external = hex(c.bright_blue);
    t.tag_fg = hex(c.cyan);
    t.tag_bg = selection;
    t.code_fg = hex(c.cyan);
    t.quote_bar = hex(c.accent);
    t.quote_fg = muted_text.clone();
    t.hr = hex(c.muted);
    t.list_marker = hex(c.accent);
    t.checkbox_done = hex(c.green);
    t.checkbox_todo = muted_text.clone();
    t.table_border = hex(c.muted);
    t.frontmatter_key = hex(c.magenta);
    t.frontmatter_value = muted_text.clone();

    // Code.
    t.syn_keyword = hex(c.red);
    t.syn_string = hex(c.green);
    t.syn_comment = hex(c.muted);
    t.syn_number = hex(c.yellow);
    t.syn_function = hex(c.blue);
    t.syn_type = hex(c.magenta);
    t.syn_punct = muted_text.clone();

    // Callouts.
    t.callout_note = hex(c.blue);
    t.callout_tip = hex(c.cyan);
    t.callout_warning = hex(c.orange);
    t.callout_danger = hex(c.red);
    t.callout_success = hex(c.green);
    t.callout_question = hex(c.yellow);
    t.callout_quote = muted_text;

    // Graph.
    t.graph_node_focused = hex(c.accent);
    t.graph_edge_active = hex(c.accent);

    t.enforce_contrast(bg, fg);
    t
}

/// The directory a `colors.toml` is looked for in: `override_dir` if it is set
/// and not empty, otherwise Omarchy's own place under `home`.
#[must_use]
pub fn theme_dir_from(override_dir: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    match override_dir {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home
            .filter(|home| !home.is_empty())
            .map(|home| PathBuf::from(home).join(".local/state/omarchy/current/theme")),
    }
}

/// [`theme_dir_from`] with the process environment.
#[must_use]
pub fn theme_dir() -> Option<PathBuf> {
    theme_dir_from(
        std::env::var(THEME_DIR_ENV).ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// What a [`Source`] found on a poll.
#[derive(Debug)]
pub enum Poll {
    /// The file is as it was when it last loaded.
    Unchanged,
    /// The file is new or changed, and parsed.
    Loaded(Colors),
    /// There is no file. Not an error: Omarchy may simply not be installed.
    Missing,
    /// There is a file that could not be used, or read. The caller keeps the
    /// colors it has; the next poll tries again.
    Failed(String),
}

/// What identifies one version of the file. The inode (behind any symlink)
/// catches a theme switch that swaps the directory `current/theme` points at,
/// the mtime and length one that rewrites the file in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            identity: (meta.dev(), meta.ino()),
        }
    }
}

/// A theme file that is checked for changes. One `stat` per poll.
#[derive(Debug)]
pub struct Source {
    path: PathBuf,
    stamp: Option<Stamp>,
}

/// A `colors.toml` is under a kilobyte; a file far bigger than this is not one.
const MAX_BYTES: u64 = 64 * 1024;

impl Source {
    /// Watches `dir/colors.toml`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            path: dir.join("colors.toml"),
            stamp: None,
        }
    }

    /// A source for [`theme_dir`], if the environment says where it is.
    #[must_use]
    pub fn locate() -> Option<Self> {
        theme_dir().map(Self::new)
    }

    /// Looks at the file. Cheap when nothing changed; reads and parses it
    /// when something did.
    pub fn poll(&mut self) -> Poll {
        let meta = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(err) => {
                // Forget what was loaded: a file that comes back, even with
                // the same bytes, is a theme switch that finished.
                self.stamp = None;
                return if err.kind() == std::io::ErrorKind::NotFound {
                    Poll::Missing
                } else {
                    Poll::Failed(err.to_string())
                };
            }
        };
        if !meta.is_file() {
            self.stamp = None;
            return Poll::Missing;
        }
        let stamp = Stamp::of(&meta);
        if self.stamp == Some(stamp) {
            return Poll::Unchanged;
        }
        let mut text = String::new();
        let read = fs::File::open(&self.path)
            .and_then(|file| file.take(MAX_BYTES).read_to_string(&mut text));
        let parsed = read
            .map_err(|err| err.to_string())
            .and_then(|_| Colors::parse(&text));
        match parsed {
            Ok(colors) => {
                self.stamp = Some(stamp);
                Poll::Loaded(colors)
            }
            // Not remembered, so the next poll reads again: a half-written
            // file is complete a moment later, and its stamp may not change.
            Err(err) => Poll::Failed(err),
        }
    }
}
