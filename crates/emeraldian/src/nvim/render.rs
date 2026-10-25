//! Paints a session's grid into the note pane.
//!
//! The grid is written into ratatui's buffer cell by cell rather than through a
//! `Paragraph`: nvim already decided where every character sits, and handing
//! the text to a widget would let ratatui lay it out a second time (and wrap
//! it) with its own idea of widths.

use emeraldian_theme::Palette;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use super::grid::{Cell, Grid};

/// Draw `grid` into `area`, one nvim cell per screen cell, and put the caret
/// where nvim's cursor is while the pane has the focus.
///
/// `area` and the grid can disagree for a frame while a resize is on its way to
/// nvim and back. The overlap is drawn; what the grid does not cover is the
/// theme's background, and what does not fit in `area` is left out.
pub fn draw(frame: &mut Frame, area: Rect, grid: &Grid, palette: &Palette, focused: bool) {
    let area = area.intersection(frame.area());
    let blank = Style::default()
        .fg(palette.text_normal)
        .bg(palette.bg_primary);
    let buffer = frame.buffer_mut();
    for row in 0..area.height {
        for col in 0..area.width {
            buffer[(area.x + col, area.y + row)]
                .set_symbol(" ")
                .set_style(blank);
        }
    }

    let width = usize::from(grid.width);
    let columns = area.width.min(grid.width);
    let rows = area.height.min(grid.height);
    for row in 0..rows {
        let mut col = 0;
        while col < columns {
            let Some(cell) = grid.cells.get(usize::from(row) * width + usize::from(col)) else {
                break;
            };
            let next = grid
                .cells
                .get(usize::from(row) * width + usize::from(col) + 1);
            let style = style_of(grid, cell);
            let target = &mut buffer[(area.x + col, area.y + row)];
            target.set_style(style);

            // A double-width character fills its own cell and the one after
            // it, which nvim sends as `""`. Only a character both sides agree
            // is two wide may use both: when ratatui would skip a cell nvim
            // thinks is separate, everything after it on the row shifts.
            // Only when the continuation is inside the pane, too — ratatui
            // writes the second half of the glyph into the cell after it, and
            // a glyph sitting in the pane's last column would erase the cell
            // *outside* the pane, which this frame may be borrowing from
            // another widget mid-resize. A clipped half draws as a space.
            let wide = cell_width(cell) == 2
                && next.is_some_and(|next| next.text.is_empty())
                && col + 1 < columns;
            if cell.text.is_empty() {
                // The continuation of a wide character that was cut off by the
                // pane's edge or by a scroll; the space keeps the columns.
                target.set_symbol(" ");
            } else if cell_width(cell) == 1 || wide {
                target.set_symbol(&cell.text);
            } else {
                target.set_symbol(" ");
            }

            if wide && col + 1 < columns {
                // Ratatui draws the second half itself; it only needs the same
                // colours so a background spans both.
                buffer[(area.x + col + 1, area.y + row)].set_style(style);
                col += 2;
            } else {
                col += 1;
            }
        }
    }

    if focused && let Some(position) = cursor_at(area, grid) {
        frame.set_cursor_position(position);
    }
}

/// Where nvim's cursor belongs on screen, when it is somewhere the pane shows.
#[must_use]
pub fn cursor_at(area: Rect, grid: &Grid) -> Option<(u16, u16)> {
    // nvim hides its cursor while it is busy, and its position is not
    // meaningful then.
    if grid.busy {
        return None;
    }
    let (col, row) = grid.cursor;
    if col >= grid.width || row >= grid.height || col >= area.width || row >= area.height {
        return None;
    }
    Some((area.x + col, area.y + row))
}

/// How many columns a cell's text takes. Zero-width and over-wide clusters are
/// reported as `0` so the caller draws a space instead of shifting the row.
fn cell_width(cell: &Cell) -> usize {
    match UnicodeWidthStr::width(cell.text.as_str()) {
        width @ (1 | 2) => width,
        _ => 0,
    }
}

/// The colours and attributes of one cell, from nvim's highlight table.
fn style_of(grid: &Grid, cell: &Cell) -> Style {
    let attr = grid.hl.get(&cell.hl).cloned().unwrap_or_default();
    let mut fg = attr.fg.unwrap_or(grid.default.fg);
    let mut bg = attr.bg.unwrap_or(grid.default.bg);
    if attr.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = Style::default().fg(rgb(fg)).bg(rgb(bg));
    let mut modifiers = Modifier::empty();
    if attr.bold {
        modifiers |= Modifier::BOLD;
    }
    if attr.italic {
        modifiers |= Modifier::ITALIC;
    }
    // A terminal cell has no wavy line of its own to promise, so an undercurl
    // is a plain underline in the special colour: the mark stays, as spelling
    // and diagnostics rely on it.
    if attr.underline || attr.undercurl {
        modifiers |= Modifier::UNDERLINED;
        if attr.undercurl || attr.sp.is_some() {
            style = style.underline_color(rgb(attr.sp.unwrap_or(grid.default.sp)));
        }
    }
    if attr.strikethrough {
        modifiers |= Modifier::CROSSED_OUT;
    }
    style.add_modifier(modifiers)
}

fn rgb(value: u32) -> Color {
    Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emeraldian_theme::presets;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use crate::nvim::grid::HlAttr;

    fn palette() -> Palette {
        Palette::from(&presets::default_theme())
    }

    /// A grid holding `rows`, one character per cell.
    fn grid(width: u16, height: u16, rows: &[&str]) -> Grid {
        let mut grid = Grid::new(width, height);
        for (index, text) in rows.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                grid.cells[index * usize::from(width) + col].text = ch.to_string();
            }
        }
        grid
    }

    fn draw_into(width: u16, height: u16, area: Rect, grid: &Grid, focused: bool) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw(frame, area, grid, &palette(), focused))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        // The cell after a double-width character is covered by it, as in a
        // terminal, so it is not part of the text.
        (0..height)
            .map(|row| {
                let mut line = String::new();
                let mut col = 0;
                while col < width {
                    let symbol = buffer.cell((col, row)).map_or("", |cell| cell.symbol());
                    line.push_str(symbol);
                    col += u16::try_from(unicode_width::UnicodeWidthStr::width(symbol).max(1))
                        .unwrap_or(1);
                }
                line
            })
            .collect()
    }

    #[test]
    fn render_draws_the_grid_into_the_pane() {
        let grid = grid(10, 2, &["hello", "world"]);
        let screen = draw_into(14, 5, Rect::new(2, 1, 10, 2), &grid, true);

        assert_eq!(screen[1], "  hello       ");
        assert_eq!(screen[2], "  world       ");
        // Nothing was drawn outside the pane.
        assert_eq!(screen[0], "              ");
    }

    #[test]
    fn render_keeps_wide_chars_on_their_columns() {
        let mut grid = Grid::new(6, 1);
        // A double-width character sits in its first cell; the next one is the
        // protocol's empty continuation marker. Drawing a space for it would
        // push everything after it one column right.
        grid.cells[0].text = "漢".to_string();
        grid.cells[1].text = String::new();
        grid.cells[2].text = "字".to_string();
        grid.cells[3].text = String::new();
        grid.cells[4].text = "!".to_string();

        let screen = draw_into(6, 1, Rect::new(0, 0, 6, 1), &grid, true);

        assert_eq!(screen[0], "漢字! ");
    }

    #[test]
    fn render_paints_highlight_colours() {
        let mut grid = Grid::new(2, 1);
        grid.cells[0].text = "a".to_string();
        grid.cells[0].hl = 7;
        grid.hl.insert(
            7,
            HlAttr {
                fg: Some(0x00FF_0000),
                bg: Some(0x0000_00FF),
                bold: true,
                ..HlAttr::default()
            },
        );

        let mut terminal = Terminal::new(TestBackend::new(2, 1)).expect("terminal");
        terminal
            .draw(|frame| draw(frame, Rect::new(0, 0, 2, 1), &grid, &palette(), true))
            .expect("draw");
        let cell = terminal.backend().buffer().cell((0, 0)).cloned();
        let cell = cell.expect("cell");
        assert_eq!(cell.fg, Color::Rgb(0xFF, 0x00, 0x00));
        assert_eq!(cell.bg, Color::Rgb(0x00, 0x00, 0xFF));
        assert!(cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn render_clips_a_wide_glyph_at_the_pane_edge() {
        // A resize on its way to nvim leaves the pane narrower than its grid
        // for a frame (the `draw` contract supports exactly this). The last
        // pane column can then hold the first half of a double-width glyph —
        // and ratatui writes such a glyph into the cell *after* it too, which
        // would erase whatever sits outside the pane.
        let mut grid = Grid::new(4, 1);
        grid.cells[0].text = "a".to_string();
        grid.cells[1].text = "漢".to_string();
        grid.cells[2].text = String::new();
        grid.cells[3].text = "b".to_string();

        let mut terminal = Terminal::new(TestBackend::new(6, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                for col in 0..6 {
                    frame.buffer_mut()[(col, 0)].set_symbol("X");
                }
                draw(frame, Rect::new(1, 0, 2, 1), &grid, &palette(), false);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let symbols: Vec<&str> = (0..6)
            .map(|col| buffer.cell((col, 0)).expect("cell").symbol())
            .collect();
        assert_eq!(
            symbols,
            ["X", "a", " ", "X", "X", "X"],
            "a clipped wide glyph must become a placeholder inside the pane, \\
             never a two-cell write into the column beyond it"
        );

        // When both halves fit, the glyph still renders in full up to the
        // pane's edge, and the exterior stays untouched.
        let mut fitting = Grid::new(4, 1);
        fitting.cells[0].text = "a".to_string();
        fitting.cells[1].text = "b".to_string();
        fitting.cells[2].text = "漢".to_string();
        fitting.cells[3].text = String::new();

        let mut terminal = Terminal::new(TestBackend::new(6, 1)).expect("terminal");
        terminal
            .draw(|frame| {
                for col in 0..6 {
                    frame.buffer_mut()[(col, 0)].set_symbol("X");
                }
                draw(frame, Rect::new(1, 0, 4, 1), &fitting, &palette(), false);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let symbols: Vec<&str> = (0..6)
            .map(|col| buffer.cell((col, 0)).expect("cell").symbol())
            .collect();
        assert_eq!(
            symbols,
            ["X", "a", "b", "漢", " ", "X"],
            "a wide glyph whose continuation fits must render whole"
        );
    }

    #[test]
    fn render_cursor_follows_the_pane_and_hides_while_busy() {
        let mut grid = Grid::new(10, 3);
        grid.cursor = (1, 2);
        let area = Rect::new(5, 4, 10, 3);

        assert_eq!(cursor_at(area, &grid), Some((6, 6)));

        // nvim draws nothing while it is busy, so neither does the caret.
        grid.busy = true;
        assert_eq!(cursor_at(area, &grid), None);
        grid.busy = false;

        // A cursor outside the pane belongs to no pane.
        grid.cursor = (10, 0);
        assert_eq!(cursor_at(area, &grid), None);
    }
}
