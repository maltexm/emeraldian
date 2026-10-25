//! The note pane: tab bar, reading mode and the editor.
//!
//! Reading mode renders the markdown model into styled lines the way Obsidian's
//! preview does — headings colored by level, wikilinks in the accent color and
//! unresolved ones dimmed, callouts with a tinted bar, code blocks on their own
//! background. Editing mode shows the source with a line-number gutter.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use emeraldian_core::markdown;
use emeraldian_theme::Palette;

use crate::app::{App, Mode, Regions};
use crate::editor::{Cursor, Row};
use crate::ui::{icons, scrollbar, truncate};

/// Left padding inside the note pane, matching Obsidian's generous margins.
const PADDING: u16 = 2;

/// Columns reserved for the line-number gutter when it is switched on.
///
/// Fixed rather than sized to the note, and reserved while *reading* too, where
/// it is left blank. Both modes then wrap the same prose to the same width in
/// the same column, which is what makes `Ctrl+E` restyle the page instead of
/// reflowing it. A note past 999 lines spends the separating space on a fourth
/// digit rather than moving the text.
const GUTTER: u16 = 4;

use super::reading::draw_reading;
pub use super::reading::{ReadingCache, anchor_row};

pub fn draw(
    frame: &mut Frame,
    app: &mut App,
    palette: &Palette,
    area: Rect,
    regions: &mut Regions,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    draw_tab_bar(frame, app, palette, rows[0], regions);

    let body = Rect {
        x: rows[1].x + PADDING,
        y: rows[1].y,
        width: rows[1].width.saturating_sub(PADDING * 2),
        height: rows[1].height,
    };

    frame.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(palette.bg_primary)),
        rows[1],
    );

    if app.active().is_none() {
        draw_empty_state(frame, palette, body);
        return;
    }

    // Reserved in both modes so the prose column never moves between them.
    let gutter = if app.config.ui.line_numbers {
        GUTTER.min(body.width)
    } else {
        0
    };

    match app.active().map(|t| t.mode) {
        Some(Mode::Reading) => draw_reading(frame, app, palette, body, gutter, regions),
        Some(Mode::Editing) => draw_editing(frame, app, palette, body, gutter, regions),
        None => {}
    }
}

/// The prose column inside the note pane: the gutter and the scrollbar's column
/// taken off.
pub(super) fn prose_area(area: Rect, gutter: u16) -> Rect {
    Rect {
        x: area.x + gutter,
        y: area.y,
        width: area.width.saturating_sub(gutter),
        height: area.height,
    }
}

/// Width and height the editor's text was last drawn at.
///
/// Key handling needs it to move between wrapped rows, and reads it from the
/// last frame rather than recomputing the layout arithmetic a second time.
#[must_use]
pub fn edit_viewport(app: &App) -> (usize, usize) {
    app.regions.editor.map_or((0, 0), |(rect, _)| {
        (rect.width as usize, rect.height as usize)
    })
}

fn draw_tab_bar(
    frame: &mut Frame,
    app: &App,
    palette: &Palette,
    area: Rect,
    regions: &mut Regions,
) {
    frame.render_widget(
        ratatui::widgets::Block::default().style(Style::default().bg(palette.tab_bar_bg)),
        area,
    );
    if app.tabs.is_empty() {
        return;
    }

    // The side panes light their borders up when they have the keyboard. The
    // note pane has no border, so its active tab carries the same colour.
    let focused = app.focus == crate::app::Focus::Note;
    let mut spans = Vec::new();
    let mut x = area.x;
    for (index, tab) in app.tabs.iter().enumerate() {
        let active = app.active_tab == Some(index);
        let title = app.note_title(tab.note);
        let title = truncate(&title, 22);

        // `" {title}"` plus the two-column modified marker.
        let width = title.chars().count() as u16 + 3;
        regions.tabs.push((
            Rect {
                x,
                y: area.y,
                width,
                height: 1,
            },
            index,
        ));
        x += width;

        let style = if active {
            Style::default()
                .bg(palette.tab_active_bg)
                .fg(palette.tab_active_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(palette.tab_bar_bg)
                .fg(palette.tab_inactive_fg)
        };

        if active && focused {
            spans.push(Span::styled("▎", style.fg(palette.border_focus)));
            spans.push(Span::styled(title.clone(), style.fg(palette.text_accent)));
        } else {
            spans.push(Span::styled(format!(" {title}"), style));
        }
        // Obsidian marks an unsaved tab with a dot where the close button goes.
        let modified = app.tab_marked_modified(index);
        spans.push(Span::styled(
            if modified {
                format!(" {} ", icons::MODIFIED)
            } else {
                "  ".to_string()
            },
            style.fg(if modified {
                palette.tab_modified
            } else {
                palette.tab_inactive_fg
            }),
        ));
    }

    Paragraph::new(Line::from(spans)).render(area, frame.buffer_mut());
}

fn draw_empty_state(frame: &mut Frame, palette: &Palette, area: Rect) {
    let hints = [
        ("Ctrl+O", "open a note"),
        ("Ctrl+P", "command palette"),
        ("Ctrl+N", "new note"),
        ("Ctrl+G", "graph view"),
        ("Ctrl+L", "assistant"),
        ("?", "all shortcuts"),
    ];

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "emeraldian",
            Style::default()
                .fg(palette.text_accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (key, description) in hints {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<10}"),
                Style::default().fg(palette.text_accent),
            ),
            Span::styled(description, Style::default().fg(palette.text_muted)),
        ]));
    }

    Paragraph::new(lines).render(area, frame.buffer_mut());
}

// ---------------------------------------------------------------------------
// Reading mode
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Editing mode
// ---------------------------------------------------------------------------

/// Paints the embedded Neovim's grid into the note pane.
///
/// The area is recorded for the session to size its screen to. Until the
/// session exists, and in the one frame before its first redraw, the pane is
/// just the theme's background.
fn draw_nvim(frame: &mut Frame, app: &App, palette: &Palette, area: Rect, regions: &mut Regions) {
    regions.nvim = Some(area);
    let Some(session) = app.nvim.as_ref() else {
        return;
    };
    let focused = app.focus == crate::app::Focus::Note;
    crate::nvim::render::draw(frame, area, session.grid(), palette, focused);
}

fn draw_editing(
    frame: &mut Frame,
    app: &mut App,
    palette: &Palette,
    area: Rect,
    gutter: u16,
    regions: &mut Regions,
) {
    // The embedded Neovim draws its own line numbers, wrapping and scrollbar
    // into the whole pane, so none of the built-in editor's layout applies.
    if app.nvim_editing() {
        draw_nvim(frame, app, palette, area, regions);
        return;
    }

    let wrap = app.config.editor.wrap;
    // A caret in an unfocused pane claims input that would go somewhere else.
    let focused = app.focus == crate::app::Focus::Note;
    // Vim's Normal-mode cursor covers a character rather than sitting between
    // two, so on an empty or just-ended line the block would otherwise be drawn
    // one column past the text it is supposed to be on.
    let on_character = app.config.editor.vim && app.vim.mode.is_normal_like();
    // Highlighting only the match jumped to would hide the rest, which is most
    // of what a search is for. Read before the editor is borrowed below.
    let search = app
        .config
        .editor
        .vim
        .then(|| app.vim.search.as_ref().map(|s| s.pattern.clone()))
        .flatten();
    let prose = prose_area(area, gutter);
    // One column is left for the scrollbar, which also gives the caret a place
    // to sit at the end of a full row.
    let text = Rect {
        width: prose.width.saturating_sub(1),
        ..prose
    };
    let height = text.height as usize;

    let Some(editor) = app.editor_mut() else {
        return;
    };
    if on_character {
        editor.clamp_normal();
    }
    let layout = editor.layout(text.width as usize, wrap);
    editor.scroll_into_view(&layout, height);

    let scroll = editor.scroll;
    let hscroll = editor.hscroll;
    let (caret_row, caret_column) = editor.caret(&layout);
    let selection = editor.selection();
    let cursor_line = editor.cursor().line;
    let fenced = fenced_lines(editor.lines());

    // Painting a whole source line at once and slicing it per row keeps one
    // decision — what each character is — in one place, however it wrapped.
    let mut painted: Option<(usize, Vec<(char, Style)>)> = None;
    let mut numbers: Vec<Line> = Vec::new();
    let mut body: Vec<Line> = Vec::new();

    for row in layout.rows().iter().skip(scroll).take(height) {
        let on_cursor_line = row.line == cursor_line;
        numbers.push(gutter_line(row, on_cursor_line, gutter, palette));

        if painted.as_ref().is_none_or(|(line, _)| *line != row.line) {
            let source = &editor.lines()[row.line];
            painted = Some((
                row.line,
                paint_line(source, on_cursor_line, fenced[row.line], palette),
            ));
        }
        let Some((_, chars)) = &painted else { continue };

        let hits = match &search {
            Some(pattern) => editor.matches_on(row.line, pattern),
            None => Vec::new(),
        };
        body.push(row_line(
            row,
            &chars[row.start.min(chars.len())..row.end.min(chars.len())],
            RowStyle {
                background: if fenced[row.line] == Fence::Code {
                    palette.code_bg
                } else if on_cursor_line {
                    palette.cursor_line_bg
                } else {
                    palette.bg_primary
                },
                selection: palette.bg_selection,
                search: palette.bg_selection,
            },
            selection,
            &hits,
            text.width as usize + hscroll,
        ));
    }

    if gutter > 0 {
        Paragraph::new(numbers).render(
            Rect {
                width: gutter,
                ..area
            },
            frame.buffer_mut(),
        );
    }
    Paragraph::new(body)
        .scroll((0, u16::try_from(hscroll).unwrap_or(u16::MAX)))
        .render(text, frame.buffer_mut());
    scrollbar(frame, palette, area, scroll, layout.rows().len());

    regions.editor = Some((text, scroll));

    // Place the terminal cursor so the user sees a real caret.
    let column = usize::from(caret_column);
    if focused && caret_row >= scroll && caret_row - scroll < height && column >= hscroll {
        let x = text.x + u16::try_from(column - hscroll).unwrap_or(u16::MAX);
        if x < prose.x + prose.width {
            frame.set_cursor_position((x, text.y + (caret_row - scroll) as u16));
        }
    }
}

/// Colours for the row a line is drawn on, behind whatever it says.
struct RowStyle {
    background: ratatui::style::Color,
    selection: ratatui::style::Color,
    /// Behind a search hit. Same colour as a selection, since both mean
    /// "this is the text you asked about".
    search: ratatui::style::Color,
}

/// The gutter cell for one row: a line number, or the wrap marker on a row that
/// is the continuation of the one above.
///
/// The marker lives here rather than in the text so that distinguishing a soft
/// wrap from a real line break costs no reading width at all.
fn gutter_line(row: &Row, on_cursor_line: bool, gutter: u16, palette: &Palette) -> Line<'static> {
    if gutter == 0 {
        return Line::from("");
    }
    let width = usize::from(gutter) - 1;
    let (text, color) = if row.first {
        (
            format!("{:>width$} ", row.line + 1),
            if on_cursor_line {
                palette.line_number_active
            } else {
                palette.line_number
            },
        )
    } else {
        (format!("{:>width$} ", icons::WRAP), palette.text_faint)
    };
    Line::from(Span::styled(text, Style::default().fg(color)))
}

/// Builds one terminal row from the painted characters it shows.
///
/// The row is padded to the full width so a code block's background and the
/// cursor line's highlight reach the edge of the pane rather than stopping at
/// the end of the text.
fn row_line(
    row: &Row,
    chars: &[(char, Style)],
    colors: RowStyle,
    selection: Option<(Cursor, Cursor)>,
    hits: &[(usize, usize)],
    width: usize,
) -> Line<'static> {
    let selected = |col: usize| {
        selection.is_some_and(|(start, end)| {
            let at = (row.line, col);
            at >= (start.line, start.col) && at < (end.line, end.col)
        })
    };
    let matched = |col: usize| hits.iter().any(|(start, end)| col >= *start && col < *end);
    // A background already on the character — inline code — outranks the row's,
    // a search hit outranks that, and a selection outranks everything: the
    // more deliberate the act, the louder it is drawn.
    let resolve = |style: Style, col: usize| {
        if selected(col) {
            return style.bg(colors.selection);
        }
        if matched(col) {
            return style.bg(colors.search);
        }
        match style.bg {
            Some(_) => style,
            None => style.bg(colors.background),
        }
    };

    let mut spans = vec![Span::styled(
        " ".repeat(usize::from(row.indent)),
        Style::default().bg(colors.background),
    )];
    let mut run = String::new();
    let mut current: Option<Style> = None;

    for (offset, (ch, style)) in chars.iter().enumerate() {
        let style = resolve(*style, row.start + offset);
        if current.is_some_and(|open| open != style) {
            spans.push(Span::styled(std::mem::take(&mut run), current.unwrap()));
        }
        current = Some(style);
        run.push(*ch);
    }
    if let Some(style) = current {
        spans.push(Span::styled(run, style));
    }

    // Trailing fill, including the selected newline at the end of a line the
    // selection runs through.
    let used: usize = spans.iter().map(Span::width).sum();
    let end = selected(row.end).then_some(colors.selection);
    if let Some(color) = end {
        spans.push(Span::styled(" ", Style::default().bg(color)));
    }
    let pad = width.saturating_sub(used + usize::from(end.is_some()));
    spans.push(Span::styled(
        " ".repeat(pad),
        Style::default().bg(colors.background),
    ));
    Line::from(spans)
}

/// What a source line belongs to, as far as painting it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fence {
    Prose,
    /// Inside a fenced code block, the fences included.
    Code,
    /// Inside a `$$` equation set on lines of its own, the delimiters included.
    Math,
}

/// Which lines sit inside a fenced code block or a `$$` equation, so their
/// content is never read as Markdown while it is being edited. TeX is full of
/// `_` and `*`, which would otherwise be dimmed and italicised as emphasis.
fn fenced_lines(lines: &[String]) -> Vec<Fence> {
    let is_fence = |line: &String| {
        let trimmed = line.trim_start();
        trimmed.starts_with("```") || trimmed.starts_with("~~~")
    };
    let mut fences = vec![Fence::Prose; lines.len()];
    let mut at = 0;
    while at < lines.len() {
        if is_fence(&lines[at]) {
            // The fence lines belong to the block they open and close, and an
            // unclosed one runs to the end.
            let end = lines[at + 1..]
                .iter()
                .position(is_fence)
                .map_or(lines.len() - 1, |offset| at + 1 + offset);
            fences[at..=end].fill(Fence::Code);
            at = end + 1;
            continue;
        }
        // The same test the reading pane uses, so the two agree on where an
        // equation starts and stops. One on a single line is left to
        // `scan_inline`, which already paints it.
        if lines[at].trim_start().starts_with("$$") {
            let rest: Vec<&str> = lines[at..].iter().map(String::as_str).collect();
            if let Some(math) = markdown::math_block(&rest)
                && math.lines > 1
            {
                fences[at..at + math.lines].fill(Fence::Math);
                at += math.lines;
                continue;
            }
        }
        at += 1;
    }
    fences
}

/// Styles one source line character by character, ready to be sliced across
/// however many rows it wrapped onto.
///
/// Markers are dimmed and given the glyph the reading pane would use, but only
/// where that glyph is exactly as wide as what it replaces — a `-` becomes a
/// `•`, an `x` inside `[x]` becomes a `☑`. Nothing is hidden and nothing moves,
/// so the caret is always where the character is, and `on_cursor_line` can show
/// the line exactly as typed without the text shifting underneath.
fn paint_line(
    text: &str,
    on_cursor_line: bool,
    fence: Fence,
    palette: &Palette,
) -> Vec<(char, Style)> {
    let chars: Vec<char> = text.chars().collect();
    match fence {
        Fence::Code => {
            let style = Style::default().fg(palette.code_fg).bg(palette.code_bg);
            return chars.into_iter().map(|ch| (ch, style)).collect();
        }
        Fence::Math => {
            // TeX in the maths colour, with its `$$` dimmed like any other
            // delimiter.
            let paint = |ink| {
                let paint = markdown::Paint {
                    style: markdown::Style::default(),
                    ink,
                };
                paint_style(paint, on_cursor_line, palette)
            };
            let (tex, marker) = (paint(markdown::Ink::Math), paint(markdown::Ink::Marker));
            let mut out: Vec<(char, Style)> = chars.iter().map(|&ch| (ch, tex)).collect();
            for at in 1..chars.len() {
                if chars[at - 1] == '$' && chars[at] == '$' {
                    out[at - 1].1 = marker;
                    out[at].1 = marker;
                }
            }
            return out;
        }
        Fence::Prose => {}
    }

    let mut out: Vec<(char, Style)> = markdown::scan_inline(text)
        .into_iter()
        .zip(&chars)
        .map(|(paint, ch)| (*ch, paint_style(paint, on_cursor_line, palette)))
        .collect();

    decorate_markers(&chars, on_cursor_line, palette, &mut out);
    out
}

/// Theme colors for one inline character of source.
fn paint_style(paint: markdown::Paint, on_cursor_line: bool, palette: &Palette) -> Style {
    use markdown::Ink;

    let mut style = Style::default().fg(match paint.ink {
        // On the cursor's own line the syntax is what you are editing, so it is
        // shown at full contrast rather than pushed into the background.
        Ink::Marker if on_cursor_line => palette.text_muted,
        Ink::Marker => palette.text_faint,
        // Whether a link resolves is the reading pane's business: it has the
        // link target, and this only has the characters.
        Ink::WikiLink => palette.link,
        Ink::Link => palette.link_external,
        Ink::Tag => palette.tag_fg,
        Ink::Math => palette.text_accent,
        Ink::Text => palette.text_normal,
    });
    if paint.ink == Ink::Tag {
        style = style.bg(palette.tag_bg);
    }
    emphasis(style, paint.style, palette)
}

/// Applies bold, italic, strikethrough, inline code and highlight colors.
///
/// Shared with the reading pane so a bold word can't end up a different color
/// depending on which mode you are looking at it in.
pub(super) fn emphasis(mut style: Style, md: markdown::Style, palette: &Palette) -> Style {
    if md.code {
        style = style.fg(palette.code_fg).bg(palette.code_bg);
    }
    if md.bold {
        style = style.fg(palette.bold).add_modifier(Modifier::BOLD);
    }
    if md.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if md.strikethrough {
        style = style
            .fg(palette.strikethrough)
            .add_modifier(Modifier::CROSSED_OUT);
    }
    if md.highlight {
        style = style
            .bg(palette.text_highlight_bg)
            .fg(palette.text_highlight_fg);
    }
    style
}

/// Restyles a line's leading Markdown marker, and swaps in the glyph the
/// reading pane uses wherever it is exactly as wide.
fn decorate_markers(
    chars: &[char],
    on_cursor_line: bool,
    palette: &Palette,
    out: &mut [(char, Style)],
) {
    let indent = chars
        .iter()
        .take_while(|ch| **ch == ' ' || **ch == '\t')
        .count();
    let rest: String = chars[indent..].iter().collect();
    let at = |offset: usize| indent + offset;

    // A heading keeps its hashes, dimmed, and colors its title.
    let hashes = rest.chars().take_while(|ch| *ch == '#').count();
    if (1..=6).contains(&hashes) && rest.chars().nth(hashes) == Some(' ') {
        let level = u8::try_from(hashes).unwrap_or(6);
        let title = Style::default()
            .fg(palette.heading(level))
            .add_modifier(Modifier::BOLD);
        for entry in out.iter_mut().skip(at(hashes + 1)) {
            entry.1 = title;
        }
        return;
    }

    // `- [x] `: color the box, tick it, and strike the item through.
    if let Some(checked) = task(&rest) {
        let color = if checked {
            palette.checkbox_done
        } else {
            palette.checkbox_todo
        };
        substitute(out, at(0), bullet_glyph(on_cursor_line), color);
        for offset in [2, 4] {
            if let Some(entry) = out.get_mut(at(offset)) {
                entry.1 = Style::default().fg(palette.text_faint);
            }
        }
        let tick = if checked { icons::TASK_DONE } else { ' ' };
        substitute(out, at(3), (!on_cursor_line).then_some(tick), color);
        if checked {
            for entry in out.iter_mut().skip(at(6)) {
                entry.1 = entry
                    .1
                    .fg(palette.text_faint)
                    .add_modifier(Modifier::CROSSED_OUT);
            }
        }
        return;
    }

    if matches!(rest.as_bytes().first(), Some(b'-' | b'*' | b'+'))
        && rest.as_bytes().get(1) == Some(&b' ')
    {
        substitute(
            out,
            at(0),
            bullet_glyph(on_cursor_line),
            palette.list_marker,
        );
        return;
    }

    // `1. ` keeps its number; only the color says it is a marker.
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && matches!(rest.as_bytes().get(digits), Some(b'.' | b')'))
        && rest.as_bytes().get(digits + 1) == Some(&b' ')
    {
        for entry in out.iter_mut().skip(indent).take(digits + 1) {
            entry.1 = Style::default().fg(palette.list_marker);
        }
        return;
    }

    // Every `>` of a quote, at every level, becomes the bar the reading pane
    // draws — and the body it introduces is tinted to match.
    if rest.starts_with('>') {
        let prefix = rest
            .bytes()
            .take_while(|b| matches!(b, b'>' | b' '))
            .count();
        for offset in 0..prefix {
            if chars.get(at(offset)) == Some(&'>') {
                substitute(
                    out,
                    at(offset),
                    (!on_cursor_line).then_some(icons::QUOTE_BAR),
                    palette.quote_bar,
                );
            }
        }
        for entry in out.iter_mut().skip(at(prefix)) {
            if entry.1.fg == Some(palette.text_normal) {
                entry.1 = entry.1.fg(palette.quote_fg);
            }
        }
    }
}

/// The glyph a list bullet is drawn as, or nothing on the cursor's own line
/// where the character as typed is what matters.
fn bullet_glyph(on_cursor_line: bool) -> Option<char> {
    (!on_cursor_line).then_some(icons::BULLET)
}

/// Recolors one character, optionally drawing a different glyph in its place.
///
/// Only ever called with a replacement the same width as the original, so the
/// column a character occupies never depends on how it is styled.
fn substitute(
    out: &mut [(char, Style)],
    at: usize,
    glyph: Option<char>,
    color: ratatui::style::Color,
) {
    if let Some(entry) = out.get_mut(at) {
        if let Some(glyph) = glyph {
            entry.0 = glyph;
        }
        entry.1 = Style::default().fg(color);
    }
}

/// Whether a line is a task, and whether it is done.
fn task(rest: &str) -> Option<bool> {
    let bytes = rest.as_bytes();
    let valid = matches!(bytes.first(), Some(b'-' | b'*' | b'+'))
        && bytes.get(1) == Some(&b' ')
        && bytes.get(2) == Some(&b'[')
        && bytes.get(4) == Some(&b']')
        && bytes.get(5) == Some(&b' ');
    valid.then(|| matches!(bytes.get(3), Some(b'x' | b'X')))
}

#[cfg(test)]
mod reading_boundary_tests {
    #[test]
    fn reading_renderer_is_callable_without_the_note_pane() {
        use crate::ui::reading::{Ctx, anchor_row, render_document};
        use emeraldian_core::{markdown, test_support::TempVault};
        use emeraldian_theme::{Palette, presets};

        let vault = TempVault::new("reading-boundary");
        let index = emeraldian_core::index::VaultIndex::build(vault.vault(), Default::default())
            .expect("index");
        let palette = Palette::from(&presets::default_theme());
        let mut ctx = Ctx::text(&palette, &index);
        let document = markdown::parse("# Heading\n\nplain words\n");
        let lines = render_document(&document, &mut ctx, 40);
        assert!(lines.iter().any(|line| line.to_string() == "plain words"));
        assert_eq!(anchor_row(&ctx.anchors, 2), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emeraldian_theme::{Palette, presets};

    fn palette() -> Palette {
        Palette::from(&presets::default_theme())
    }

    #[test]
    fn a_display_equation_is_tex_in_the_editor() {
        let lines: Vec<String> = ["$$", "x_1 + y_2 * z", "$$", "a_b c_d"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            fenced_lines(&lines),
            [Fence::Math, Fence::Math, Fence::Math, Fence::Prose]
        );

        // The underscores are TeX, not emphasis to dim and italicise.
        let palette = palette();
        let painted = paint_line(&lines[1], false, Fence::Math, &palette);
        assert!(
            painted
                .iter()
                .all(|(_, style)| style.fg == Some(palette.text_accent)
                    && !style.add_modifier.contains(Modifier::ITALIC))
        );
        let delimiters = paint_line(&lines[0], false, Fence::Math, &palette);
        assert!(
            delimiters
                .iter()
                .all(|(_, style)| style.fg == Some(palette.text_faint)),
            "the dollars are dimmed like any delimiter"
        );
    }

    #[test]
    fn an_unclosed_equation_does_not_take_over_the_editor() {
        let lines: Vec<String> = ["$$", "x^2", "", "# Next"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            fenced_lines(&lines)
                .iter()
                .all(|fence| *fence == Fence::Prose)
        );
    }

    #[test]
    fn code_fences_still_paint_as_code() {
        let lines: Vec<String> = ["```", "$$", "```", "text", "```"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            fenced_lines(&lines),
            [
                Fence::Code,
                Fence::Code,
                Fence::Code,
                Fence::Prose,
                Fence::Code
            ],
            "and an unclosed fence still runs to the end"
        );
    }
}
