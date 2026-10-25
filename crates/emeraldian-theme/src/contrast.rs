//! The contrast guard: which text sits on which background in the reading
//! view, how much contrast each pair needs, and the pass that repairs a theme
//! until they all have it.
//!
//! [`PAIRS`] is the single list. [`Theme::enforce_contrast`] reads it to repair
//! a theme as it is derived; [`Palette::contrast_report`] reads it to measure
//! one, which is what the tests do for every Omarchy theme.
//!
//! Only true-color values can be measured. A slot left at `reset` stands for the
//! terminal's own page or text color, so measuring and repairing take the page
//! colors as arguments; an ANSI name such as `blue` is the terminal's to choose
//! and is skipped.

use ratatui::style::Color;

use crate::color::{contrast, pull_toward, raise_contrast, to_hex};
use crate::theme::{Palette, Theme};

/// Which side of a pair is moved when it falls short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// The text color is nudged toward black or white.
    Text,
    /// The background is pulled back toward the page, which is the one
    /// background text is always drawn on. Used where moving the text would
    /// change a color shared with every other place it appears.
    Surface,
}

/// One text-on-background pair of the reading view.
#[derive(Debug, Clone, Copy)]
pub struct Pair {
    pub fg: &'static str,
    pub bg: &'static str,
    /// Least WCAG contrast ratio the pair must reach.
    pub min: f64,
    pub fix: Fix,
}

const BODY: f64 = 4.5;
const TEXT: f64 = 3.0;
/// How far past the minimum a repaired text color is taken.
const HEADROOM: f64 = 1.25;

const fn pair(fg: &'static str, bg: &'static str, min: f64, fix: Fix) -> Pair {
    Pair { fg, bg, min, fix }
}

/// Everything the reading view draws as text. Body text needs 4.5:1; links and
/// the code and chip text that is read as prose need it too; every other pair
/// (headings, dim text, callout titles, markers) needs 3:1.
pub const PAIRS: &[Pair] = &[
    // ---- the page --------------------------------------------------------
    pair("text_normal", "bg_primary", BODY, Fix::Text),
    pair("text_muted", "bg_primary", TEXT, Fix::Text),
    pair("text_faint", "bg_primary", TEXT, Fix::Text),
    pair("quote_fg", "bg_primary", TEXT, Fix::Text),
    pair("link", "bg_primary", BODY, Fix::Text),
    pair("link_external", "bg_primary", BODY, Fix::Text),
    pair("link_unresolved", "bg_primary", TEXT, Fix::Text),
    pair("text_accent", "bg_primary", TEXT, Fix::Text),
    pair("list_marker", "bg_primary", TEXT, Fix::Text),
    pair("quote_bar", "bg_primary", TEXT, Fix::Text),
    pair("checkbox_done", "bg_primary", TEXT, Fix::Text),
    pair("checkbox_todo", "bg_primary", TEXT, Fix::Text),
    pair("frontmatter_key", "bg_primary", TEXT, Fix::Text),
    pair("frontmatter_value", "bg_primary", TEXT, Fix::Text),
    pair("text_info", "bg_primary", TEXT, Fix::Text),
    pair("text_warning", "bg_primary", TEXT, Fix::Text),
    pair("text_error", "bg_primary", TEXT, Fix::Text),
    pair("text_success", "bg_primary", TEXT, Fix::Text),
    pair("h1", "bg_primary", TEXT, Fix::Text),
    pair("h2", "bg_primary", TEXT, Fix::Text),
    pair("h3", "bg_primary", TEXT, Fix::Text),
    pair("h4", "bg_primary", TEXT, Fix::Text),
    pair("h5", "bg_primary", TEXT, Fix::Text),
    pair("h6", "bg_primary", TEXT, Fix::Text),
    // Task glyphs and callout titles are drawn in the callout colors, on the
    // page and inside the callout panel.
    pair("callout_note", "bg_primary", TEXT, Fix::Text),
    pair("callout_tip", "bg_primary", TEXT, Fix::Text),
    pair("callout_warning", "bg_primary", TEXT, Fix::Text),
    pair("callout_danger", "bg_primary", TEXT, Fix::Text),
    pair("callout_success", "bg_primary", TEXT, Fix::Text),
    pair("callout_question", "bg_primary", TEXT, Fix::Text),
    pair("callout_quote", "bg_primary", TEXT, Fix::Text),
    // ---- the callout panel -----------------------------------------------
    pair("callout_note", "callout_bg", TEXT, Fix::Text),
    pair("callout_tip", "callout_bg", TEXT, Fix::Text),
    pair("callout_warning", "callout_bg", TEXT, Fix::Text),
    pair("callout_danger", "callout_bg", TEXT, Fix::Text),
    pair("callout_success", "callout_bg", TEXT, Fix::Text),
    pair("callout_question", "callout_bg", TEXT, Fix::Text),
    pair("callout_quote", "callout_bg", TEXT, Fix::Text),
    pair("text_normal", "callout_bg", BODY, Fix::Surface),
    pair("text_muted", "callout_bg", TEXT, Fix::Surface),
    pair("link", "callout_bg", BODY, Fix::Surface),
    // ---- code: the block panel and the inline code background ------------
    pair("text_normal", "code_bg", BODY, Fix::Surface),
    pair("text_faint", "code_bg", TEXT, Fix::Surface),
    pair("code_fg", "code_bg", BODY, Fix::Text),
    pair("syn_keyword", "code_bg", TEXT, Fix::Text),
    pair("syn_string", "code_bg", TEXT, Fix::Text),
    pair("syn_number", "code_bg", TEXT, Fix::Text),
    pair("syn_function", "code_bg", TEXT, Fix::Text),
    pair("syn_type", "code_bg", TEXT, Fix::Text),
    pair("syn_punct", "code_bg", TEXT, Fix::Text),
    pair("syn_comment", "code_bg", TEXT, Fix::Text),
    // ---- tag chip and key cap ---------------------------------------------
    pair("tag_fg", "tag_bg", BODY, Fix::Text),
    pair("text_muted", "tag_bg", TEXT, Fix::Surface),
    // ---- selection and the picked link ------------------------------------
    pair("text_normal", "bg_selection", TEXT, Fix::Surface),
    pair("text_normal", "bg_active", TEXT, Fix::Surface),
    pair("text_normal", "bg_hover", TEXT, Fix::Surface),
    pair("text_on_accent", "accent", TEXT, Fix::Text),
    // ---- ==highlight== ----------------------------------------------------
    pair("text_highlight_fg", "text_highlight_bg", BODY, Fix::Text),
];

/// One measured pair.
#[derive(Debug, Clone, Copy)]
pub struct Check {
    pub pair: &'static Pair,
    /// The ratio, or `None` when a side is not a measurable color.
    pub ratio: Option<f64>,
}

impl Check {
    /// A measurable pair that falls short. Unmeasurable pairs are not failures.
    #[must_use]
    pub fn fails(&self) -> bool {
        self.ratio.is_some_and(|ratio| ratio + 1e-9 < self.pair.min)
    }
}

fn resolve(color: Color, page: Color) -> Color {
    if color == Color::Reset { page } else { color }
}

impl Palette {
    /// Measures every pair in [`PAIRS`]. `page_bg` and `page_fg` stand in for
    /// slots that are `reset`.
    #[must_use]
    pub fn contrast_report(&self, page_bg: Color, page_fg: Color) -> Vec<Check> {
        PAIRS
            .iter()
            .map(|pair| {
                let fg = self.slot(pair.fg).map(|c| resolve(c, page_fg));
                let bg = self.slot(pair.bg).map(|c| resolve(c, page_bg));
                let ratio = fg.zip(bg).and_then(|(fg, bg)| contrast(fg, bg));
                Check { pair, ratio }
            })
            .collect()
    }
}

impl Theme {
    /// Repairs every pair in [`PAIRS`] that falls short, changing as little as
    /// possible. Slots that already pass, and slots that cannot be measured,
    /// are left alone. `page_bg` and `page_fg` stand in for `reset`.
    pub fn enforce_contrast(&mut self, page_bg: Color, page_fg: Color) {
        // Fixing one pair can move a color that another pair uses, so go round
        // until nothing changes. Every step moves a color toward black, white
        // or the page, which only ever helps, so this settles quickly.
        for _ in 0..4 {
            let mut changed = false;
            for pair in PAIRS {
                let palette = Palette::from(&*self);
                let (Some(fg), Some(bg)) = (palette.slot(pair.fg), palette.slot(pair.bg)) else {
                    continue;
                };
                let (fg, bg) = (resolve(fg, page_fg), resolve(bg, page_bg));
                if contrast(fg, bg).is_none_or(|ratio| ratio >= pair.min) {
                    continue;
                }
                let (slot, fixed) = match pair.fix {
                    // Aim past the minimum: the same text is also drawn on the
                    // panels, which sit a little off the page, and landing
                    // exactly on the limit here would fail there by a hair.
                    Fix::Text => (pair.fg, raise_contrast(fg, bg, pair.min * HEADROOM)),
                    Fix::Surface => (pair.bg, pull_toward(bg, page_bg, fg, pair.min)),
                };
                if let (Some(hex), Some(text)) = (to_hex(fixed), self.slot_mut(slot)) {
                    changed |= *text != hex;
                    *text = hex;
                }
            }
            if !changed {
                break;
            }
        }
    }
}

/// Of `candidates`, the color with the most contrast against `bg`.
#[must_use]
pub fn best_on(bg: Color, candidates: &[Color]) -> Color {
    candidates
        .iter()
        .copied()
        .max_by(|a, b| {
            let (a, b) = (
                contrast(*a, bg).unwrap_or(0.0),
                contrast(*b, bg).unwrap_or(0.0),
            );
            a.total_cmp(&b)
        })
        .unwrap_or(bg)
}
