//! Color string parsing.
//!
//! Theme slots are stored as strings so a user can drop a TOML theme into their
//! config without emeraldian needing to know about it at compile time. A slot
//! is one of:
//!
//! - a hex color, `#rrggbb` or the short `#rgb` form
//! - one of the terminal's own ANSI color names (`red`, `brightblue`, `gray`, …)
//! - `reset`, meaning "inherit whatever the terminal itself uses"
//!
//! `reset` is what makes the `terminal` theme work: rather than imposing a
//! palette, it defers to the colors the user already configured in their
//! terminal emulator, so emeraldian never clashes with the rest of their setup.

use ratatui::style::Color;

/// Parses a theme color string. Anything unrecognized resolves to
/// [`Color::Reset`] rather than panicking, so a typo in a user's theme file
/// degrades to "terminal default" instead of taking the app down.
pub fn parse(s: &str) -> Color {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex).unwrap_or(Color::Reset);
    }

    match s.to_ascii_lowercase().as_str() {
        "reset" | "none" | "" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" | "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" | "brightred" => Color::LightRed,
        "lightgreen" | "brightgreen" => Color::LightGreen,
        "lightyellow" | "brightyellow" => Color::LightYellow,
        "lightblue" | "brightblue" => Color::LightBlue,
        "lightmagenta" | "brightmagenta" => Color::LightMagenta,
        "lightcyan" | "brightcyan" => Color::LightCyan,
        other => other.parse::<u8>().map_or(Color::Reset, Color::Indexed),
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    let bytes = hex.as_bytes();
    match bytes.len() {
        // #rgb — each nibble is doubled, so `#f0a` means `#ff00aa`.
        3 => {
            let r = nibble(bytes[0])?;
            let g = nibble(bytes[1])?;
            let b = nibble(bytes[2])?;
            Some(Color::Rgb(r * 17, g * 17, b * 17))
        }
        6 => {
            let r = byte(bytes[0], bytes[1])?;
            let g = byte(bytes[2], bytes[3])?;
            let b = byte(bytes[4], bytes[5])?;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn byte(hi: u8, lo: u8) -> Option<u8> {
    Some(nibble(hi)? * 16 + nibble(lo)?)
}

/// Blends `a` toward `b` by `t` (0.0 → `a`, 1.0 → `b`).
///
/// Only meaningful for two true-color values; if either side is an ANSI or
/// `reset` color there is nothing to interpolate between, so `a` is returned
/// unchanged. Used to derive hover/selection tints from a theme's base colors.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return a;
    };
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// Perceived luminance in 0.0..=1.0, or `None` for non-RGB colors.
///
/// Used to decide whether text drawn on top of a color should be light or dark.
pub fn luminance(color: Color) -> Option<f32> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    Some((0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0)
}

/// `#rrggbb` for a true-color value, `None` for ANSI and `reset`.
#[must_use]
pub fn to_hex(color: Color) -> Option<String> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// WCAG relative luminance (sRGB, gamma-expanded), or `None` for non-RGB.
fn relative_luminance(color: Color) -> Option<f64> {
    let Color::Rgb(r, g, b) = color else {
        return None;
    };
    let channel = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
}

/// WCAG contrast ratio between two colors (1.0 to 21.0, symmetric), or `None`
/// when either is not a true-color value. ANSI colors and `reset` are chosen by
/// the terminal, so there is nothing to measure.
#[must_use]
pub fn contrast(a: Color, b: Color) -> Option<f64> {
    let (la, lb) = (relative_luminance(a)?, relative_luminance(b)?);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// Blend steps tried by [`pull_toward`], 1% each. Contrast is not monotonic in
/// the blend, so the smallest passing step is found by scanning, not bisecting.
const STEPS: u8 = 100;

/// Moves `color` toward `toward` in the smallest step that gives at least `min`
/// contrast against `against`; the full distance if nothing in between does.
/// Colors that are not true-color are returned unchanged.
#[must_use]
pub fn pull_toward(color: Color, toward: Color, against: Color, min: f64) -> Color {
    let passes = |c: Color| contrast(c, against).is_some_and(|ratio| ratio >= min);
    if contrast(color, against).is_none() || passes(color) {
        return color;
    }
    (1..=STEPS)
        .map(|step| blend(color, toward, f32::from(step) / f32::from(STEPS)))
        .find(|&c| passes(c))
        .unwrap_or(toward)
}

/// Nudges a text color toward black or white until it reaches `min` contrast on
/// `bg`. A color already there is returned as it is, and so is one that is not
/// true-color.
///
/// The direction is the one the color already leans (lighter than the
/// background goes lighter), so the hue is kept as long as that can work; only
/// when that side cannot reach `min` does it go the other way.
#[must_use]
pub fn raise_contrast(fg: Color, bg: Color, min: f64) -> Color {
    let (Some(lf), Some(lb)) = (relative_luminance(fg), relative_luminance(bg)) else {
        return fg;
    };
    if contrast(fg, bg).is_some_and(|ratio| ratio >= min) {
        return fg;
    }
    let white = Color::Rgb(255, 255, 255);
    let black = Color::Rgb(0, 0, 0);
    let (first, second) = if lf >= lb {
        (white, black)
    } else {
        (black, white)
    };
    let reaches = |end: Color| contrast(end, bg).is_some_and(|ratio| ratio >= min);
    let toward = if reaches(first) || !reaches(second) {
        first
    } else {
        second
    };
    pull_toward(fg, toward, bg, min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_follows_the_wcag_formula() {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(255, 255, 255);
        let ratio = contrast(black, white).expect("rgb pair");
        assert!(
            (ratio - 21.0).abs() < 0.01,
            "black on white is 21:1, got {ratio}"
        );
        assert!((contrast(white, black).expect("rgb pair") - ratio).abs() < 1e-9);
        // The figure from the reading-view research: catppuccin-latte's tag
        // chip, magenta on muted.
        let chip =
            contrast(Color::Rgb(0xea, 0x76, 0xcb), Color::Rgb(0xac, 0xb0, 0xbe)).expect("rgb pair");
        assert!((chip - 1.22).abs() < 0.01, "got {chip}");
        assert_eq!(contrast(Color::Reset, white), None);
        assert_eq!(contrast(Color::Red, white), None);
    }

    #[test]
    fn hex_round_trips_rgb_only() {
        assert_eq!(
            to_hex(Color::Rgb(0x1e, 0x66, 0xf5)).as_deref(),
            Some("#1e66f5")
        );
        assert_eq!(to_hex(Color::Reset), None);
        assert_eq!(
            parse(&to_hex(Color::Rgb(1, 2, 3)).expect("rgb")),
            Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn raise_contrast_leaves_a_passing_colour_alone() {
        let fg = Color::Rgb(0xa9, 0xb1, 0xd6);
        let bg = Color::Rgb(0, 0, 0);
        assert_eq!(raise_contrast(fg, bg, 4.5), fg);
        // Not measurable, so not touched.
        assert_eq!(raise_contrast(Color::Blue, bg, 4.5), Color::Blue);
    }

    #[test]
    fn raise_contrast_moves_a_colour_the_way_it_already_leans() {
        // Magenta is darker than the latte page, so it has to get darker.
        let bg = Color::Rgb(0xef, 0xf1, 0xf5);
        let fixed = raise_contrast(Color::Rgb(0xea, 0x76, 0xcb), bg, 4.5);
        assert!(contrast(fixed, bg).expect("rgb") >= 4.5);
        let Color::Rgb(r, _, _) = fixed else {
            panic!("rgb expected")
        };
        assert!(r < 0xea, "went darker: {fixed:?}");
        // On a black page a dim blue has to get lighter.
        let dark = Color::Rgb(0, 0, 0);
        let fixed = raise_contrast(Color::Rgb(0x20, 0x20, 0x60), dark, 4.5);
        assert!(contrast(fixed, dark).expect("rgb") >= 4.5);
    }

    #[test]
    fn raise_contrast_is_gentle() {
        // It takes the smallest step that is enough, so the hue survives.
        let bg = Color::Rgb(0xef, 0xf1, 0xf5);
        let blue = Color::Rgb(0x1e, 0x66, 0xf5);
        let fixed = raise_contrast(blue, bg, 4.5);
        assert!(contrast(fixed, bg).expect("rgb") >= 4.5);
        let Color::Rgb(r, g, b) = fixed else {
            panic!("rgb expected")
        };
        assert!(b > r && b > g, "still blue: {fixed:?}");
    }

    #[test]
    fn pull_toward_moves_a_background_until_the_text_reads() {
        let page = Color::Rgb(0xef, 0xf1, 0xf5);
        let text = Color::Rgb(0x4c, 0x4f, 0x69);
        let selection = Color::Rgb(0x30, 0x30, 0x50);
        let pulled = pull_toward(selection, page, text, 4.5);
        assert!(contrast(text, pulled).expect("rgb") >= 4.5);
        assert_eq!(pull_toward(page, page, text, 4.5), page);
    }

    #[test]
    fn parses_hex_forms() {
        assert_eq!(parse("#1e1e1e"), Color::Rgb(0x1e, 0x1e, 0x1e));
        assert_eq!(parse("#f0a"), Color::Rgb(0xff, 0x00, 0xaa));
        assert_eq!(parse("  #8b6cef  "), Color::Rgb(0x8b, 0x6c, 0xef));
    }

    #[test]
    fn parses_names_case_insensitively() {
        assert_eq!(parse("Red"), Color::Red);
        assert_eq!(parse("darkgrey"), Color::DarkGray);
        assert_eq!(parse("brightblue"), Color::LightBlue);
        assert_eq!(parse("42"), Color::Indexed(42));
    }

    #[test]
    fn unknown_and_empty_fall_back_to_reset() {
        assert_eq!(parse(""), Color::Reset);
        assert_eq!(parse("reset"), Color::Reset);
        assert_eq!(parse("chartreuse"), Color::Reset);
        assert_eq!(parse("#12345"), Color::Reset);
    }

    #[test]
    fn blend_interpolates_rgb_only() {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(255, 255, 255);
        assert_eq!(blend(black, white, 0.5), Color::Rgb(128, 128, 128));
        assert_eq!(blend(black, white, 0.0), black);
        // Nothing to interpolate against a terminal-default color.
        assert_eq!(blend(Color::Reset, white, 0.5), Color::Reset);
    }
}
