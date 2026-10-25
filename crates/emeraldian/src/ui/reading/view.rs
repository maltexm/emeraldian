//! Reading cache, viewport, selection and picture painting.

use super::*;

/// The note last laid out for reading, kept until something it was laid out
/// from changes.
///
/// Every frame used to read the note from disk, parse it and lay all of it out,
/// to show one screen of it. On a long note that was most of the work of each
/// keypress, and most of the memory the app touched: paging through a note of a
/// few megabytes took it past half a gigabyte. A frame that changes nothing the
/// layout depends on now reuses it.
#[derive(Default)]
pub struct ReadingCache {
    key: Option<ReadingKey>,
    pub(super) lines: Vec<Line<'static>>,
    anchors: Vec<(usize, usize)>,
    pub(super) pictures: Vec<Picture>,
    widest: usize,
}

/// Everything a reading layout depends on.
#[derive(PartialEq)]
struct ReadingKey {
    note: usize,
    /// When the file was last written, and its length: a cheap stand-in for
    /// its content that needs no reading.
    modified: Option<std::time::SystemTime>,
    len: u64,
    /// Which links resolve, and which attachments exist.
    index: u64,
    /// What each picture measures.
    images: u64,
    width: usize,
    reading_width: u16,
    height: u16,
    palette: Palette,
}

impl ReadingCache {
    /// Forgets the layout, for when something outside its key has changed.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

pub(in crate::ui) fn draw_reading(
    frame: &mut Frame,
    app: &mut App,
    palette: &Palette,
    area: Rect,
    gutter: u16,
    regions: &mut Regions,
) {
    let Some(id) = app.active_note() else { return };
    // Taken whether or not this frame can use it, so it can't go off later on
    // some other note.
    let reveal = app.reveal.take();
    let Some(path) = app.index.note(id).map(|note| note.meta.path.clone()) else {
        return;
    };
    let unreadable = |frame: &mut Frame| {
        Paragraph::new("could not read this note").render(area, frame.buffer_mut());
    };
    // The scrollbar stays on the pane's edge; the prose sits inside the gutter.
    let full = area;
    let area = prose_area(area, gutter);

    // An Excalidraw note is a wrapper around a scene: its Markdown is a warning
    // banner and a wall of compressed base64, and the drawing is the content.
    // Judged by the file name, so an ordinary note that happens to have a
    // `## Drawing` heading is still read as prose.
    let is_drawing = app
        .index
        .note(id)
        .is_some_and(|note| excalidraw::is_drawing(&note.meta.rel));
    if is_drawing {
        let Ok(content) = app.index.read(id) else {
            return unreadable(frame);
        };
        let scroll = app.active().map_or(0, |tab| tab.scroll);
        if let Some(scene) = app.scenes.get(&content) {
            let rows = drawing::draw(frame, palette, area, scene, scroll);
            let max_scroll = rows.saturating_sub(area.height as usize);
            if let Some(tab) = app.active_mut() {
                tab.scroll = tab.scroll.min(max_scroll);
            }
            return;
        }
    }
    let file = std::fs::metadata(&path).ok();

    let width = area.width.saturating_sub(1) as usize;
    // Pictures are capped at a share of the pane, so the layout pass has to
    // know how tall it is before it measures the first one.
    app.images.set_pane_height(area.height);

    let key = ReadingKey {
        note: id,
        modified: file.as_ref().and_then(|file| file.modified().ok()),
        len: file.as_ref().map_or(0, std::fs::Metadata::len),
        index: app.index.revision(),
        images: app.images.generation(),
        width,
        reading_width: app.config.ui.reading_width,
        height: area.height,
        palette: palette.clone(),
    };
    // A file that can't be looked at can't be trusted to be unchanged either.
    if file.is_none() || app.reading.key.as_ref() != Some(&key) {
        let Ok(content) = app.index.read(id) else {
            app.reading.clear();
            return unreadable(frame);
        };
        // A Mermaid file is one diagram: read as the code block it is, which
        // draws it, or says why it can't and shows the source.
        let is_mermaid = app
            .index
            .note(id)
            .is_some_and(|note| is_mermaid_file(&note.meta.rel));
        let document = if is_mermaid {
            mermaid_document(&content)
        } else {
            markdown::parse(&content)
        };
        let note_meta = app.index.note(id).map(|note| note.meta.rel.clone());
        let note_dir = note_meta
            .as_deref()
            .map(|rel| app.index.vault.path.join(rel))
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let mut ctx = Ctx {
            palette,
            index: &app.index,
            note_dir,
            images: Some(&mut app.images),
            pictures: Vec::new(),
            anchors: Vec::new(),
            embeds: note_meta.into_iter().collect(),
            reading_width: app.config.ui.reading_width as usize,
            breakout: 0,
        };
        let lines = render_document(&document, &mut ctx, width);
        // Content can be wider than the pane — a table laid out at its
        // natural width, a long code line — and panning is the only way to
        // reach the rest.
        let widest = lines.iter().map(Line::width).max().unwrap_or(0);
        app.reading = ReadingCache {
            key: Some(key),
            lines,
            anchors: std::mem::take(&mut ctx.anchors),
            pictures: std::mem::take(&mut ctx.pictures),
            widest,
        };
    }
    regions.anchors.clone_from(&app.reading.anchors);
    regions.reading = Some(area);

    let total = app.reading.lines.len();
    let height = area.height as usize;
    if let Some(reveal) = reveal {
        let start = anchor_row(&regions.anchors, reveal.line);
        let row = row_showing(
            &app.reading.lines,
            &regions.anchors,
            start,
            &reveal.text,
            reveal.nth,
        )
        .unwrap_or(start);
        if let Some(tab) = app.active_mut() {
            // In the upper part of the pane, with a little of what leads up to
            // it above.
            tab.scroll = row.saturating_sub(height / 3);
        }
    }
    let max_scroll = total.saturating_sub(height);
    let max_hscroll = app.reading.widest.saturating_sub(area.width as usize);
    if let Some(tab) = app.active_mut() {
        tab.scroll = tab.scroll.min(max_scroll);
        tab.hscroll = tab.hscroll.min(max_hscroll);
    }
    let (scroll, hscroll) = app.active().map_or((0, 0), |t| (t.scroll, t.hscroll));

    let visible: Vec<Line> = app
        .reading
        .lines
        .iter()
        .skip(scroll)
        .take(height)
        .cloned()
        .collect();
    let mut visible = visible;
    // `]`/`[` pick one link. Mark that row so Enter's target is visible. The
    // scroll that brings it on screen happens in the key handler, not here:
    // this runs every frame, and moving the scroll here would snap the view
    // back the moment `j`/`k` left the selection.
    if let Some(row) = selected_link_row(app, id)
        && row >= scroll
        && row < scroll + height
        && let Some(line) = visible.get_mut(row - scroll)
    {
        mark_selected_link(line, palette);
    }
    Paragraph::new(visible)
        .scroll((0, u16::try_from(hscroll).unwrap_or(u16::MAX)))
        .render(area, frame.buffer_mut());
    draw_pictures(
        frame,
        &mut app.images,
        palette,
        area,
        &app.reading.pictures,
        scroll,
        hscroll,
    );
    scrollbar(frame, palette, full, scroll, total);
}

/// Link under a reading-mode mouse point.
///
/// The cached rows hold exactly what was drawn, margins included, so a point is
/// resolved against the cell ranges each link occupies there: the very
/// occurrence its `]`/`[` position names, on every row it wraps over. Nothing
/// outside those cells — the centring margin, the selection star, another
/// link's identical label — can act as the link.
pub fn link_at(app: &App, point: (u16, u16)) -> Option<usize> {
    let area = app.regions.reading?;
    if !area.contains(ratatui::layout::Position::new(point.0, point.1)) {
        return None;
    }
    let tab = app.active()?;
    let row = tab.scroll + usize::from(point.1 - area.y);
    let col = tab.hscroll + usize::from(point.0 - area.x);
    let note = app.index.note(tab.note)?;
    let lines = &app.reading.lines;
    let anchors = &app.reading.anchors;
    if row >= lines.len() {
        return None;
    }
    // Each block is flattened once, however many links it holds.
    let mut blocks: Vec<(usize, Vec<TextCell>)> = Vec::new();
    for idx in 0..note.links.len() {
        let reveal = Reveal::for_link(note, idx)?;
        let start = anchor_row(anchors, reveal.line);
        if row < start || row >= block_end(lines.len(), anchors, start) {
            continue;
        }
        let at = match blocks.iter().position(|(first, _)| *first == start) {
            Some(at) => at,
            None => {
                blocks.push((start, block_cells(lines, anchors, start)));
                blocks.len() - 1
            }
        };
        let nth = nth_in_block(note, idx, anchors);
        let hit = link_ranges(&blocks[at].1, &reveal.text, nth)
            .into_iter()
            .any(|(r, from, to)| r == row && col >= from && col < to);
        if hit {
            return Some(idx);
        }
    }
    None
}

/// How many links before the `idx`-th one show the same label in the same
/// block. This is the occurrence of the label that the `idx`-th link is: a
/// paragraph can run over several source lines, and the label is looked for in
/// the whole of it.
fn nth_in_block(
    note: &emeraldian_core::index::IndexedNote,
    idx: usize,
    anchors: &[(usize, usize)],
) -> usize {
    let block = |idx: usize| anchor_row(anchors, note.body_offset + note.links[idx].raw.line);
    let label = note.links[idx].display().to_lowercase();
    (0..idx)
        .filter(|&earlier| {
            block(earlier) == block(idx) && note.links[earlier].display().to_lowercase() == label
        })
        .count()
}

/// One character of a block's text, with the cells it occupies on its row.
struct TextCell {
    /// Lowercased, so that labels match as `row_showing` matches them. A
    /// character that lowercases to several is several cells sharing a place.
    ch: char,
    /// `None` for the space standing in for a line break.
    row: Option<usize>,
    from: usize,
    to: usize,
}

/// The row after the last of the block starting at `start`.
fn block_end(total: usize, anchors: &[(usize, usize)], start: usize) -> usize {
    anchors
        .iter()
        .map(|(_, row)| *row)
        .find(|row| *row > start)
        .unwrap_or(total)
        .min(total)
}

/// The text of the block starting at `start`, as the cells it was drawn in.
/// Rows are joined the way the wrap split them: the blanks around a break
/// make one space.
fn block_cells(lines: &[Line], anchors: &[(usize, usize)], start: usize) -> Vec<TextCell> {
    let end = block_end(lines.len(), anchors, start);
    let mut out = Vec::new();
    for (row, line) in lines.iter().enumerate().take(end).skip(start) {
        let mut cells: Vec<(char, usize, usize)> = Vec::new();
        let mut col = 0;
        for ch in line.spans.iter().flat_map(|span| span.content.chars()) {
            let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            cells.push((ch, col, col + width));
            col += width;
        }
        if row > start {
            let blank = cells
                .iter()
                .take_while(|(ch, ..)| ch.is_whitespace())
                .count();
            cells.drain(..blank);
        }
        if row + 1 < end {
            while cells.last().is_some_and(|(ch, ..)| ch.is_whitespace()) {
                cells.pop();
            }
        }
        for (ch, from, to) in cells {
            for lower in ch.to_lowercase() {
                out.push(TextCell {
                    ch: lower,
                    row: Some(row),
                    from,
                    to,
                });
            }
        }
        if row + 1 < end {
            out.push(TextCell {
                ch: ' ',
                row: None,
                from: 0,
                to: 0,
            });
        }
    }
    out
}

/// The cells the `nth` occurrence of `text` takes in a block, one `(row, from,
/// to)` run per row it wraps over. Short of `nth`, the last occurrence.
fn link_ranges(cells: &[TextCell], text: &str, nth: usize) -> Vec<(usize, usize, usize)> {
    let needle: Vec<char> = text.to_lowercase().chars().collect();
    if needle.is_empty() || text.trim().is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut at = 0;
    while at + needle.len() <= cells.len() {
        if cells[at..at + needle.len()]
            .iter()
            .map(|c| c.ch)
            .eq(needle.iter().copied())
        {
            found.push(at);
            at += needle.len();
        } else {
            at += 1;
        }
    }
    let Some(&first) = found.get(nth).or(found.last()) else {
        return Vec::new();
    };
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
    for cell in &cells[first..first + needle.len()] {
        let Some(row) = cell.row else { continue };
        match ranges.last_mut() {
            Some((last, _, to)) if *last == row => *to = (*to).max(cell.to),
            _ => ranges.push((row, cell.from, cell.to)),
        }
    }
    ranges
}

/// Draws the pictures whose rows are on screen, over the blanks left for them.
fn draw_pictures(
    frame: &mut Frame,
    images: &mut Images,
    palette: &Palette,
    area: Rect,
    pictures: &[Picture],
    scroll: usize,
    hscroll: usize,
) {
    for picture in pictures {
        // Where the top of the picture sits relative to the pane, which is
        // above it — a negative row — once it is partly scrolled off.
        let Ok(top) = i32::try_from(picture.line).map(|line| line - scroll as i32) else {
            continue;
        };
        if top >= i32::from(area.height) || top + i32::from(picture.size.height) <= 0 {
            continue;
        }
        let Ok(top) = i16::try_from(top) else {
            continue;
        };
        // Pictures pan with the text around them, but only while they stay
        // fully inside the pane: the graphics protocols slice by row, not by
        // column, so a picture that has run off the left edge would be redrawn
        // squashed against it rather than clipped. Dropping it is the honest
        // answer, and panning is for reaching a wide table anyway — a picture
        // pinned over that table is exactly what's in the way.
        let left = i32::from(picture.indent) - hscroll as i32;
        if left < 0 || left >= i32::from(area.width) {
            continue;
        }
        let Ok(left) = i16::try_from(left) else {
            continue;
        };

        let source = match &picture.source {
            PictureSource::File(path) => path,
            PictureSource::Drawing(scene) => {
                // The rows are blank; the braille is traced over them, clipped to
                // the part of the drawing that is on screen. Scaled to its own
                // width, so it has to fit the pane whole or stay out of it.
                let hidden = u16::try_from((-i32::from(top)).max(0)).unwrap_or(0);
                let left = u16::try_from(left).unwrap_or(0);
                let first = u16::try_from(i32::from(top).max(0)).unwrap_or(0);
                if left + picture.size.width > area.width {
                    continue;
                }
                let rect = Rect {
                    x: area.x + left,
                    y: area.y + first,
                    width: picture.size.width,
                    height: (picture.size.height - hidden).min(area.height - first),
                };
                drawing::paint(frame, palette, rect, scene, usize::from(hidden));
                continue;
            }
        };
        match images.get(source, picture.size) {
            Some(protocol) => {
                let position = SignedPosition::from((left, top));
                frame.render_widget(SlicedImage::new(protocol, position), area);
            }
            // Still encoding. The rows are already the right size, so the
            // picture appears without moving anything around it.
            None => {
                let row = area.y as i32 + i32::from(top).max(0);
                let column = area.x as i32 + i32::from(left).max(0);
                if let (Ok(y), Ok(x)) = (u16::try_from(row), u16::try_from(column)) {
                    let placeholder = Rect {
                        x: x.min(area.x + area.width),
                        y,
                        width: picture.size.width.min(area.width),
                        height: 1,
                    };
                    Paragraph::new(Line::from(Span::styled(
                        format!("{} …", icons::IMAGE),
                        Style::default().fg(palette.text_faint),
                    )))
                    .render(placeholder.intersection(area), frame.buffer_mut());
                }
            }
        }
    }
}

/// The rendered row of the link `]`/`[` picked, if one is picked and still
/// exists. File line is body-relative plus the frontmatter offset, matching
/// the anchors `render_document` records.
fn selected_link_row(app: &App, id: usize) -> Option<usize> {
    let idx = app.active()?.selected_link?;
    let reveal = Reveal::for_link(app.index.note(id)?, idx)?;
    let start = anchor_row(&app.reading.anchors, reveal.line);
    Some(
        row_showing(
            &app.reading.lines,
            &app.reading.anchors,
            start,
            &reveal.text,
            reveal.nth,
        )
        .unwrap_or(start),
    )
}

/// Paints the picked link's row with the theme accent and a star, so the
/// selection is visible without reading colors.
///
/// The row keeps every cell where it was: mouse hits are resolved against the
/// cached, unmarked rows, so a mark that moved the text would put the pointer
/// off the link it points at. The star therefore takes the last two blanks of
/// the row's leading margin, beside the text. A row with no such margin (full
/// width reading, a table, a panel) gets the accent alone.
pub(super) fn mark_selected_link(line: &mut Line<'_>, palette: &Palette) {
    let mark = Style::default()
        .fg(palette.text_on_accent)
        .bg(palette.accent);
    let margin = line
        .spans
        .first()
        .filter(|span| span.content.len() >= 2 && span.content.chars().all(|c| c == ' '))
        .map(|span| span.content.len());
    let mut skip = 0;
    if let Some(margin) = margin {
        let first = &mut line.spans[0];
        first.content = " ".repeat(margin - 2).into();
        line.spans.insert(1, Span::styled("★ ", mark));
        skip = 2;
    }
    for span in line.spans.iter_mut().skip(skip) {
        span.style = span.style.bg(palette.accent).fg(palette.text_on_accent);
    }
}

/// The row, from `start` to the end of its block, showing the `nth` time
/// `text` occurs there, counting from 0 and ignoring case. A row and the one
/// after it are read together, so text wrapped onto the next row is found as
/// well. Short of `nth`, the last one found.
fn row_showing(
    lines: &[Line],
    anchors: &[(usize, usize)],
    start: usize,
    text: &str,
    nth: usize,
) -> Option<usize> {
    let needle = text.to_lowercase();
    if needle.trim().is_empty() {
        return None;
    }
    let end = anchors
        .iter()
        .map(|(_, row)| *row)
        .find(|row| *row > start)
        .unwrap_or(lines.len())
        .min(lines.len());
    let row_text = |row: usize| -> String {
        lines.get(row).map_or_else(String::new, |line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .to_lowercase()
        })
    };
    let count = |text: &str| text.matches(needle.as_str()).count();
    let mut seen = 0;
    let mut last = None;
    for row in start..end {
        let here = row_text(row);
        let next = row_text(row + 1);
        let joined = format!("{} {}", here.trim_end(), next.trim_start());
        // What this row shows, and what only shows across its end.
        let found = count(&here) + count(&joined).saturating_sub(count(&here) + count(&next));
        if found > 0 {
            last = Some(row);
            if seen + found > nth {
                return last;
            }
            seen += found;
        }
    }
    last
}
