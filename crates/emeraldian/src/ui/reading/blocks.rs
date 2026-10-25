//! Block dispatch and nested layout.

use super::*;

pub(super) fn render_block(
    block: &Block,
    ctx: &mut Ctx,
    width: usize,
    indent: usize,
    out: &mut Vec<Line<'static>>,
) {
    if ctx.breakout > 0
        && matches!(
            block.kind,
            BlockKind::Code { .. } | BlockKind::Math(_) | BlockKind::Table(_)
        )
    {
        let extra = std::mem::take(&mut ctx.breakout);
        let start = out.len();
        render_block(block, ctx, width + extra, indent, out);
        ctx.breakout = extra;
        // Display maths centres itself on the pane, which is also the middle
        // of the column; everything else is placed by what it needs.
        let mark = if matches!(block.kind, BlockKind::Math(_)) {
            ratatui::layout::Alignment::Center
        } else {
            ratatui::layout::Alignment::Left
        };
        for line in &mut out[start..] {
            line.alignment = Some(mark);
        }
        return;
    }
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
                Marker::Task(state) => {
                    let look = task_look(*state, palette);
                    (look.glyph.to_string(), look.glyph_style)
                }
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
            // Done and cancelled tasks are struck through, as in Obsidian, and
            // a cancelled one is dimmer still.
            if let Marker::Task(state) = marker {
                let look = task_look(*state, palette);
                if look.struck {
                    for line in &mut wrapped {
                        for span in &mut line.spans {
                            span.style = span
                                .style
                                .add_modifier(Modifier::CROSSED_OUT)
                                .fg(palette.text_faint);
                            if look.dim {
                                span.style = span.style.add_modifier(Modifier::DIM);
                            }
                        }
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
                out.push(Line {
                    alignment: line.alignment,
                    ..Line::from(spans)
                });
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
                out.push(Line {
                    alignment: line.alignment,
                    ..Line::from(spans)
                });
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
                paint_panel(&mut spans, palette.callout_bg, width, line.alignment);
                out.push(Line {
                    alignment: line.alignment,
                    ..Line::from(spans)
                });
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
                paint_panel(&mut spans, palette.callout_bg, width, line.alignment);
                out.push(Line {
                    alignment: line.alignment,
                    ..Line::from(spans)
                });
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

        BlockKind::Table(table) => render_table(table, palette, &pad, &mut ctx.links, out),

        BlockKind::Rule => out.push(Line::from(Span::styled(
            format!("{pad}{}", "─".repeat(inner_width)),
            Style::default().fg(palette.hr),
        ))),
    }
}

/// Puts a callout row on the callout panel: every span without a surface of its
/// own (a tag chip or code panel keeps theirs) gets the panel color, and the
/// row is filled out to `width` so the panel reads as one block. The first
/// span is the indent, which stays outside it. A `reset` panel is the
/// terminal's own page: nothing to paint, the row is left alone.
fn paint_panel(
    spans: &mut Vec<Span<'static>>,
    panel: ratatui::style::Color,
    width: usize,
    alignment: Option<ratatui::layout::Alignment>,
) {
    if panel == ratatui::style::Color::Reset {
        return;
    }
    for span in spans.iter_mut().skip(1) {
        if span.style.bg.is_none() {
            span.style = span.style.bg(panel);
        }
    }
    // A centred row (display maths) is placed by its width, so it must not be
    // stretched to the whole block.
    if alignment == Some(ratatui::layout::Alignment::Center) {
        return;
    }
    let used: usize = spans.iter().map(Span::width).sum();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(panel),
        ));
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// How a task's box, and the text after it, look for one state character.
struct TaskLook {
    glyph: char,
    glyph_style: Style,
    /// The text is struck through and drawn in the faint colour.
    struck: bool,
    /// The text is dimmed on top of that.
    dim: bool,
}

/// The look of `- [<state>]`: Obsidian's `x` and `-` end a task, the rest of
/// the common sets (`>` forwarded, `<` scheduled, `?`, `!`, `/` in progress)
/// each get a box and a colour of their own, and any other character is a
/// custom state, which Obsidian counts as complete. Only the palette's roles
/// are used for colour, so every theme draws them in its own.
fn task_look(state: char, palette: &Palette) -> TaskLook {
    let plain = |glyph: char, colour| TaskLook {
        glyph,
        glyph_style: Style::default().fg(colour),
        struck: false,
        dim: false,
    };
    match state {
        ' ' => plain(icons::TASK_TODO, palette.checkbox_todo),
        'x' | 'X' => TaskLook {
            struck: true,
            ..plain(icons::TASK_DONE, palette.checkbox_done)
        },
        '-' => TaskLook {
            struck: true,
            dim: true,
            glyph_style: Style::default()
                .fg(palette.text_faint)
                .add_modifier(Modifier::DIM),
            ..plain(icons::TASK_CANCELLED, palette.text_faint)
        },
        '>' => plain(icons::TASK_FORWARDED, palette.text_info),
        '<' => plain(icons::TASK_SCHEDULED, palette.callout_tip),
        '?' => plain(icons::TASK_QUESTION, palette.callout_question),
        '!' => TaskLook {
            glyph_style: Style::default()
                .fg(palette.callout_danger)
                .add_modifier(Modifier::BOLD),
            ..plain(icons::TASK_IMPORTANT, palette.callout_danger)
        },
        '/' => plain(icons::TASK_PROGRESS, palette.text_warning),
        _ => plain(icons::TASK_OTHER, palette.text_accent),
    }
}
