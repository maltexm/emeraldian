//! Block dispatch and nested layout.

use super::*;

pub(super) fn render_block(
    block: &Block,
    ctx: &mut Ctx,
    width: usize,
    indent: usize,
    out: &mut Vec<Line<'static>>,
) {
    let palette = ctx.palette;
    let pad = " ".repeat(indent);
    let inner_width = width.saturating_sub(indent).max(8);

    match &block.kind {
        BlockKind::Blank => out.push(Line::from("")),

        BlockKind::Frontmatter(entries) => {
            for (key, value) in entries {
                let entry = [
                    (
                        format!("{key}: "),
                        Style::default().fg(palette.frontmatter_key),
                    ),
                    (
                        value.clone(),
                        Style::default().fg(palette.frontmatter_value),
                    ),
                ];
                out.extend(wrap_spans(&entry, width, &pad, &format!("{pad}  ")));
            }
            out.push(Line::from(Span::styled(
                "─".repeat(width.min(60)),
                Style::default().fg(palette.hr),
            )));
            out.push(Line::from(""));
        }

        BlockKind::Heading { level, spans } => {
            let style = Style::default()
                .fg(palette.heading(*level))
                .add_modifier(Modifier::BOLD);
            // Wrapped like a paragraph is. A heading can be longer than the
            // pane, and one pasted in from elsewhere can carry a whole page of
            // text and maths on its single line.
            let start = out.len();
            render_styled_spans(spans, ctx, inner_width, indent, &pad, &pad, style, out);
            // Obsidian underlines H1 and H2; a rule is the terminal equivalent
            // of the larger type it uses to separate sections.
            if *level <= 2 {
                let widest = out[start..]
                    .iter()
                    .map(Line::width)
                    .max()
                    .unwrap_or(0)
                    .saturating_sub(indent);
                out.push(Line::from(Span::styled(
                    format!("{pad}{}", "─".repeat(inner_width.min(widest + 8))),
                    Style::default().fg(palette.border),
                )));
            }
        }

        BlockKind::Paragraph(spans) => {
            render_spans(spans, ctx, inner_width, indent, &pad, &pad, out);
        }

        BlockKind::ListItem {
            depth,
            marker,
            spans,
        } => {
            let list_indent = " ".repeat(indent + depth * 2);
            let (glyph, glyph_style) = match marker {
                Marker::Bullet => (
                    icons::BULLET.to_string(),
                    Style::default().fg(palette.list_marker),
                ),
                Marker::Ordered(n) => (format!("{n}."), Style::default().fg(palette.list_marker)),
                Marker::Task(true) => (
                    icons::TASK_DONE.to_string(),
                    Style::default().fg(palette.checkbox_done),
                ),
                Marker::Task(false) => (
                    icons::TASK_TODO.to_string(),
                    Style::default().fg(palette.checkbox_todo),
                ),
            };

            let prefix_width = glyph.chars().count() + 1;
            let continuation = format!("{list_indent}{}", " ".repeat(prefix_width));
            let text_indent = indent + depth * 2 + prefix_width;

            let mark = ctx.pictures.len();
            let mut wrapped = Vec::new();
            render_spans(
                spans,
                ctx,
                inner_width.saturating_sub(depth * 2 + prefix_width),
                text_indent,
                "",
                "",
                &mut wrapped,
            );
            // Completed tasks are struck through, as in Obsidian.
            if matches!(marker, Marker::Task(true)) {
                for line in &mut wrapped {
                    for span in &mut line.spans {
                        span.style = span
                            .style
                            .add_modifier(Modifier::CROSSED_OUT)
                            .fg(palette.text_faint);
                    }
                }
            }
            shift(&mut ctx.pictures[mark..], out.len(), 0);

            if wrapped.is_empty() {
                wrapped.push(Line::from(""));
            }
            for (i, line) in wrapped.into_iter().enumerate() {
                let mut spans = Vec::new();
                if i == 0 {
                    spans.push(Span::raw(list_indent.clone()));
                    spans.push(Span::styled(format!("{glyph} "), glyph_style));
                } else {
                    spans.push(Span::raw(continuation.clone()));
                }
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }

        BlockKind::Quote(body) => {
            let mark = ctx.pictures.len();
            let mut inner = Vec::new();
            for child in body {
                render_block(child, ctx, width.saturating_sub(2), 0, &mut inner);
            }
            // Every inner line gains a quote bar and lands after what is
            // already in `out`, so anything laid out inside moves with it.
            shift(&mut ctx.pictures[mark..], out.len(), indent as u16 + 2);
            for line in inner {
                let mut spans = vec![
                    Span::raw(pad.clone()),
                    Span::styled(
                        format!("{} ", icons::QUOTE_BAR),
                        Style::default().fg(palette.quote_bar),
                    ),
                ];
                spans.extend(line.spans.into_iter().map(|mut span| {
                    span.style = span.style.fg(palette.quote_fg);
                    span
                }));
                out.push(Line::from(spans));
            }
        }

        BlockKind::Callout { kind, title, body } => {
            let color = palette.callout(kind);
            let fallback = [markdown::Span::plain(capitalize(kind))];
            let title: &[markdown::Span] = if title.is_empty() { &fallback } else { title };

            // The title wraps under its bar, like the body below it.
            let mark = ctx.pictures.len();
            let mut heading = Vec::new();
            let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            render_styled_spans(
                title,
                ctx,
                width.saturating_sub(2),
                0,
                "",
                "",
                style,
                &mut heading,
            );
            shift(&mut ctx.pictures[mark..], out.len(), indent as u16 + 2);
            for line in heading {
                let mut spans = vec![
                    Span::raw(pad.clone()),
                    Span::styled(format!("{} ", icons::QUOTE_BAR), Style::default().fg(color)),
                ];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }

            let mark = ctx.pictures.len();
            let mut inner = Vec::new();
            for child in body {
                render_block(child, ctx, width.saturating_sub(2), 0, &mut inner);
            }
            shift(&mut ctx.pictures[mark..], out.len(), indent as u16 + 2);
            for line in inner {
                let mut spans = vec![
                    Span::raw(pad.clone()),
                    Span::styled(format!("{} ", icons::QUOTE_BAR), Style::default().fg(color)),
                ];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }

        BlockKind::Code { lang, lines: code } => {
            // A Mermaid block is a diagram first and source second: drawn on
            // the character grid when it is a flowchart that parses, and shown
            // as code under a line saying why when it is not.
            let mut why = None;
            if lang.eq_ignore_ascii_case("mermaid") {
                match mermaid::lines(&code.join("\n"), palette, &format!("{pad} ")) {
                    Ok(drawn) => {
                        out.extend(drawn);
                        return;
                    }
                    Err(reason) => why = Some(reason),
                }
            }
            let background = Style::default().bg(palette.code_bg);
            if !lang.is_empty() {
                out.push(Line::from(Span::styled(
                    format!("{pad} {lang} "),
                    background.fg(palette.text_faint),
                )));
            }
            if let Some(reason) = why {
                out.extend(wrap_spans(
                    &[(
                        format!("mermaid: {reason}"),
                        Style::default().fg(palette.text_faint),
                    )],
                    width,
                    &format!("{pad} "),
                    &format!("{pad}   "),
                ));
            }
            for line in code {
                let mut spans = vec![Span::styled(format!("{pad} "), background)];
                spans.extend(highlight(line, lang, palette, background));
                // Pad to the pane width so the block reads as a filled panel.
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                if used < width {
                    spans.push(Span::styled(" ".repeat(width - used), background));
                }
                out.push(Line::from(spans));
            }
        }

        BlockKind::Math(tex) => display_math(tex, palette, inner_width, &pad, out),

        BlockKind::Table(table) => render_table(table, palette, &pad, out),

        BlockKind::Rule => out.push(Line::from(Span::styled(
            format!("{pad}{}", "─".repeat(inner_width)),
            Style::default().fg(palette.hr),
        ))),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
