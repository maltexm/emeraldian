//! Image and diagram embeds, including recursion guards.

use super::*;

/// How deeply embeds may nest before they are refused.
///
/// A note is depth 0 and starts the count; Obsidian stops long before a chain
/// of embeds could recurse forever, and shows a placeholder instead.
pub(super) const MAX_EMBED_DEPTH: usize = 5;

/// What an inline span turns into when it is lifted out of its line of text.
pub(super) enum Lift {
    /// Blank rows to draw a picture or a drawing over, and what to draw.
    Picture(PictureSource, Size),
    /// Rows of text that already are the drawing.
    Lines(Vec<Line<'static>>),
}

/// Most rows an embedded drawing takes among the prose.
const EMBEDDED_DRAWING_ROWS: usize = 24;

/// Works out what a span lifts out to, if anything, and how much room it needs.
pub(super) fn lift(span: &markdown::Span, ctx: &mut Ctx, width: usize, pad: &str) -> Option<Lift> {
    let (target, embed) = match &span.kind {
        SpanKind::Image { src } => (src, false),
        // `![[x]]` embeds a note as often as a picture, and only the vault
        // knows which; a note embed finds no image file and stays a link.
        SpanKind::WikiLink {
            target,
            embed: true,
            ..
        } => (target, true),
        _ => return None,
    };

    let Some(path) = ctx
        .index
        .attachment_path(target, ctx.note_dir.as_deref())
        .filter(|path| images::is_image(path))
    else {
        return embedded_diagram(target, ctx, width, pad);
    };

    let images = ctx.images.as_deref_mut()?;
    let mut room = Size::new(u16::try_from(width).unwrap_or(u16::MAX), u16::MAX);
    // Obsidian sizes an embed with `![[chart.png|400]]`, in pixels. Anything
    // else after the pipe is an alias, which is only ever alt text.
    if let Some(pixels) = embed.then(|| width_hint(&span.text)).flatten() {
        room.width = room.width.min(images.cells_wide(pixels)?);
    }

    let size = images.measure(&path, room)?;
    (size.width > 0 && size.height > 0).then_some(Lift::Picture(PictureSource::File(path), size))
}

/// The drawing or Mermaid diagram an embed names, ready to be laid out.
///
/// `None` when the target is neither or can't be read: the embed stays a link
/// then. A Mermaid file that can't be drawn is still laid out, as its source
/// under the reason, the same as a block of it in the note.
///
/// Every embed target passes through here, so the guard against a chain of
/// embeds that never ends — a file embedding itself, or two files embedding
/// each other — lives here too, as Obsidian does it: a stack of what is
/// already being rendered plus a shallow depth limit, and a placeholder where
/// the recursion would have gone.
pub(super) fn embedded_diagram(
    target: &str,
    ctx: &mut Ctx,
    width: usize,
    pad: &str,
) -> Option<Lift> {
    let decoded = target.replace("%20", " ");
    let index = ctx.index;
    let id = index.resolve(&decoded).or_else(|| {
        // `![](drawings/flow.excalidraw)` is relative to the note holding it.
        let path = ctx.note_dir.as_ref()?.join(&decoded);
        let rel = emeraldian_core::note::relative_path(&index.vault.path, &path)?;
        index.id_of_rel(&rel)
    })?;
    let rel = index.note(id)?.meta.rel.clone();

    if ctx.embeds.contains(&rel) {
        return Some(refused(&format!("circular embed: {rel}"), ctx, pad));
    }
    if ctx.embeds.len() >= MAX_EMBED_DEPTH {
        return Some(refused(&format!("embed too deep: {rel}"), ctx, pad));
    }

    if excalidraw::is_drawing(&rel) {
        let scene = excalidraw::parse(&index.read(id).ok()?)?;
        let (cols, rows) = drawing::fit(&scene, width, EMBEDDED_DRAWING_ROWS)?;
        return Some(Lift::Picture(
            PictureSource::Drawing(Arc::new(scene)),
            Size::new(cols, rows),
        ));
    }
    if is_mermaid_file(&rel) {
        let document = mermaid_document(&index.read(id).ok()?);
        let indent = pad.chars().count();
        let mut lines = Vec::new();
        ctx.embeds.push(rel);
        for block in &document.blocks {
            render_block(block, ctx, width + indent, indent, &mut lines);
        }
        ctx.embeds.pop();
        return Some(Lift::Lines(lines));
    }
    None
}

/// The line an embed becomes when it must not be followed.
fn refused(words: &str, ctx: &Ctx, pad: &str) -> Lift {
    Lift::Lines(vec![Line::from(Span::styled(
        format!("{pad}{words}"),
        Style::default().fg(ctx.palette.text_faint),
    ))])
}

/// A Mermaid file as the one code block it is, which draws the diagram or says
/// why it can't and shows the source.
///
/// The source is data, never parsed as Markdown: interpolating it into a
/// fence of our own would let a ``` inside it close that fence, and everything
/// after it — an embed of this very file — would run again.
pub(super) fn mermaid_document(source: &str) -> markdown::Document {
    markdown::Document {
        blocks: vec![Block {
            line: 0,
            kind: BlockKind::Code {
                lang: "mermaid".to_string(),
                lines: source.lines().map(str::to_string).collect(),
            },
        }],
    }
}

/// Whether a note's file is Mermaid source rather than Markdown.
pub(super) fn is_mermaid_file(rel: &str) -> bool {
    matches!(
        emeraldian_core::note::own_extension(rel),
        Some(".mmd" | ".mermaid")
    )
}

/// Reads Obsidian's `|400` or `|400x300` embed size, in pixels.
fn width_hint(alias: &str) -> Option<u32> {
    let width = alias.split_once('x').map_or(alias, |(width, _)| width);
    width.trim().parse().ok().filter(|&pixels| pixels > 0)
}
