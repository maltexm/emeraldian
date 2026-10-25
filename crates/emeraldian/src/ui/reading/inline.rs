//! Inline styling, wrapping and display maths.

use super::*;

/// Lays out inline spans, lifting any picture out onto rows of its own.
///
/// A picture is a block in a line of text: the words around it wrap as usual
/// above and below, and it gets however many rows it needs in between. When it
/// can't be drawn — no support in the terminal, a missing file, a link to the
/// web — nothing is lifted out and it stays inline as its alt text.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_spans(
    spans: &[markdown::Span],
    ctx: &mut Ctx,
    width: usize,
    indent: usize,
    first_pad: &str,
    rest_pad: &str,
    out: &mut Vec<Line<'static>>,
) {
    let base = Style::default().fg(ctx.palette.text_normal);
    render_styled_spans(spans, ctx, width, indent, first_pad, rest_pad, base, out);
}

/// [`render_spans`] for text whose plain parts take `base` rather than the
/// body colour, such as a heading or a callout title. Links, tags and maths
/// inside it are still coloured as they are anywhere else.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_styled_spans(
    spans: &[markdown::Span],
    ctx: &mut Ctx,
    width: usize,
    indent: usize,
    first_pad: &str,
    rest_pad: &str,
    base: Style,
    out: &mut Vec<Line<'static>>,
) {
    let mut run: Vec<markdown::Span> = Vec::new();
    // Text resuming after something lifted out starts a line of its own, and
    // shouldn't start it with the space that separated them.
    let mut lifted = false;
    for span in spans {
        // A hard break ends the row: what came before it is laid out now, and
        // what follows starts a row of its own, without the space it began
        // with. Two in a row leave a blank row, as they do in HTML.
        if span.kind == SpanKind::Break {
            if let Some(last) = run.last_mut() {
                last.text = last.text.trim_end().to_string();
            }
            run.retain(|span| !span.text.is_empty());
            let pad = if out.is_empty() { first_pad } else { rest_pad };
            let styled = style_spans(&run, ctx.palette, ctx.index, base);
            out.extend(wrap_spans(&styled, width, pad, rest_pad));
            run.clear();
            lifted = true;
            continue;
        }
        let lifted_out = match span.kind {
            // `$$…$$` written mid-paragraph is lifted out the same way, since
            // Obsidian sets it apart from the text around it.
            SpanKind::Math { display: true } => None,
            _ => match lift(span, ctx, width + ctx.breakout, rest_pad) {
                Some(found) => Some(found),
                None => {
                    let mut span = span.clone();
                    if lifted && run.is_empty() {
                        span.text = span.text.trim_start().to_string();
                    }
                    if !span.text.is_empty() {
                        run.push(span);
                    }
                    continue;
                }
            },
        };
        lifted = true;

        if !run.is_empty() {
            let pad = if out.is_empty() { first_pad } else { rest_pad };
            let styled = style_spans(&run, ctx.palette, ctx.index, base);
            out.extend(wrap_spans(&styled, width, pad, rest_pad));
            run.clear();
        }

        let (source, size) = match lifted_out {
            None => {
                let pad = if out.is_empty() { first_pad } else { rest_pad };
                let start = out.len();
                display_math(&span.text, ctx.palette, width + ctx.breakout, pad, out);
                // Centred on the pane, which is also the column's middle.
                for line in &mut out[start..] {
                    line.alignment = Some(ratatui::layout::Alignment::Center);
                }
                continue;
            }
            Some(Lift::Lines(drawn)) => {
                out.extend(drawn.into_iter().map(|line| line.left_aligned()));
                continue;
            }
            Some(Lift::Picture(source, size)) => (source, size),
        };
        ctx.pictures.push(Picture {
            line: out.len(),
            indent: indent as u16,
            size,
            source,
        });
        // The picture is drawn over these rows once it has been encoded. They
        // are blank so that the text below sits where it will finally sit, and
        // so a half-scrolled picture leaves no debris behind.
        out.extend(std::iter::repeat_n(
            Line::from("").left_aligned(),
            size.height as usize,
        ));
    }

    if !run.is_empty() || out.is_empty() {
        let pad = if out.is_empty() { first_pad } else { rest_pad };
        let styled = style_spans(&run, ctx.palette, ctx.index, base);
        out.extend(wrap_spans(&styled, width, pad, rest_pad));
    }
}

/// Moves pictures laid out in a nested buffer to where that buffer landed.
pub(super) fn shift(pictures: &mut [Picture], lines: usize, indent: u16) {
    for picture in pictures {
        picture.line += lines;
        picture.indent += indent;
    }
}

/// Applies theme colors to parsed inline spans, starting from `base`.
fn style_spans(
    spans: &[markdown::Span],
    palette: &Palette,
    index: &emeraldian_core::index::VaultIndex,
    base: Style,
) -> Vec<(String, Style)> {
    spans
        .iter()
        .map(|span| {
            let mut style = base;

            match &span.kind {
                SpanKind::WikiLink { target, .. } => {
                    // A link to a note that doesn't exist yet is dimmed rather
                    // than hidden — that's the signal to go write it.
                    style = if index.resolve(target).is_some() {
                        style.fg(palette.link).add_modifier(Modifier::UNDERLINED)
                    } else {
                        style.fg(palette.link_unresolved)
                    };
                }
                SpanKind::Link { .. } => {
                    style = style
                        .fg(palette.link_external)
                        .add_modifier(Modifier::UNDERLINED);
                }
                // Reached only when the picture could not be drawn, so this is
                // the alt text standing in for it.
                SpanKind::Image { .. } => style = style.fg(palette.text_faint),
                SpanKind::Tag(_) => {
                    style = style.fg(palette.tag_fg).bg(palette.tag_bg);
                }
                SpanKind::Math { .. } => {
                    style = style.fg(palette.text_accent);
                    return (
                        math::inline(&span.text),
                        emphasis(style, span.style, palette),
                    );
                }
                // A key cap: dim text on the tag chip's background, padded
                // with no-break spaces so that a row never splits it.
                SpanKind::Kbd => {
                    style = style.fg(palette.text_muted).bg(palette.tag_bg);
                    return (
                        format!("\u{a0}{}\u{a0}", span.text),
                        emphasis(style, span.style, palette),
                    );
                }
                SpanKind::Text | SpanKind::Break => {}
            }

            (span.text.clone(), emphasis(style, span.style, palette))
        })
        .collect()
}

/// Inline spans as the reading pane shows them, for the places that draw them
/// as a single run: headings, callout titles and table cells.
pub(super) fn shown_text(spans: &[markdown::Span]) -> String {
    spans
        .iter()
        .map(|span| match span.kind {
            SpanKind::Math { .. } => math::inline(&span.text),
            _ => span.text.clone(),
        })
        .collect()
}

/// Sets an equation on rows of its own, centred as Obsidian centres it.
///
/// One wider than the pane starts at the margin instead and is panned across,
/// like a wide table. `width` counts the `pad` in front of each row, as it does
/// for [`wrap_spans`].
pub(super) fn display_math(
    tex: &str,
    palette: &Palette,
    width: usize,
    pad: &str,
    out: &mut Vec<Line<'static>>,
) {
    let set = math::display(tex);
    let style = Style::default().fg(palette.text_accent);
    let room = width.saturating_sub(pad.width());
    let tag = set.tag.as_deref().map(|tag| (tag, tag.width()));
    // A tag sits at the right margin, so the equation is centred clear of it.
    let reserve = tag.map_or(0, |(_, tag_width)| 2 * (tag_width + 2));
    let left = if set.width + reserve <= room {
        (room - set.width) / 2
    } else {
        0
    };

    for (at, row) in set.rows.iter().enumerate() {
        let row = row.trim_end();
        let mut spans = vec![
            Span::raw(format!("{pad}{}", " ".repeat(left))),
            Span::styled(row.to_string(), style),
        ];
        if at == set.baseline
            && let Some((tag, tag_width)) = tag
        {
            let used = left + row.width();
            let gap = room.saturating_sub(used + tag_width).max(2);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(
                tag.to_string(),
                Style::default().fg(palette.text_muted),
            ));
        }
        out.push(Line::from(spans));
    }
}

/// Wraps styled spans to `width`, breaking on spaces and keeping styles intact.
///
/// Widths are display columns, so wide characters wrap where they are drawn.
/// A word wider than a whole line, such as a long URL, is broken at the edge
/// of the pane rather than left running off it.
#[must_use]
pub fn wrap_spans(
    spans: &[(String, Style)],
    width: usize,
    first_indent: &str,
    indent: &str,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let indent_width = indent.width();
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<Span> = vec![Span::raw(first_indent.to_string())];
    let mut used = first_indent.width();
    let mut break_line = |current: &mut Vec<Span<'static>>, used: &mut usize| {
        lines.push(Line::from(std::mem::take(current)));
        current.push(Span::raw(indent.to_string()));
        *used = indent_width;
    };

    for (text, style) in spans {
        // Splitting inclusively keeps the space attached to the preceding word,
        // so styles don't fragment on every gap.
        for word in text.split_inclusive(' ') {
            let mut word = word;
            if used + word.width() > width && used > indent_width {
                break_line(&mut current, &mut used);
                // A wrapped line shouldn't start with the space that ended the
                // previous one.
                word = word.trim_start();
                if word.is_empty() {
                    continue;
                }
            }
            // Checked for emptiness as well as width: with an indent as wide as
            // the pane, even the last character of a word is too wide.
            while !word.trim_end().is_empty() && used + word.trim_end().width() > width {
                let (head, rest) = split_at_column(word, width.saturating_sub(used));
                current.push(Span::styled(head.to_string(), *style));
                break_line(&mut current, &mut used);
                word = rest;
            }
            if !word.is_empty() {
                current.push(Span::styled(word.to_string(), *style));
                used += word.width();
            }
        }
    }

    lines.push(Line::from(current));
    lines
}

/// Splits `text` after as many characters as fit in `columns`, and after one
/// at the least, so that breaking a word always makes progress.
fn split_at_column(text: &str, columns: usize) -> (&str, &str) {
    use unicode_width::UnicodeWidthChar;

    let mut used = 0;
    for (at, ch) in text.char_indices() {
        let width = ch.width().unwrap_or(0);
        if at > 0 && used + width > columns {
            return text.split_at(at);
        }
        used += width;
    }
    (text, "")
}
