//! Drawing Mermaid flowcharts on the terminal's character grid.
//!
//! The layout comes from [`emeraldian_core::mermaid`] in whole cells, so nodes
//! are boxes of box-drawing characters, edges are lines of the same with
//! arrowheads, and labels are text, all placed exactly where the layout says.
//! It is the same pictureless look as the Excalidraw canvas — no graphics
//! protocol, no dependency — in a form that lives in the document as ordinary
//! rows of text, so it scrolls and pans with the prose around it.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use emeraldian_core::mermaid::{self, Diagram, EdgeStyle, Link, NodeShape, Placed};
use emeraldian_theme::Palette;

/// Cells a diagram may take before it stays source; a runaway diagram would
/// otherwise be a gigabyte of spans.
const MAX_CELLS: usize = 200_000;

/// What a cell of the grid is part of, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Blank,
    Node,
    Label,
    Edge,
    EdgeLabel,
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    ch: char,
    role: Role,
}

struct Grid {
    rows: Vec<Vec<Cell>>,
}

impl Grid {
    fn new(width: usize, height: usize) -> Self {
        let blank = Cell {
            ch: ' ',
            role: Role::Blank,
        };
        Self {
            rows: vec![vec![blank; width]; height],
        }
    }

    fn get(&self, x: i64, y: i64) -> Option<&Cell> {
        self.rows
            .get(usize::try_from(y).ok()?)?
            .get(usize::try_from(x).ok()?)
    }

    fn put(&mut self, x: i64, y: i64, ch: char, role: Role) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if let Some(cell) = self.rows.get_mut(y).and_then(|row| row.get_mut(x)) {
            *cell = Cell { ch, role };
        }
    }

    /// Writes `text` from `(x, y)`; a wide character takes two cells, the
    /// second left as `\0` for the output to skip.
    fn text(&mut self, x: i64, y: i64, text: &str, role: Role) {
        let mut at = x;
        for ch in text.chars() {
            let width = ch.width().unwrap_or(0);
            if width == 0 {
                continue;
            }
            self.put(at, y, ch, role);
            if width == 2 {
                self.put(at + 1, y, '\0', role);
            }
            at += width as i64;
        }
    }
}

// Directions an edge cell connects in.
const N: u8 = 1;
const E: u8 = 2;
const S: u8 = 4;
const W: u8 = 8;

/// The character for a cell whose edge runs in the directions of `mask`.
fn line_glyph(mask: u8, style: EdgeStyle) -> char {
    let heavy = style == EdgeStyle::Thick;
    let dotted = style == EdgeStyle::Dotted;
    match (mask, heavy) {
        (0, _) => ' ',
        (m, false) if m == N | S && dotted => '┆',
        (m, false) if m == E | W && dotted => '┄',
        (N, false) => '╵',
        (E, false) => '╶',
        (S, false) => '╷',
        (W, false) => '╴',
        (m, false) if m == N | S => '│',
        (m, false) if m == E | W => '─',
        (m, false) if m == N | E => '╰',
        (m, false) if m == N | W => '╯',
        (m, false) if m == S | E => '╭',
        (m, false) if m == S | W => '╮',
        (m, false) if m == N | E | S => '├',
        (m, false) if m == N | S | W => '┤',
        (m, false) if m == E | S | W => '┬',
        (m, false) if m == N | E | W => '┴',
        (_, false) => '┼',
        (N, true) => '╹',
        (E, true) => '╺',
        (S, true) => '╻',
        (W, true) => '╸',
        (m, true) if m == N | S => '┃',
        (m, true) if m == E | W => '━',
        (m, true) if m == N | E => '┗',
        (m, true) if m == N | W => '┛',
        (m, true) if m == S | E => '┏',
        (m, true) if m == S | W => '┓',
        (m, true) if m == N | E | S => '┣',
        (m, true) if m == N | S | W => '┫',
        (m, true) if m == E | S | W => '┳',
        (m, true) if m == N | E | W => '┻',
        (_, true) => '╋',
    }
}

fn bit(dx: i64, dy: i64) -> u8 {
    match (dx.signum(), dy.signum()) {
        (0, -1) => N,
        (1, 0) => E,
        (0, 1) => S,
        (-1, 0) => W,
        _ => 0,
    }
}

/// Lays a laid-out diagram onto a grid of cells.
fn paint(diagram: &Diagram) -> Grid {
    let width = diagram.width.ceil() as usize;
    let height = diagram.height.ceil() as usize;
    let mut grid = Grid::new(width, height);

    for node in &diagram.nodes {
        draw_node(&mut grid, node);
    }

    // Edge cells first as sets of directions, so crossings and joins come out
    // as the right junction; characters are chosen once all are in.
    let mut mask = vec![vec![0_u8; width]; height];
    let mut styles = vec![vec![EdgeStyle::Solid; width]; height];
    let mut heads: Vec<(i64, i64, char)> = Vec::new();
    for link in &diagram.edges {
        let points: Vec<(i64, i64)> = link
            .points
            .iter()
            .map(|&(x, y)| (x as i64, y as i64))
            .collect();
        if points.len() < 2 {
            continue;
        }
        let last = points.len() - 1;
        for (i, pair) in points.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            let (dx, dy) = ((b.0 - a.0).signum(), (b.1 - a.1).signum());
            let length = (b.0 - a.0).abs().max((b.1 - a.1).abs());
            let (forward, backward) = (bit(dx, dy), bit(-dx, -dy));
            for k in 0..=length {
                // The last point is the border of the node it points at.
                if i + 1 == last && k == length {
                    break;
                }
                let mut m = 0;
                if k < length {
                    m |= forward;
                }
                if k > 0 || i == 0 {
                    m |= backward;
                }
                let (x, y) = (a.0 + dx * k, a.1 + dy * k);
                if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y))
                    && y < height
                    && x < width
                    && matches!(grid.rows[y][x].role, Role::Blank | Role::Edge)
                {
                    mask[y][x] |= m;
                    styles[y][x] = link.style;
                }
            }
        }
        let arrow = |dx: i64, dy: i64| match (dx.signum(), dy.signum()) {
            (0, 1) => '▼',
            (0, -1) => '▲',
            (1, 0) => '▶',
            _ => '◀',
        };
        if link.arrow_end {
            let (p, l) = (points[last - 1], points[last]);
            let (dx, dy) = (l.0 - p.0, l.1 - p.1);
            heads.push((l.0 - dx.signum(), l.1 - dy.signum(), arrow(dx, dy)));
        }
        if link.arrow_start {
            let (a, b) = (points[0], points[1]);
            let (dx, dy) = (a.0 - b.0, a.1 - b.1);
            heads.push((a.0, a.1, arrow(dx, dy)));
        }
    }
    for (y, row) in mask.iter().enumerate() {
        for (x, &m) in row.iter().enumerate() {
            if m != 0 {
                grid.put(x as i64, y as i64, line_glyph(m, styles[y][x]), Role::Edge);
            }
        }
    }
    for (x, y, ch) in heads {
        if matches!(
            grid.get(x, y).map(|cell| cell.role),
            Some(Role::Edge | Role::Blank)
        ) {
            grid.put(x, y, ch, Role::Edge);
        }
    }

    for link in &diagram.edges {
        join_to_source(&mut grid, link);
        if let Some((text, x, y)) = &link.label {
            grid.text(*x as i64, *y as i64, text, Role::EdgeLabel);
        }
    }
    grid
}

/// Where an edge leaves a straight border, the border becomes a junction, so
/// the line reads as growing out of the node.
fn join_to_source(grid: &mut Grid, link: &Link) {
    let [a, b, ..] = link.points.as_slice() else {
        return;
    };
    let (dx, dy) = (
        (b.0 as i64 - a.0 as i64).signum(),
        (b.1 as i64 - a.1 as i64).signum(),
    );
    let (x, y) = (a.0 as i64 - dx, a.1 as i64 - dy);
    let Some(cell) = grid.get(x, y) else { return };
    if cell.role != Role::Node || !matches!(cell.ch, '─' | '│') {
        return;
    }
    let junction = match (dx, dy) {
        (0, 1) => '┬',
        (0, -1) => '┴',
        (1, 0) => '├',
        _ => '┤',
    };
    grid.put(x, y, junction, Role::Node);
}

fn draw_node(grid: &mut Grid, node: &Placed) {
    let (x, y) = (node.x as i64, node.y as i64);
    let (w, h) = (node.width as i64, node.height as i64);
    let (top, bottom, left, right, side_left, side_right) = match node.shape {
        NodeShape::Box => (('┌', '┐'), ('└', '┘'), '│', '│', '│', '│'),
        NodeShape::Diamond => (('╱', '╲'), ('╲', '╱'), '│', '│', '<', '>'),
        NodeShape::Circle => (('╭', '╮'), ('╰', '╯'), '│', '│', '(', ')'),
    };
    grid.put(x, y, top.0, Role::Node);
    grid.put(x + w - 1, y, top.1, Role::Node);
    grid.put(x, y + h - 1, bottom.0, Role::Node);
    grid.put(x + w - 1, y + h - 1, bottom.1, Role::Node);
    for dx in 1..w - 1 {
        grid.put(x + dx, y, '─', Role::Node);
        grid.put(x + dx, y + h - 1, '─', Role::Node);
    }
    let middle = (node.lines.len() as i64 - 1) / 2;
    for (i, line) in node.lines.iter().enumerate() {
        let row = y + 1 + i as i64;
        let pointed = node.shape != NodeShape::Box && i as i64 == middle;
        grid.put(x, row, if pointed { side_left } else { left }, Role::Node);
        grid.put(
            x + w - 1,
            row,
            if pointed { side_right } else { right },
            Role::Node,
        );
        let text_width: i64 = line.chars().filter_map(|c| c.width()).sum::<usize>() as i64;
        let start = x + 1 + (w - 2 - text_width) / 2;
        grid.text(start, row, line, Role::Label);
    }
}

/// Draws Mermaid source as rows of styled text, each starting with `pad`.
///
/// # Errors
///
/// Why it stays source: a diagram type that is not drawn, a statement that
/// does not parse, or a diagram too large to be worth drawing.
pub fn lines(source: &str, palette: &Palette, pad: &str) -> Result<Vec<Line<'static>>, String> {
    let diagram = mermaid::diagram(source)?;
    if diagram.width * diagram.height > MAX_CELLS as f64 {
        return Err("too large to draw".into());
    }
    let grid = paint(&diagram);

    let style = |role: Role| match role {
        Role::Blank => Style::default(),
        Role::Node => Style::default().fg(palette.text_accent),
        Role::Label => Style::default().fg(palette.text_normal),
        Role::Edge => Style::default().fg(palette.text_muted),
        Role::EdgeLabel => Style::default().fg(palette.text_faint),
    };
    let mut out = Vec::with_capacity(grid.rows.len());
    for row in &grid.rows {
        let end = row
            .iter()
            .rposition(|cell| cell.role != Role::Blank)
            .map_or(0, |at| at + 1);
        let mut spans = vec![Span::raw(pad.to_string())];
        let mut run = String::new();
        let mut role = Role::Blank;
        for cell in &row[..end] {
            if cell.ch == '\0' {
                continue;
            }
            if cell.role != role && !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), style(role)));
            }
            role = cell.role;
            run.push(cell.ch);
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, style(role)));
        }
        out.push(Line::from(spans));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emeraldian_theme::presets;
    use unicode_width::UnicodeWidthStr;

    fn palette() -> Palette {
        Palette::from(&presets::default_theme())
    }

    fn rows(source: &str) -> Vec<String> {
        lines(source, &palette(), "")
            .expect("drawn")
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The row a text first appears on.
    fn row_of(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in\n{}", rows.join("\n")))
    }

    /// The column, in cells, a text starts at on its row.
    fn column_of(rows: &[String], needle: &str) -> usize {
        let row = &rows[row_of(rows, needle)];
        UnicodeWidthStr::width(&row[..row.find(needle).expect("found")])
    }

    #[test]
    fn a_top_down_flow_is_boxes_joined_by_a_line_with_an_arrowhead() {
        let rows = rows("flowchart TD\n  A[Root] --> B[Left]");
        let shown = rows.join("\n");
        assert!(rows[row_of(&rows, "Root") - 1].contains('┌'), "{shown}");
        assert!(shown.contains('┬'), "the line grows out of Root:\n{shown}");
        assert!(shown.contains('▼'), "and ends in a head:\n{shown}");
        assert!(
            row_of(&rows, "Root") < row_of(&rows, "▼")
                && row_of(&rows, "▼") < row_of(&rows, "Left"),
            "Root, the head, Left, top to bottom:\n{shown}"
        );
        assert_eq!(
            column_of(&rows, "▼"),
            column_of(&rows, "┬"),
            "straight down:\n{shown}"
        );
    }

    #[test]
    fn a_left_to_right_flow_puts_the_nodes_on_one_row_with_a_head_between() {
        let rows = rows("flowchart LR\n  A[Start] --> B[Done]");
        let shown = rows.join("\n");
        let row = &rows[row_of(&rows, "Start")];
        assert!(row.contains("Done"), "one row:\n{shown}");
        assert!(row.contains('├'), "{shown}");
        assert!(row.contains('▶'), "{shown}");
        assert!(
            row.find("Start") < row.find('▶') && row.find('▶') < row.find("Done"),
            "{shown}"
        );
    }

    #[test]
    fn the_other_two_directions_point_the_other_two_ways() {
        let up = rows("flowchart BT\n  A[Start] --> B[Done]");
        assert!(up.join("\n").contains('▲'));
        assert!(row_of(&up, "Done") < row_of(&up, "Start"));
        let left = rows("flowchart RL\n  A[Start] --> B[Done]");
        assert!(left.join("\n").contains('◀'));
        assert!(column_of(&left, "Done") < column_of(&left, "Start"));
    }

    #[test]
    fn a_branch_fans_out_and_both_arms_end_in_heads() {
        let rows = rows("flowchart TD\n  A[Root] --> B[Left]\n  A --> C[Right]");
        let shown = rows.join("\n");
        assert_eq!(shown.matches('▼').count(), 2, "{shown}");
        assert!(shown.contains('╭') && shown.contains('╮'), "{shown}");
        assert_eq!(row_of(&rows, "Left"), row_of(&rows, "Right"), "{shown}");
        assert!(column_of(&rows, "Left") < column_of(&rows, "Right"));
    }

    #[test]
    fn an_edge_label_is_written_beside_its_line() {
        let rows = rows("flowchart TD\n  A -->|yes| B");
        let shown = rows.join("\n");
        assert!(
            row_of(&rows, "A") < row_of(&rows, "yes") && row_of(&rows, "yes") < row_of(&rows, "B"),
            "{shown}"
        );
        assert!(
            column_of(&rows, "yes") > column_of(&rows, "▼"),
            "to the right of the line, not on it:\n{shown}"
        );
    }

    #[test]
    fn a_decision_is_drawn_pointed_and_a_circle_round() {
        let shown = rows("flowchart TD\n  A{Ok?}").join("\n");
        assert!(shown.contains('╱') && shown.contains('╲'), "{shown}");
        assert!(shown.contains("< ") && shown.contains(" >"), "{shown}");
        assert!(shown.contains("Ok?"));
        let round = rows("flowchart TD\n  A((Dot))").join("\n");
        assert!(round.contains('╭') && round.contains('╯'), "{round}");
        assert!(round.contains("( ") && round.contains(" )"), "{round}");
    }

    #[test]
    fn dotted_and_thick_edges_look_different_from_plain_ones() {
        let dotted = rows("flowchart TD\n  A -.-> B").join("\n");
        assert!(dotted.contains('┆'), "{dotted}");
        let thick = rows("flowchart TD\n  A ==> B").join("\n");
        assert!(thick.contains('┃'), "{thick}");
        let open = rows("flowchart TD\n  A --- B").join("\n");
        assert!(open.contains('│') && !open.contains('▼'), "{open}");
    }

    #[test]
    fn an_edge_that_closes_a_cycle_points_back_up() {
        let shown = rows("flowchart TD\n  A --> B\n  B --> C\n  C --> A").join("\n");
        assert!(shown.contains('▲'), "the way back is an arrow up:\n{shown}");
        assert!(shown.contains('▼'), "{shown}");
    }

    #[test]
    fn a_wide_label_keeps_its_box_square() {
        let rows = rows("flowchart TD\n  A[日本語]");
        let shown = rows.join("\n");
        let width = |needle: &str| UnicodeWidthStr::width(rows[row_of(&rows, needle)].as_str());
        assert_eq!(width("┌"), width("日本語"), "{shown}");
        assert_eq!(width("└"), width("日本語"), "{shown}");
    }

    #[test]
    fn a_way_back_runs_beside_the_way_forward_not_on_it() {
        let rows = rows("flowchart TD\n  A[Start node] --> B[Stop node]\n  B --> A");
        let shown = rows.join("\n");
        assert!(shown.contains('▼') && shown.contains('▲'), "{shown}");
        assert_ne!(column_of(&rows, "▼"), column_of(&rows, "▲"), "{shown}");
        let stop = row_of(&rows, "Stop node");
        assert!(
            !rows[stop].contains('╵') && rows[stop - 1].contains('─'),
            "nothing ends up inside the node:\n{shown}"
        );
    }

    #[test]
    fn the_drawing_is_coloured_by_what_each_part_is() {
        let palette = palette();
        let lines = lines("flowchart TD\n  A[Root] --> B[Left]", &palette, "").expect("drawn");
        let span = |needle: &str| {
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains(needle))
                .unwrap_or_else(|| panic!("no span with {needle:?}"))
        };
        assert_eq!(span("┌").style.fg, Some(palette.text_accent));
        assert_eq!(span("Root").style.fg, Some(palette.text_normal));
        assert_eq!(span("▼").style.fg, Some(palette.text_muted));
    }

    #[test]
    fn the_pad_starts_every_row() {
        let lines = lines("flowchart TD\n  A --> B", &palette(), "  ").expect("drawn");
        assert!(
            lines
                .iter()
                .all(|line| line.spans[0].content.as_ref() == "  ")
        );
    }

    #[test]
    fn what_cannot_be_drawn_says_why_instead() {
        let palette = palette();
        let reason = lines("sequenceDiagram\n  A->>B: hi", &palette, "").expect_err("not drawn");
        assert!(reason.contains("sequenceDiagram"), "{reason}");
        let reason = lines("flowchart TD\n  A -->", &palette, "").expect_err("broken");
        assert!(reason.contains("edge"), "{reason}");
        let many: String = (0..160).map(|n| format!("N{n} --> N{}\n", n + 1)).collect();
        let reason = lines(&format!("flowchart TD\n{many}"), &palette, "").expect_err("huge");
        assert!(reason.contains("too large"), "{reason}");
    }
}
