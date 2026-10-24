//! Diagrams as pictures: Excalidraw scenes and Mermaid sources.
//!
//! Both end up as SVG and are rasterised by `resvg` at the exact pixel size of
//! the cells they are drawn into, so a terminal with a graphics protocol (kitty,
//! WezTerm, iTerm2, sixel) shows crisp vector output at any pane width and font
//! size instead of a screenshot scaled to fit. Mermaid is laid out and drawn by
//! `mermaid-rs-renderer`, pure Rust with no browser behind it; Excalidraw scenes
//! are written out to SVG here, element by element, in z-order.
//!
//! Nothing in this module touches the terminal. The expensive parts — layout,
//! font loading, rasterising — are called from the worker thread in
//! [`crate::images`], never from the draw loop.

use std::cell::Cell;
use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use emeraldian_core::excalidraw::{Drawing, Element, Head, Rgb, Shape, TextAlign};
use emeraldian_theme::Palette;
use mermaid_rs_renderer::{RenderOptions, Theme};
use ratatui::style::Color;
use resvg::{tiny_skia, usvg};

/// What a diagram is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Excalidraw,
    Mermaid,
}

/// Which diagram language a note's file name says it holds, if any.
///
/// `Flow.excalidraw` and `Flow.excalidraw.md` are both drawings; `.mmd` and
/// `.mermaid` files are Mermaid source.
#[must_use]
pub fn kind_of(rel: &str) -> Option<Kind> {
    if emeraldian_core::excalidraw::is_drawing(rel) {
        return Some(Kind::Excalidraw);
    }
    matches!(
        emeraldian_core::note::own_extension(rel),
        Some(".mmd" | ".mermaid")
    )
    .then_some(Kind::Mermaid)
}

/// The colours a diagram is drawn against, so it sits in the pane rather than
/// on a white card in the middle of a dark theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Look {
    pub dark: bool,
    pub background: [u8; 3],
}

impl Look {
    #[must_use]
    pub fn from_palette(palette: &Palette) -> Self {
        let background = match palette.bg_primary {
            Color::Rgb(r, g, b) => [r, g, b],
            // An ANSI colour names whatever the terminal makes of it; the
            // theme's own brightness is the best guess left.
            _ if palette.dark => [0x1e, 0x1e, 0x1e],
            _ => [0xff, 0xff, 0xff],
        };
        Self {
            dark: palette.dark,
            background,
        }
    }

    fn hex(self) -> String {
        let [r, g, b] = self.background;
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// A drawn diagram and its natural size in SVG pixels.
#[derive(Debug, Clone)]
pub struct Svg {
    pub text: String,
    pub width: f32,
    pub height: f32,
}

// ---------------------------------------------------------------------------
// Panics inside the renderers
// ---------------------------------------------------------------------------

thread_local! {
    static GUARDED: Cell<bool> = const { Cell::new(false) };
}

/// Whether the current thread is inside [`guarded`], where a panic is reported
/// as a failed diagram and must not tear down the terminal on its way.
#[must_use]
pub fn in_guarded_render() -> bool {
    GUARDED.with(Cell::get)
}

/// Runs a renderer, turning a panic into an error.
///
/// Mermaid has twenty-odd diagram types and a hand-written parser; a note with
/// a half-typed one in it is ordinary, and must not end the session.
fn guarded<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    GUARDED.with(|flag| flag.set(true));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
    GUARDED.with(|flag| flag.set(false));
    outcome.map_err(|_| "the renderer gave up on this diagram".to_string())
}

// ---------------------------------------------------------------------------
// Mermaid
// ---------------------------------------------------------------------------

/// Lays out and draws Mermaid source.
///
/// # Errors
///
/// The renderer's own message when the source does not parse.
pub fn mermaid_svg(source: &str, look: &Look) -> Result<Svg, String> {
    let mut theme = if look.dark {
        Theme::dark()
    } else {
        Theme::modern()
    };
    theme.background = look.hex();
    let options = RenderOptions {
        theme,
        ..RenderOptions::default()
    };
    let text = guarded(|| mermaid_rs_renderer::render_with_options(source, options))?
        .map_err(|err| first_line(&err.to_string()))?;
    let (width, height) = svg_size(&text).ok_or("the diagram came out with no size")?;
    Ok(Svg {
        text,
        width,
        height,
    })
}

fn first_line(message: &str) -> String {
    message.lines().next().unwrap_or(message).trim().to_string()
}

/// Reads `width` and `height` off the root element, falling back to its
/// `viewBox`.
fn svg_size(svg: &str) -> Option<(f32, f32)> {
    let start = svg.find("<svg")?;
    let tag = &svg[start..start + svg[start..].find('>')?];
    let number = |value: &str| -> Option<f32> {
        value
            .trim()
            .trim_end_matches("px")
            .parse::<f32>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    if let (Some(width), Some(height)) = (attribute(tag, "width"), attribute(tag, "height"))
        && let (Some(width), Some(height)) = (number(width), number(height))
    {
        return Some((width, height));
    }
    let view_box = attribute(tag, "viewBox")?;
    let parts: Vec<f32> = view_box
        .split([' ', ','])
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    match parts.as_slice() {
        [_, _, width, height] if *width > 0.0 && *height > 0.0 => Some((*width, *height)),
        _ => None,
    }
}

/// The value of `name="…"` inside a tag, matched on a whole attribute name.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    Some(&tag[start..start + end])
}

// ---------------------------------------------------------------------------
// Excalidraw
// ---------------------------------------------------------------------------

/// Excalidraw's default ink, used where a text element names no colour.
const INK: Rgb = Rgb {
    r: 0x1e,
    g: 0x1e,
    b: 0x1e,
};

/// Space left round the drawing, in scene units.
const MARGIN: f64 = 28.0;

const SANS: &str = "'Liberation Sans', 'DejaVu Sans', 'Noto Sans', Helvetica, Arial, sans-serif";
const MONO: &str = "'DejaVu Sans Mono', 'Liberation Mono', 'Noto Sans Mono', monospace";

/// Draws a scene as SVG, elements in their z-order.
///
/// `None` for an empty scene.
#[must_use]
pub fn excalidraw_svg(drawing: &Drawing, look: &Look) -> Option<Svg> {
    let (left, top, width, height) = drawing.bounds()?;
    let (x, y) = (left - MARGIN, top - MARGIN);
    let (w, h) = (width + 2.0 * MARGIN, height + 2.0 * MARGIN);

    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.1}\" height=\"{h:.1}\" \
         viewBox=\"{x:.1} {y:.1} {w:.1} {h:.1}\">\
         <rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" fill=\"{}\"/>",
        look.hex()
    );
    for element in &drawing.elements {
        write_element(&mut svg, element, look.dark);
    }
    svg.push_str("</svg>");

    Some(Svg {
        text: svg,
        width: w as f32,
        height: h as f32,
    })
}

fn write_element(out: &mut String, e: &Element, dark: bool) {
    let (cx, cy) = (e.x + e.width / 2.0, e.y + e.height / 2.0);
    let _ = write!(out, "<g");
    if e.angle.abs() > 1e-6 {
        let _ = write!(
            out,
            " transform=\"rotate({:.3} {cx:.2} {cy:.2})\"",
            e.angle.to_degrees()
        );
    }
    if e.opacity < 1.0 {
        let _ = write!(out, " opacity=\"{:.2}\"", e.opacity);
    }
    out.push('>');

    let stroke = e
        .stroke
        .map_or_else(|| "none".to_string(), |c| paint(c, dark));
    let fill = e
        .fill
        .map_or_else(|| "none".to_string(), |c| paint(c, dark));
    let fill_opacity = if e.hatched { 0.45 } else { 1.0 };
    let width = e.stroke_width.max(0.5);
    let dash = if e.dotted {
        format!(" stroke-dasharray=\"0.1 {:.1}\"", width * 3.0)
    } else if e.dashed {
        format!(
            " stroke-dasharray=\"{:.1} {:.1}\"",
            width * 4.0,
            width * 4.0
        )
    } else {
        String::new()
    };
    let style_for = |fill: &str| {
        format!(
            "fill=\"{fill}\" fill-opacity=\"{fill_opacity}\" stroke=\"{stroke}\" \
             stroke-width=\"{width:.2}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"{dash}"
        )
    };
    let style = style_for(&fill);

    let (bx, bw) = ordered(e.x, e.width);
    let (by, bh) = ordered(e.y, e.height);
    match e.shape {
        Shape::Rectangle | Shape::Frame => {
            let radius = if e.rounded && e.shape == Shape::Rectangle {
                (bw.min(bh) * 0.25).min(32.0)
            } else {
                0.0
            };
            let style = if e.shape == Shape::Frame && e.stroke.is_none() {
                let ghost = paint(
                    Rgb {
                        r: 160,
                        g: 160,
                        b: 160,
                    },
                    dark,
                );
                format!(
                    "fill=\"none\" stroke=\"{ghost}\" stroke-width=\"1\" stroke-dasharray=\"6 4\""
                )
            } else {
                style
            };
            let _ = write!(
                out,
                "<rect x=\"{bx:.2}\" y=\"{by:.2}\" width=\"{bw:.2}\" height=\"{bh:.2}\" \
                 rx=\"{radius:.2}\" {style}/>"
            );
        }
        Shape::Ellipse => {
            let _ = write!(
                out,
                "<ellipse cx=\"{:.2}\" cy=\"{:.2}\" rx=\"{:.2}\" ry=\"{:.2}\" {style}/>",
                bx + bw / 2.0,
                by + bh / 2.0,
                bw / 2.0,
                bh / 2.0
            );
        }
        Shape::Diamond => {
            let _ = write!(
                out,
                "<polygon points=\"{:.2},{:.2} {:.2},{:.2} {:.2},{:.2} {:.2},{:.2}\" {style}/>",
                bx + bw / 2.0,
                by,
                bx + bw,
                by + bh / 2.0,
                bx + bw / 2.0,
                by + bh,
                bx,
                by + bh / 2.0
            );
        }
        Shape::Line | Shape::Arrow | Shape::Freehand => {
            write_path(out, e, &style_for, &fill, &stroke, width);
        }
        Shape::Text => write_text(out, e, dark),
    }
    out.push_str("</g>");
}

/// A box given as origin and a possibly negative extent, as origin and extent
/// with the extent positive.
fn ordered(start: f64, extent: f64) -> (f64, f64) {
    if extent < 0.0 {
        (start + extent, -extent)
    } else {
        (start, extent)
    }
}

fn write_path(
    out: &mut String,
    e: &Element,
    style_for: &dyn Fn(&str) -> String,
    fill: &str,
    stroke: &str,
    width: f64,
) {
    let points: Vec<(f64, f64)> = e
        .points
        .iter()
        .map(|&(dx, dy)| (e.x + dx, e.y + dy))
        .collect();
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return;
    };
    if points.len() == 1 {
        let _ = write!(
            out,
            "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" fill=\"{stroke}\"/>",
            first.0,
            first.1,
            width / 2.0
        );
        return;
    }

    let closed = points.len() > 2 && distance(first, last) < 1.0;
    // Only a closed outline can hold a fill; an open line drawn with one would
    // be shaded between its ends.
    let style = style_for(if closed { fill } else { "none" });
    let curved = e.rounded && points.len() > 2 || e.shape == Shape::Freehand && points.len() > 2;
    let mut d = String::new();
    let _ = write!(d, "M{:.2} {:.2}", first.0, first.1);
    if curved {
        for i in 0..points.len() - 1 {
            let p0 = points[i.saturating_sub(1)];
            let (p1, p2) = (points[i], points[i + 1]);
            let p3 = points[(i + 2).min(points.len() - 1)];
            let c1 = (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0);
            let c2 = (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0);
            let _ = write!(
                d,
                " C{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                c1.0, c1.1, c2.0, c2.1, p2.0, p2.1
            );
        }
    } else {
        for point in &points[1..] {
            let _ = write!(d, " L{:.2} {:.2}", point.0, point.1);
        }
    }
    if closed {
        d.push('Z');
    }
    let _ = write!(out, "<path d=\"{d}\" {style}/>");

    let size = (14.0 + 2.0 * width).min(path_length(&points) * 0.4);
    if let (Some(head), true) = (e.end_head, points.len() >= 2) {
        write_head(
            out,
            last,
            from_point(&points, true),
            head,
            size,
            stroke,
            width,
        );
    }
    if let (Some(head), true) = (e.start_head, points.len() >= 2) {
        write_head(
            out,
            first,
            from_point(&points, false),
            head,
            size,
            stroke,
            width,
        );
    }
}

/// The point an arrow leaves its tip from: the nearest one that isn't on top of
/// the tip itself.
fn from_point(points: &[(f64, f64)], at_end: bool) -> (f64, f64) {
    let ordered: Vec<_> = if at_end {
        points.iter().rev().copied().collect()
    } else {
        points.to_vec()
    };
    let tip = ordered[0];
    ordered
        .iter()
        .skip(1)
        .find(|&&p| distance(p, tip) > 0.5)
        .copied()
        .unwrap_or(tip)
}

fn write_head(
    out: &mut String,
    tip: (f64, f64),
    from: (f64, f64),
    head: Head,
    size: f64,
    stroke: &str,
    width: f64,
) {
    let length = distance(tip, from);
    if length < 1e-6 {
        return;
    }
    // Unit vector pointing away from the tip, along the line.
    let back = ((from.0 - tip.0) / length, (from.1 - tip.1) / length);
    let turn = |angle: f64| {
        let (sin, cos) = angle.sin_cos();
        (back.0 * cos - back.1 * sin, back.0 * sin + back.1 * cos)
    };
    let barb = |angle: f64| {
        let direction = turn(angle);
        (tip.0 + direction.0 * size, tip.1 + direction.1 * size)
    };
    let common = format!(
        "stroke=\"{stroke}\" stroke-width=\"{width:.2}\" stroke-linecap=\"round\" \
         stroke-linejoin=\"round\""
    );
    match head {
        Head::Arrow => {
            let (a, b) = (barb(0.38), barb(-0.38));
            let _ = write!(
                out,
                "<polyline points=\"{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}\" fill=\"none\" {common}/>",
                a.0, a.1, tip.0, tip.1, b.0, b.1
            );
        }
        Head::Triangle => {
            let (a, b) = (barb(0.45), barb(-0.45));
            let _ = write!(
                out,
                "<polygon points=\"{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}\" fill=\"{stroke}\" {common}/>",
                tip.0, tip.1, a.0, a.1, b.0, b.1
            );
        }
        Head::Bar => {
            let (a, b) = (
                barb(std::f64::consts::FRAC_PI_2),
                barb(-std::f64::consts::FRAC_PI_2),
            );
            let (a, b) = (
                (tip.0 + (a.0 - tip.0) * 0.5, tip.1 + (a.1 - tip.1) * 0.5),
                (tip.0 + (b.0 - tip.0) * 0.5, tip.1 + (b.1 - tip.1) * 0.5),
            );
            let _ = write!(
                out,
                "<line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" {common}/>",
                a.0, a.1, b.0, b.1
            );
        }
        Head::Dot => {
            let _ = write!(
                out,
                "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" fill=\"{stroke}\"/>",
                tip.0,
                tip.1,
                size * 0.3
            );
        }
    }
}

fn write_text(out: &mut String, e: &Element, dark: bool) {
    let lines: Vec<&str> = e.text.lines().collect();
    if lines.is_empty() {
        return;
    }
    let size = if e.font_size > 0.0 { e.font_size } else { 20.0 };
    let line_height = if e.height > 0.0 {
        e.height / lines.len() as f64
    } else {
        size * 1.25
    };
    let (x, anchor) = match e.align {
        TextAlign::Left => (e.x, "start"),
        TextAlign::Center => (e.x + e.width / 2.0, "middle"),
        TextAlign::Right => (e.x + e.width, "end"),
    };
    let color = paint(e.stroke.unwrap_or(INK), dark);
    let family = if e.monospace { MONO } else { SANS };
    for (row, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        // The middle of the line box, then down by a third of the em: close
        // enough to the baseline that the words sit inside the shape they label.
        let y = e.y + row as f64 * line_height + line_height / 2.0 + size * 0.33;
        let _ = write!(
            out,
            "<text x=\"{x:.2}\" y=\"{y:.2}\" font-size=\"{size:.1}\" font-family=\"{family}\" \
             fill=\"{color}\" text-anchor=\"{anchor}\" xml:space=\"preserve\">{}</text>",
            escape(line)
        );
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn path_length(points: &[(f64, f64)]) -> f64 {
    points
        .windows(2)
        .map(|pair| distance(pair[0], pair[1]))
        .sum()
}

/// A colour as SVG writes it, turned into its dark-mode counterpart when the
/// pane is dark.
///
/// The transform is Excalidraw's own dark mode — invert 93%, then rotate the hue
/// half a turn — so white paper becomes near-black, black ink becomes near-white
/// and a red stays a red.
fn paint(color: Rgb, dark: bool) -> String {
    let Rgb { r, g, b } = if dark { night(color) } else { color };
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn night(color: Rgb) -> Rgb {
    let invert = |c: u8| {
        let c = f64::from(c);
        0.93 * (255.0 - c) + 0.07 * c
    };
    let (r, g, b) = (invert(color.r), invert(color.g), invert(color.b));
    let clamp = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    // The standard hue-rotate matrix at 180 degrees.
    Rgb {
        r: clamp(-0.574 * r + 1.43 * g + 0.144 * b),
        g: clamp(0.426 * r + 0.43 * g + 0.144 * b),
        b: clamp(0.426 * r + 1.43 * g - 0.856 * b),
    }
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

/// The system's fonts, loaded once: reading them takes long enough to notice
/// and nothing about them changes while the program runs.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            // fontdb names Arial and Times as the generic families, which are
            // missing on most Linux machines — and text in a family nobody can
            // find is dropped, not substituted.
            let has = |db: &usvg::fontdb::Database, name: &str| {
                db.faces().any(|face| {
                    face.families
                        .iter()
                        .any(|(f, _)| f.eq_ignore_ascii_case(name))
                })
            };
            let pick = |db: &usvg::fontdb::Database, wanted: &[&str]| {
                wanted
                    .iter()
                    .find(|name| has(db, name))
                    .map(ToString::to_string)
                    .or_else(|| {
                        db.faces()
                            .find_map(|face| face.families.first().map(|(f, _)| f.clone()))
                    })
            };
            if let Some(sans) = pick(
                &db,
                &[
                    "Liberation Sans",
                    "DejaVu Sans",
                    "Noto Sans",
                    "Inter",
                    "Helvetica",
                    "Arial",
                ],
            ) {
                db.set_sans_serif_family(sans.clone());
                db.set_serif_family(sans.clone());
                db.set_cursive_family(sans.clone());
                db.set_fantasy_family(sans);
            }
            if let Some(mono) = pick(
                &db,
                &[
                    "DejaVu Sans Mono",
                    "Liberation Mono",
                    "Noto Sans Mono",
                    "Courier New",
                ],
            ) {
                db.set_monospace_family(mono);
            }
            Arc::new(db)
        })
        .clone()
}

/// Draws a diagram so it fits inside `max_width` x `max_height` pixels, keeping
/// its proportions. Scales up as well as down: this is vector art.
#[must_use]
pub fn rasterize(
    svg: &str,
    look: &Look,
    max_width: u32,
    max_height: u32,
) -> Option<image::DynamicImage> {
    let fonts = fonts();
    // Text that names no family at all gets the sans-serif the database was
    // pointed at above, not usvg's default Times New Roman.
    let options = usvg::Options {
        fontdb: fonts,
        font_family: "sans-serif".into(),
        ..usvg::Options::default()
    };
    let tree = guarded(|| usvg::Tree::from_str(svg, &options)).ok()?.ok()?;
    let size = tree.size();
    let (width, height) = (size.width(), size.height());
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    // Ceiling on a side, so a very tall page doesn't ask for a gigabyte.
    const MAX_SIDE: f32 = 8192.0;
    let scale = (max_width.max(1) as f32 / width)
        .min(max_height.max(1) as f32 / height)
        .min(MAX_SIDE / width)
        .min(MAX_SIDE / height);
    let pixel_width = ((width * scale).round() as u32).max(1);
    let pixel_height = ((height * scale).round() as u32).max(1);

    let mut pixmap = tiny_skia::Pixmap::new(pixel_width, pixel_height)?;
    // Painted opaque first, so premultiplied and straight alpha are the same
    // thing and no protocol has to be trusted with transparency.
    let [r, g, b] = look.background;
    pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, 255));
    guarded(|| {
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(
                pixel_width as f32 / width,
                pixel_height as f32 / height,
            ),
            &mut pixmap.as_mut(),
        );
    })
    .ok()?;

    image::RgbaImage::from_raw(pixel_width, pixel_height, pixmap.take())
        .map(image::DynamicImage::ImageRgba8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Look = Look {
        dark: true,
        background: [0x1e, 0x1e, 0x2e],
    };
    const LIGHT: Look = Look {
        dark: false,
        background: [255, 255, 255],
    };

    fn scene(elements: &str) -> Drawing {
        emeraldian_core::excalidraw::parse(&format!(r#"{{"elements": [{elements}]}}"#))
            .expect("a scene")
    }

    #[test]
    fn mermaid_source_becomes_a_sized_svg() {
        let svg = mermaid_svg(
            "flowchart LR\n  A[Start] --> B{Ok?}\n  B --> C[Done]",
            &LIGHT,
        )
        .expect("renders");
        assert!(svg.text.contains("<svg"));
        assert!(svg.width > 50.0 && svg.height > 20.0, "{svg:?}");
        assert!(svg.text.contains("Start"), "labels are in the drawing");
    }

    #[test]
    fn mermaid_follows_the_theme() {
        let dark = mermaid_svg("flowchart LR\n  A --> B", &DARK).expect("renders");
        assert!(
            dark.text.to_lowercase().contains("#1e1e2e"),
            "the pane's own background is the paper"
        );
    }

    #[test]
    fn broken_mermaid_is_an_error_not_a_crash() {
        assert!(mermaid_svg("this is not a diagram at all {{{", &LIGHT).is_err());
        assert!(mermaid_svg("", &LIGHT).is_err());
        assert!(!in_guarded_render(), "the guard is released again");
    }

    #[test]
    fn svg_size_reads_the_root_element() {
        assert_eq!(
            svg_size(
                r#"<svg xmlns="x" width="120px" height="80" viewBox="0 0 1 1"><g stroke-width="3"/></svg>"#
            ),
            Some((120.0, 80.0))
        );
        assert_eq!(
            svg_size(r#"<svg viewBox="0 0 300.5 150"></svg>"#),
            Some((300.5, 150.0))
        );
        assert_eq!(svg_size("<p>not svg</p>"), None);
    }

    #[test]
    fn a_scene_is_drawn_in_order_with_its_labels() {
        let drawing = scene(
            r##"{"type": "rectangle", "x": 0, "y": 0, "width": 100, "height": 50,
                 "strokeColor": "#1e1e1e", "backgroundColor": "#a5d8ff",
                 "fillStyle": "solid", "roundness": {"type": 3}},
                {"type": "text", "x": 10, "y": 10, "width": 80, "height": 25,
                 "text": "a < b & c", "fontSize": 20, "textAlign": "center"},
                {"type": "arrow", "x": 100, "y": 25, "width": 60, "height": 0,
                 "points": [[0, 0], [60, 0]], "strokeColor": "#e03131"}"##,
        );
        let svg = excalidraw_svg(&drawing, &LIGHT).expect("not empty");
        let text = &svg.text;
        let rect = text.find("<rect x=\"0.00\"").expect("the box");
        let label = text.find("<text").expect("the label");
        assert!(rect < label, "z-order is kept: the label is on top");
        assert!(text.contains("a &lt; b &amp; c"), "text is escaped");
        assert!(text.contains("text-anchor=\"middle\""));
        assert!(text.contains("<polyline"), "the arrow has a head");
        assert!(text.contains("#a5d8ff"), "a light pane keeps the colours");
        assert!(
            svg.width > 190.0,
            "the margin is part of the page: {}",
            svg.width
        );
    }

    #[test]
    fn a_dark_pane_gets_excalidraw_s_dark_mode() {
        let drawing = scene(
            r##"{"type": "rectangle", "x": 0, "y": 0, "width": 10, "height": 10,
                 "strokeColor": "#1e1e1e"}"##,
        );
        let svg = excalidraw_svg(&drawing, &DARK).expect("not empty");
        assert!(
            !svg.text.contains("stroke=\"#1e1e1e\""),
            "near-black ink on a dark pane would vanish"
        );
        let ink = night(INK);
        assert!(ink.r > 200 && ink.g > 200 && ink.b > 200, "{ink:?}");
    }

    #[test]
    fn an_empty_scene_has_nothing_to_draw() {
        assert!(excalidraw_svg(&scene(""), &LIGHT).is_none());
    }

    #[test]
    fn a_diagram_is_rasterised_to_the_box_it_is_given() {
        let svg = mermaid_svg("flowchart LR\n  A[One] --> B[Two]", &LIGHT).expect("renders");
        let image = rasterize(&svg.text, &LIGHT, 400, 400).expect("pixels");
        assert!(image.width() <= 400 && image.height() <= 400);
        assert!(
            image.width() == 400 || image.height() == 400,
            "it fills the box on one side: {}x{}",
            image.width(),
            image.height()
        );
        let rgba = image.to_rgba8();
        let ink = rgba
            .pixels()
            .filter(|p| p.0[0] < 128 && p.0[3] == 255)
            .count();
        assert!(
            ink > 100,
            "something was drawn on the paper: {ink} dark pixels"
        );
    }

    #[test]
    fn an_excalidraw_scene_is_rasterised_with_its_text() {
        let drawing = scene(
            r##"{"type": "rectangle", "x": 0, "y": 0, "width": 200, "height": 80,
                 "strokeColor": "#1e1e1e"},
                {"type": "text", "x": 20, "y": 25, "width": 160, "height": 25,
                 "text": "Hello", "fontSize": 20, "textAlign": "center"}"##,
        );
        let svg = excalidraw_svg(&drawing, &LIGHT).expect("not empty");
        let image = rasterize(&svg.text, &LIGHT, 600, 600).expect("pixels");
        let rgba = image.to_rgba8();
        // The label sits in the middle of the box; with the rectangle's own
        // outline far away, dark pixels there can only be glyphs.
        let (w, h) = (rgba.width(), rgba.height());
        let glyphs = (h / 3..2 * h / 3)
            .flat_map(|y| (w / 3..2 * w / 3).map(move |x| (x, y)))
            .filter(|&(x, y)| rgba.get_pixel(x, y).0[0] < 100)
            .count();
        assert!(glyphs > 50, "the text was drawn ({glyphs} dark pixels)");
    }
}
