//! Prose measure and wide-block placement in the shared reading pane.
//!
//! Prose wraps at the measure and sits in the middle of the pane. Content that
//! cannot wrap (tables, code, drawings, pictures) is laid out at the pane's
//! width and then placed by what it needs: inside the column, lined up with
//! the prose, when it fits there; centred on the pane when it needs more; at
//! the pane's edge, to be panned, when it needs more than the pane has.
//!
//! Layout marks the rows it hands over with an alignment, which this module
//! consumes: `Left` rows are content to place, `Center` rows are already
//! centred on the pane (display maths), and unmarked rows are prose.

use ratatui::layout::Alignment;

use super::*;

/// Lay out one block, then place its rows without changing row anchors.
pub(super) fn render_column_block(
    block: &Block,
    ctx: &mut Ctx,
    pane: usize,
    measure: usize,
    out: &mut Vec<Line<'static>>,
) {
    let wide = matches!(
        block.kind,
        BlockKind::Code { .. } | BlockKind::Math(_) | BlockKind::Table(_)
    );
    let width = if measure == 0 || wide {
        pane
    } else {
        measure.min(pane)
    };
    // Wide blocks are laid out at the pane's width, but the column is still
    // what they are measured against.
    let column = if measure == 0 {
        pane
    } else {
        measure.min(pane)
    };
    let offset = (pane - column).div_ceil(2);
    let start = out.len();
    let pictures = ctx.pictures.len();
    let previous = ctx.breakout;
    ctx.breakout = pane - width;
    render_block(block, ctx, width, 0, out);
    ctx.breakout = previous;
    if wide {
        // Laid out at the pane's width; `place` decides where it goes.
        let mark = if matches!(block.kind, BlockKind::Math(_)) {
            Alignment::Center
        } else {
            Alignment::Left
        };
        for row in &mut out[start..] {
            row.alignment = Some(mark);
        }
    }
    place(
        &mut out[start..],
        start,
        &mut ctx.pictures[pictures..],
        Placement {
            column,
            pane,
            offset,
        },
    );
}

#[derive(Clone, Copy)]
struct Placement {
    /// Width of the prose column.
    column: usize,
    /// Width of the whole pane.
    pane: usize,
    /// Columns in front of the prose column.
    offset: usize,
}

/// Indents prose rows and places each run of wide-content rows.
fn place(rows: &mut [Line<'static>], first: usize, pictures: &mut [Picture], at: Placement) {
    let marks: Vec<Option<Alignment>> = rows.iter_mut().map(|row| row.alignment.take()).collect();
    let mut row = 0;
    while row < rows.len() {
        match marks[row] {
            Some(Alignment::Center) => row += 1,
            Some(Alignment::Left) => {
                let mut end = row;
                while end < rows.len() && marks[end] == Some(Alignment::Left) {
                    end += 1;
                }
                place_run(&mut rows[row..end], first + row, pictures, at);
                row = end;
            }
            _ => {
                indent(&mut rows[row], at.offset);
                row += 1;
            }
        }
    }
}

/// Places consecutive wide-content rows as one piece, so that a drawing keeps
/// its shape.
fn place_run(rows: &mut [Line<'static>], first: usize, pictures: &mut [Picture], at: Placement) {
    let last = first + rows.len();
    let in_run = |picture: &Picture| (first..last).contains(&picture.line);
    let needed = rows
        .iter()
        .map(content_width)
        .chain(
            pictures
                .iter()
                .filter(|picture| in_run(picture))
                .map(|picture| usize::from(picture.indent) + usize::from(picture.size.width)),
        )
        .max()
        .unwrap_or(0);

    let (panel, shift) = if needed <= at.column {
        (at.column, at.offset)
    } else {
        let width = needed.min(at.pane);
        (width, (at.pane - width) / 2)
    };

    for row in rows {
        // A filled panel (code) is as wide as the piece, not as wide as the
        // pane it was laid out in.
        if let Some(fill) = trailing_fill(row) {
            let used = content_width(row);
            row.spans.pop();
            if used < panel {
                row.spans.push(Span::styled(" ".repeat(panel - used), fill));
            }
        }
        indent(row, shift);
    }
    for picture in pictures.iter_mut().filter(|picture| in_run(picture)) {
        picture.indent += u16::try_from(shift).unwrap_or(0);
    }
}

fn indent(row: &mut Line<'static>, columns: usize) {
    if columns > 0 {
        row.spans.insert(0, Span::raw(" ".repeat(columns)));
    }
}

/// The style of a row's closing padding, if its last span is only that: a
/// styled run of spaces, which is how a filled panel reaches its edge.
fn trailing_fill(row: &Line<'static>) -> Option<Style> {
    let last = row.spans.last()?;
    (row.spans.len() > 1 && last.style != Style::default() && last.content.trim().is_empty())
        .then_some(last.style)
}

/// A row's width without the padding that fills a panel.
fn content_width(row: &Line<'static>) -> usize {
    match trailing_fill(row) {
        Some(_) => row.width() - row.spans.last().map_or(0, Span::width),
        None => row.width(),
    }
}
