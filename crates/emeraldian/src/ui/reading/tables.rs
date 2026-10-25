//! Natural-width table layout.

use super::inline::shown_text;
use super::*;
use crate::ui::truncate;

/// Widest a single table column may be drawn, in characters.
///
/// Tables are laid out at their content's width and panned across, so this is
/// only here to stop one cell of prose from pushing every column after it out
/// of reach.
const MAX_COLUMN: usize = 40;

pub(super) fn render_table(
    table: &Table,
    palette: &Palette,
    pad: &str,
    out: &mut Vec<Line<'static>>,
) {
    let columns = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return;
    }

    // Size columns to their content.
    //
    // A table wider than the pane is left wide and scrolled across
    // horizontally, rather than squeezed to fit: dividing the pane between
    // eight columns leaves three characters each, which is not a narrow table
    // but an unreadable one. The only limit is a ceiling on a single runaway
    // column, so one essay-length cell can't push the rest off the far side.
    let mut widths = vec![0usize; columns];
    let cell_text = |cells: &Vec<Vec<markdown::Span>>, i: usize| -> String {
        cells
            .get(i)
            .map(|spans| shown_text(spans))
            .unwrap_or_default()
    };

    // Measured in columns rather than characters: typeset maths puts its
    // accents on as combining marks, which take no room of their own.
    for (i, width) in widths.iter_mut().enumerate() {
        *width = (*width).max(cell_text(&table.header, i).width());
        for row in &table.rows {
            *width = (*width).max(cell_text(row, i).width());
        }
        *width = (*width).clamp(1, MAX_COLUMN);
    }

    let border = Style::default().fg(palette.table_border);
    let rule = |left: &str, mid: &str, right: &str| -> Line<'static> {
        let mut text = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            text.push_str(&"─".repeat(w + 2));
            text.push_str(if i + 1 == widths.len() { right } else { mid });
        }
        Line::from(vec![Span::raw(pad.to_string()), Span::styled(text, border)])
    };

    let render_row = |cells: &Vec<Vec<markdown::Span>>, header: bool| -> Line<'static> {
        let mut spans = vec![Span::raw(pad.to_string()), Span::styled("│", border)];
        for (i, w) in widths.iter().enumerate() {
            let text = truncate(&cell_text(cells, i), *w);
            let padding = w.saturating_sub(text.width());
            let (before, after) = match table.aligns.get(i).copied().unwrap_or(Align::Left) {
                Align::Left => (0, padding),
                Align::Right => (padding, 0),
                Align::Center => (padding / 2, padding - padding / 2),
            };
            let style = if header {
                Style::default()
                    .fg(palette.table_header)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text_normal)
            };
            spans.push(Span::raw(format!(" {}", " ".repeat(before))));
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(format!("{} ", " ".repeat(after))));
            spans.push(Span::styled("│", border));
        }
        Line::from(spans)
    };

    out.push(rule("┌", "┬", "┐"));
    out.push(render_row(&table.header, true));
    out.push(rule("├", "┼", "┤"));
    for row in &table.rows {
        out.push(render_row(row, false));
    }
    out.push(rule("└", "┴", "┘"));
}
