//! Drawing Excalidraw scenes as vectors.
//!
//! The shapes are drawn rather than rasterised: a braille canvas gives two dots
//! across and four down per cell, which is enough resolution that a box-and-arrow
//! diagram stays a box-and-arrow diagram. It also means a drawing reads the same
//! in every terminal, with no dependence on the picture protocols that
//! [`crate::images`] needs.
//!
//! Outlines only, no fills. Excalidraw's fills are pale washes behind text; as
//! solid blocks of terminal colour they would bury the labels they sit behind,
//! and the labels are the part worth reading.
//!
//! The scene is scaled to the pane's width and scrolled vertically, like the
//! prose it replaces — diagrams are often far taller than a terminal, and
//! shrinking one to fit turns it into grey fuzz.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine};
use ratatui::widgets::{Paragraph, Widget};

use emeraldian_core::excalidraw::{Drawing, Element, Rgb, Shape};
use emeraldian_theme::Palette;

use crate::ui::scrollbar;

/// Braille dots per cell.
const DOTS_X: f64 = 2.0;
const DOTS_Y: f64 = 4.0;

/// Points used to trace an ellipse. Enough that the largest circle a terminal
/// can show has no visible corners.
const ELLIPSE_STEPS: usize = 48;

/// Length of an arrowhead's barbs, in dots.
const ARROWHEAD_DOTS: f64 = 4.0;

/// Length of one dash and the gap after it, in dots.
const DASH_DOTS: f64 = 3.0;

/// The last scene parsed, so scrolling a drawing doesn't decompress it again on
/// every frame.
///
/// Obsidian's compressed format costs a couple of milliseconds for a busy
/// drawing, which is fine once and sticky when it happens on every keystroke.
/// Keyed by the file's contents rather than its name: an edit is picked up
/// without asking, and only one drawing is ever on screen, so one slot is enough.
#[derive(Default)]
pub struct Scenes {
    key: Option<u64>,
    scene: Option<Drawing>,
}

impl Scenes {
    /// The drawing in `content`, parsing it only if it has changed.
    pub fn get(&mut self, content: &str) -> Option<&Drawing> {
        let key = digest(content);
        if self.key != Some(key) {
            self.key = Some(key);
            self.scene = emeraldian_core::excalidraw::parse(content);
        }
        self.scene.as_ref()
    }
}

fn digest(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Draws a scene, scrolled to `scroll` rows from its top.
///
/// Returns the number of rows the whole scene needs, which is what the caller
/// clamps its scroll against.
pub fn draw(
    frame: &mut Frame,
    palette: &Palette,
    area: Rect,
    drawing: &Drawing,
    scroll: usize,
) -> usize {
    let rows = paint(frame, palette, area, drawing, scroll);
    if rows > 0 {
        scrollbar(frame, palette, area, scroll, rows);
    }
    rows
}

/// A scene's rows and columns when it sits in a note among the prose: as wide
/// as `max_cols` allows but no bigger than life (a cell is about 8 by 16 scene
/// units), narrowed until it is no taller than `max_rows` if it can be.
///
/// `None` for an empty drawing.
#[must_use]
pub fn fit(drawing: &Drawing, max_cols: usize, max_rows: usize) -> Option<(u16, u16)> {
    let (_, _, width, height) = drawing.bounds()?;
    let (width, height) = (width.max(1.0), height.max(1.0));
    let natural = (width / 8.0).ceil();
    // rows = ceil(height * cols / (2 * width)), so this many columns fill the cap.
    let by_height = (2.0 * max_rows as f64 * width / height).floor();
    let cols = (max_cols as f64).min(natural).min(by_height).max(6.0);
    let cols = cols.min(max_cols.max(1) as f64);
    let rows = (height * cols / (2.0 * width)).ceil().max(1.0);
    Some((cols as u16, rows as u16))
}

/// Draws a scene into `area` and nothing else: no scrollbar, no frame round it.
/// The scene is scaled to the area's width and starts `scroll` rows down.
///
/// Returns the number of rows the whole scene needs at that width.
pub fn paint(
    frame: &mut Frame,
    palette: &Palette,
    area: Rect,
    drawing: &Drawing,
    scroll: usize,
) -> usize {
    let Some((left, top, width, height)) = drawing.bounds() else {
        Paragraph::new(Line::from(Span::styled(
            "this drawing is empty",
            Style::default().fg(palette.text_faint),
        )))
        .render(area, frame.buffer_mut());
        return 0;
    };

    let dots_across = f64::from(area.width.max(1)) * DOTS_X;
    // A scene with no width — a single vertical line — would divide by zero, and
    // still deserves to be seen.
    let scale = dots_across / width.max(1.0);
    let visible = f64::from(area.height) * DOTS_Y / scale;
    let rows = (height * scale / DOTS_Y).ceil().max(1.0);
    let rows = rows as usize;

    // Canvas y runs upward and Excalidraw's runs downward, so everything is
    // drawn negated and the window is expressed in those terms too.
    let scrolled = top + (scroll as f64) * DOTS_Y / scale;
    let x_bounds = [left, left + dots_across / scale];
    let y_bounds = [-(scrolled + visible), -scrolled];

    let ink = |color: Option<Rgb>, fallback: Option<Rgb>| readable(color.or(fallback), palette);

    let canvas = Canvas::default()
        .background_color(palette.bg_primary)
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(|ctx: &mut Context| {
            for element in &drawing.elements {
                let color = ink(element.stroke, element.fill);
                match element.shape {
                    Shape::Text => {}
                    _ => stroke(ctx, element, color, scale),
                }
            }
            // Text last, so a label sits on top of the box drawn around it.
            for element in &drawing.elements {
                if element.shape == Shape::Text && !element.text.is_empty() {
                    let color = ink(element.stroke, None);
                    for (row, text) in element.text.lines().enumerate() {
                        // A text element's y is its top, and print places a
                        // baseline, so each line drops by its own height.
                        let line_height =
                            element.height / element.text.lines().count().max(1) as f64;
                        let y = -(element.y + (row as f64 + 0.8) * line_height);
                        ctx.print(
                            element.x,
                            y,
                            Span::styled(text.to_string(), Style::default().fg(color)),
                        );
                    }
                }
            }
        });

    frame.render_widget(canvas, area);
    rows
}

/// Traces one element's outline onto the canvas.
fn stroke(ctx: &mut Context, element: &Element, color: Color, scale: f64) {
    let path = outline(element);
    for pair in path.windows(2) {
        let [(x1, y1), (x2, y2)] = [pair[0], pair[1]];
        if element.dashed {
            dashes(ctx, (x1, y1), (x2, y2), color, scale);
        } else {
            ctx.draw(&CanvasLine {
                x1,
                y1: -y1,
                x2,
                y2: -y2,
                color,
            });
        }
    }

    if element.shape == Shape::Arrow
        && let [.., from, to] = path.as_slice()
    {
        for barb in arrowhead(*from, *to, scale) {
            ctx.draw(&CanvasLine {
                x1: to.0,
                y1: -to.1,
                x2: barb.0,
                y2: -barb.1,
                color,
            });
        }
    }
}

/// The points an element's outline passes through, in Excalidraw coordinates.
fn outline(element: &Element) -> Vec<(f64, f64)> {
    let (x, y, w, h) = (element.x, element.y, element.width, element.height);
    let centre = (x + w / 2.0, y + h / 2.0);

    let points = match element.shape {
        Shape::Rectangle | Shape::Frame => {
            vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)]
        }
        Shape::Diamond => vec![
            (centre.0, y),
            (x + w, centre.1),
            (centre.0, y + h),
            (x, centre.1),
            (centre.0, y),
        ],
        Shape::Ellipse => (0..=ELLIPSE_STEPS)
            .map(|step| {
                let angle = std::f64::consts::TAU * step as f64 / ELLIPSE_STEPS as f64;
                (
                    centre.0 + angle.cos() * w / 2.0,
                    centre.1 + angle.sin() * h / 2.0,
                )
            })
            .collect(),
        Shape::Line | Shape::Arrow | Shape::Freehand => element
            .points
            .iter()
            .map(|&(dx, dy)| (x + dx, y + dy))
            .collect(),
        Shape::Text => Vec::new(),
    };

    if element.angle == 0.0 {
        return points;
    }
    let (sin, cos) = element.angle.sin_cos();
    points
        .into_iter()
        .map(|(px, py)| {
            let (dx, dy) = (px - centre.0, py - centre.1);
            (
                centre.0 + dx * cos - dy * sin,
                centre.1 + dx * sin + dy * cos,
            )
        })
        .collect()
}

/// Draws a segment as a dashed line.
fn dashes(ctx: &mut Context, from: (f64, f64), to: (f64, f64), color: Color, scale: f64) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    // A dash is measured on screen, not in the scene, so dashes stay the same
    // length whether the drawing is scaled up or down.
    let dash = DASH_DOTS / scale.max(f64::MIN_POSITIVE);
    if length <= dash || !length.is_finite() {
        ctx.draw(&CanvasLine {
            x1: from.0,
            y1: -from.1,
            x2: to.0,
            y2: -to.1,
            color,
        });
        return;
    }

    let steps = (length / (dash * 2.0)).ceil() as usize;
    let at = |t: f64| (from.0 + dx * t, from.1 + dy * t);
    for step in 0..steps {
        let start = (step as f64 * dash * 2.0) / length;
        let end = (start + dash / length).min(1.0);
        let (x1, y1) = at(start);
        let (x2, y2) = at(end);
        ctx.draw(&CanvasLine {
            x1,
            y1: -y1,
            x2,
            y2: -y2,
            color,
        });
    }
}

/// The two barbs of an arrowhead pointing along `from` → `to`.
fn arrowhead(from: (f64, f64), to: (f64, f64), scale: f64) -> [(f64, f64); 2] {
    let heading = (to.1 - from.1).atan2(to.0 - from.0);
    let length = ARROWHEAD_DOTS / scale.max(f64::MIN_POSITIVE);
    // Wide enough to read as a head at four dots long, narrow enough that it
    // still points somewhere.
    let spread = 0.4;
    [heading + spread, heading - spread]
        .map(|angle| (to.0 - angle.cos() * length, to.1 - angle.sin() * length))
}

/// How close in brightness a stroke may come to the background before it is
/// swapped for the theme's own text colour.
///
/// Excalidraw's default ink is `#1e1e1e` on white paper, which on a dark
/// terminal is the background exactly. Its palette colours sit much further
/// out — the darkest, `#343a40`, is 0.11 away and the vivid ones 0.2 and up — so
/// this catches the two that would vanish and keeps the rest.
const MIN_CONTRAST: f64 = 0.12;

/// Picks a colour that can actually be seen against the theme.
///
/// Excalidraw draws on white paper and most terminals are dark, so taking its
/// colours literally makes the average drawing invisible. Only the strokes that
/// would disappear are replaced; a diagram that colours things means it, and the
/// same rule rescues pale strokes on a light theme.
fn readable(color: Option<Rgb>, palette: &Palette) -> Color {
    let Some(Rgb { r, g, b }) = color else {
        return palette.text_normal;
    };
    let background = match palette.bg_primary {
        Color::Rgb(r, g, b) => luminance(r, g, b),
        // A theme inheriting the terminal's own palette doesn't say what the
        // background is, so assume the dark one most terminals have.
        _ => 0.0,
    };
    if (luminance(r, g, b) - background).abs() < MIN_CONTRAST {
        return palette.text_normal;
    }
    Color::Rgb(r, g, b)
}

/// Perceived brightness, 0.0 to 1.0.
fn luminance(r: u8, g: u8, b: u8) -> f64 {
    (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use emeraldian_theme::presets;

    fn element(shape: Shape) -> Element {
        Element {
            shape,
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 40.0,
            angle: 0.0,
            stroke: None,
            fill: None,
            dashed: false,
            points: Vec::new(),
            text: String::new(),
        }
    }

    #[test]
    fn an_inline_drawing_is_fitted_to_the_width_and_never_blown_up() {
        let scene = |w: u32, h: u32| {
            emeraldian_core::excalidraw::parse(&format!(
                r##"{{"elements": [{{"type": "rectangle", "x": 0, "y": 0, "width": {w}, "height": {h}}}]}}"##
            ))
            .expect("a scene")
        };
        // Wide: as wide as the room, as tall as its shape says.
        let (cols, rows) = fit(&scene(800, 400), 60, 24).expect("fits");
        assert_eq!(cols, 60);
        assert_eq!(
            rows, 15,
            "400 tall at 60 cells across: 400 * 60 / (2 * 800)"
        );
        // Small: drawn at about its own size, not stretched over the pane.
        let (cols, _) = fit(&scene(80, 40), 60, 24).expect("fits");
        assert!(cols < 20, "{cols} cells for a drawing 80 units wide");
        // Tall: narrowed until it stops being taller than the cap.
        let (cols, rows) = fit(&scene(400, 800), 60, 24).expect("fits");
        assert!(rows <= 24 && cols < 60, "{cols}x{rows}");
        assert_eq!(
            fit(&emeraldian_core::excalidraw::Drawing::default(), 60, 24),
            None,
            "an empty drawing has no size"
        );
    }

    #[test]
    fn a_drawing_painted_into_a_rect_stays_inside_it_and_scrolls_within_it() {
        let drawing = emeraldian_core::excalidraw::parse(
            r##"{"elements": [
                {"type": "rectangle", "x": 0, "y": 0, "width": 200, "height": 400},
                {"type": "line", "x": 0, "y": 400, "width": 200, "height": 0,
                 "points": [[0, 0], [200, 0]]}]}"##,
        )
        .expect("a scene");
        let palette = Palette::from(&presets::default_theme());
        let area = Rect::new(5, 3, 20, 6);
        let paint_at = |scroll: usize| {
            let backend = ratatui::backend::TestBackend::new(40, 20);
            let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
            let mut rows = 0;
            terminal
                .draw(|frame| rows = paint(frame, &palette, area, &drawing, scroll))
                .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            (rows, buffer)
        };
        let braille = |cell: &ratatui::buffer::Cell| {
            cell.symbol()
                .chars()
                .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
        };

        let (rows, top) = paint_at(0);
        assert!(
            rows > 6,
            "the whole drawing needs more rows than it was given"
        );
        for y in 0..20 {
            for x in 0..40 {
                if !area.contains((x, y).into()) {
                    assert!(!braille(&top[(x, y)]), "ink escaped the rect at {x},{y}");
                }
            }
        }
        assert!((3..9).any(|y| (5..25).any(|x| braille(&top[(x, y)]))));
        let (_, scrolled) = paint_at(rows - 6);
        assert_ne!(
            (3..9)
                .map(|y| (5..25)
                    .map(|x| scrolled[(x, y)].symbol())
                    .collect::<String>())
                .collect::<Vec<_>>(),
            (3..9)
                .map(|y| (5..25).map(|x| top[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>(),
            "scrolling shows another part of the drawing"
        );
    }

    #[test]
    fn a_scene_is_parsed_once_but_an_edit_is_noticed() {
        let scene = |width: u32| {
            format!(
                r##"{{"elements": [{{"type": "rectangle", "x": 0, "y": 0,
                     "width": {width}, "height": 10}}]}}"##
            )
        };
        let mut scenes = Scenes::default();

        let first = scenes.get(&scene(100)).cloned().expect("parsed");
        assert_eq!(first.bounds(), Some((0.0, 0.0, 100.0, 10.0)));
        assert_eq!(
            scenes.get(&scene(100)).cloned(),
            Some(first),
            "the same text gives the same scene back"
        );
        assert_eq!(
            scenes.get(&scene(200)).and_then(Drawing::bounds),
            Some((0.0, 0.0, 200.0, 10.0)),
            "an edited drawing is read again rather than served stale"
        );
        assert!(
            scenes.get("# just a note\n").is_none(),
            "and a note with no drawing in it clears the slot"
        );
    }

    #[test]
    fn a_rectangle_closes_back_on_itself() {
        let path = outline(&element(Shape::Rectangle));
        assert_eq!(path.len(), 5);
        assert_eq!(
            path.first(),
            path.last(),
            "an outline that doesn't close leaves a gap in one corner"
        );
        assert!(path.contains(&(110.0, 60.0)), "the far corner: {path:?}");
    }

    #[test]
    fn a_diamond_touches_the_middle_of_each_side() {
        let path = outline(&element(Shape::Diamond));
        assert!(path.contains(&(60.0, 20.0)), "top");
        assert!(path.contains(&(110.0, 40.0)), "right");
        assert!(path.contains(&(60.0, 60.0)), "bottom");
        assert!(path.contains(&(10.0, 40.0)), "left");
    }

    #[test]
    fn an_ellipse_stays_within_its_box() {
        let path = outline(&element(Shape::Ellipse));
        assert!(path.len() > 24, "smooth enough to have no corners");
        for &(x, y) in &path {
            assert!((10.0..=110.0).contains(&x), "x escaped: {x}");
            assert!((20.0..=60.0).contains(&y), "y escaped: {y}");
        }
    }

    #[test]
    fn rotation_turns_a_shape_about_its_own_centre() {
        let mut rotated = element(Shape::Rectangle);
        rotated.angle = std::f64::consts::FRAC_PI_2;
        let path = outline(&rotated);

        let centre = (60.0, 40.0);
        for &(x, y) in &path {
            let radius = (x - centre.0).hypot(y - centre.1);
            assert!(
                (radius - 53.851).abs() < 0.01,
                "a quarter turn keeps every corner the same distance out: {radius}"
            );
        }
        assert!(
            path.iter().all(|&(_, y)| y < 90.1),
            "and the corners moved, rather than the whole shape drifting: {path:?}"
        );
    }

    #[test]
    fn a_line_follows_its_own_points_not_its_bounding_box() {
        let mut line = element(Shape::Line);
        line.points = vec![(0.0, 0.0), (-30.0, 15.0), (50.0, 15.0)];
        assert_eq!(
            outline(&line),
            vec![(10.0, 20.0), (-20.0, 35.0), (60.0, 35.0)],
            "points are offsets from the element's origin, and may be negative"
        );
    }

    #[test]
    fn an_arrowhead_sits_behind_the_tip_and_spreads_both_ways() {
        let barbs = arrowhead((0.0, 0.0), (10.0, 0.0), 1.0);
        for (x, y) in barbs {
            assert!(x < 10.0, "barbs trail the tip rather than passing it");
            assert!((x - 10.0).hypot(y) < ARROWHEAD_DOTS + 0.01);
        }
        assert!(
            barbs[0].1 * barbs[1].1 < 0.0,
            "one either side of the shaft: {barbs:?}"
        );
    }

    #[test]
    fn near_black_ink_is_swapped_for_something_visible_on_a_dark_theme() {
        let dark = Palette::from(&presets::default_theme());
        let excalidraw_default = Some(Rgb {
            r: 30,
            g: 30,
            b: 30,
        });
        assert_eq!(
            readable(excalidraw_default, &dark),
            dark.text_normal,
            "Excalidraw's default ink is near-black, and so is a dark terminal"
        );

        let red = Some(Rgb {
            r: 224,
            g: 49,
            b: 49,
        });
        assert_eq!(
            readable(red, &dark),
            Color::Rgb(224, 49, 49),
            "a colour the author chose deliberately is kept"
        );
        assert_eq!(
            readable(None, &dark),
            dark.text_normal,
            "a shape with no colour at all still gets drawn"
        );
    }

    #[test]
    fn a_pale_stroke_is_rescued_on_a_light_theme_too() {
        let light =
            Palette::from(&presets::builtin_by_name("obsidian-light").expect("a light preset"));
        let nearly_paper = Some(Rgb {
            r: 248,
            g: 249,
            b: 250,
        });
        assert_eq!(readable(nearly_paper, &light), light.text_normal);

        let ink = Some(Rgb {
            r: 30,
            g: 30,
            b: 30,
        });
        assert_eq!(
            readable(ink, &light),
            Color::Rgb(30, 30, 30),
            "and the same near-black ink is fine on paper, where it was meant to go"
        );
    }
}
