//! Reading renderer: context and document entry point.
//!
//! Blocks, inline text, embeds, tables and code share a single layout pass.
//! The view module owns the cache and paints its result; the note pane owns
//! tab chrome and the unchanged editor painter.

mod blocks;
mod code;
mod embeds;
mod inline;
mod layout;
mod tables;
mod view;

use super::note::{emphasis, prose_area};
use blocks::render_block;
use code::highlight;
#[cfg(test)]
use embeds::embedded_diagram;
use embeds::{Lift, is_mermaid_file, lift, mermaid_document};
pub use inline::wrap_spans;
use inline::{display_math, render_spans, render_styled_spans, shift};
use tables::render_table;
pub(super) use view::draw_reading;
pub use view::{ReadingCache, link_at};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use unicode_width::UnicodeWidthStr;

use emeraldian_core::excalidraw;
use emeraldian_core::index::VaultIndex;
use emeraldian_core::markdown::{self, Align, Block, BlockKind, Marker, SpanKind, Table};
use emeraldian_theme::Palette;

use crate::app::{App, Regions, Reveal};
use crate::images::{self, Images};
use crate::ui::{drawing, icons, math, mermaid, scrollbar};

/// Everything the markdown renderer needs beyond the blocks themselves.
///
/// Bundled because pictures made the parameter list unwieldy: laying one out
/// needs the vault to find the file, the note's own folder to resolve a
/// relative path against, and the terminal's font size to know how many rows
/// it will fill.
pub struct Ctx<'a> {
    pub palette: &'a Palette,
    pub index: &'a VaultIndex,
    /// The folder of the note being read, where a relative `![](chart.png)` is
    /// looked for first.
    pub note_dir: Option<PathBuf>,
    /// `None` renders every picture as its alt text, which is what happens in
    /// a terminal that cannot draw and in tests.
    pub images: Option<&'a mut Images>,
    /// Where each picture ended up, filled in as blocks are laid out.
    pub pictures: Vec<Picture>,
    /// Which rendered row each block's source line ended up on, filled in as
    /// blocks are laid out.
    ///
    /// A source line and a rendered row are not the same number — prose wraps,
    /// headings gain a rule, a picture takes a dozen rows — so scrolling to a
    /// heading the outline points at needs the translation written down.
    pub anchors: Vec<(usize, usize)>,
    /// The files currently being rendered, the note being read first, so an
    /// embed that names one of them can be refused instead of followed into a
    /// cycle. Obsidian keeps the same stack for the same reason.
    pub embeds: Vec<String>,
    /// Prose measure inside the available pane; zero leaves it unrestricted.
    pub reading_width: usize,
    /// Additional room available to lifted content outside the prose measure.
    pub breakout: usize,
}

impl<'a> Ctx<'a> {
    /// A context that renders text only.
    #[cfg(test)]
    #[must_use]
    pub fn text(palette: &'a Palette, index: &'a VaultIndex) -> Self {
        Self {
            palette,
            index,
            note_dir: None,
            images: None,
            pictures: Vec::new(),
            anchors: Vec::new(),
            embeds: Vec::new(),
            reading_width: 0,
            breakout: 0,
        }
    }
}

/// A picture and the rows reserved for it.
#[derive(Debug, Clone)]
pub struct Picture {
    /// Index of its first row in the rendered lines.
    pub line: usize,
    /// Columns from the left edge of the pane.
    pub indent: u16,
    pub size: Size,
    pub source: PictureSource,
}

/// What a picture's rows are drawn from.
#[derive(Debug, Clone)]
pub enum PictureSource {
    /// An image file, drawn by the terminal's picture protocol.
    File(PathBuf),
    /// An Excalidraw scene, traced on a braille canvas over its rows.
    Drawing(Arc<excalidraw::Drawing>),
}

/// Turns a parsed document into styled terminal lines.
///
/// Pictures are laid out here too, as runs of blank lines with their positions
/// collected in `ctx.pictures`, so that scrolling and the scrollbar count them
/// like any other content.
pub fn render_document(
    document: &markdown::Document,
    ctx: &mut Ctx,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for block in &document.blocks {
        ctx.anchors.push((block.line, lines.len()));
        layout::render_column_block(block, ctx, width, ctx.reading_width, &mut lines);
    }
    lines
}

/// The rendered row a source line was drawn on.
///
/// Only block starts are recorded, so a line inside a block resolves to the row
/// its block began on — which is what scrolling to a heading wants anyway.
#[must_use]
pub fn anchor_row(anchors: &[(usize, usize)], line: usize) -> usize {
    anchors
        .iter()
        .take_while(|(source, _)| *source <= line)
        .last()
        .map_or(line, |(_, row)| *row)
}

#[cfg(test)]
mod tests;
