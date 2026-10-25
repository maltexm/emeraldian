use super::embeds::MAX_EMBED_DEPTH;
use super::*;
use emeraldian_core::test_support::TempVault;
use emeraldian_theme::{Palette, presets};

#[test]
fn prose_wraps_in_a_centred_hundred_column_measure() {
    let vault = TempVault::new("reading-column");
    vault.write("A.md", &"word ".repeat(80));
    let mut app = open(&vault, "A.md");
    app.focus_mode = true;
    let rows = screen_rows(&mut app);
    let row = rows.iter().find(|row| row.contains("word")).unwrap();
    assert_eq!(row.find("word"), Some(50));
    assert!(row.trim().len() <= 100);
    assert!(app.reading.lines.len() >= 4);
}

#[test]
fn wide_blocks_break_out_without_widening_surrounding_prose() {
    let vault = TempVault::new("reading-breakout");
    let code = "x".repeat(120);
    vault.write(
        "A.md",
        &format!("Before\n\n```text\n{code}\n```\n\nAfter\n"),
    );
    let mut app = open(&vault, "A.md");
    app.focus_mode = true;
    let rows = screen_rows(&mut app);
    assert_eq!(
        rows.iter()
            .find(|r| r.contains("Before"))
            .unwrap()
            .find("Before"),
        Some(50)
    );
    assert_eq!(
        rows.iter().find(|r| r.contains(&code)).unwrap().find(&code),
        // Centred on the pane (194 columns wide here), wider than the prose.
        Some(40)
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.contains("After"))
            .unwrap()
            .find("After"),
        Some(50)
    );
}

#[test]
fn inline_images_break_out_but_the_words_around_them_keep_the_measure() {
    let vault = TempVault::new("column-image");
    vault.write_bytes("chart.png", &png(1600, 200));
    let index = vault.index();
    let palette = palette();
    let mut images = images(30);
    let mut ctx = drawing(&palette, &index, &mut images);
    ctx.reading_width = 40;
    let lines = render_document(
        &markdown::parse("Before ![chart](chart.png) After"),
        &mut ctx,
        160,
    );
    assert_eq!(ctx.pictures[0].size.width, 160);
    assert_eq!(ctx.pictures[0].indent, 0);
    let text = text_of(&lines);
    assert_eq!(text[0].find("Before"), Some(60));
    assert_eq!(text.last().unwrap().find("After"), Some(60));
}

#[test]
fn quoted_code_that_fits_the_column_keeps_its_bar_in_the_column() {
    let vault = TempVault::new("quoted-column");
    let index = vault.index();
    let palette = palette();
    let mut ctx = Ctx::text(&palette, &index);
    ctx.reading_width = 40;
    let lines = render_document(
        &markdown::parse("> words\n>\n> ```rust\n> wide_code\n> ```\n"),
        &mut ctx,
        120,
    );
    let text = text_of(&lines);
    // Bar and space, then the panel's own space in front of the code.
    assert_eq!(first_column_of(&text, "words"), 42);
    assert_eq!(first_column_of(&text, "wide_code"), 43);
    assert_eq!(first_column_of(&text, "▎"), 40);
}

#[test]
fn quoted_code_wider_than_the_column_breaks_out_with_its_bar() {
    let vault = TempVault::new("quoted-wide-column");
    let index = vault.index();
    let palette = palette();
    let mut ctx = Ctx::text(&palette, &index);
    ctx.reading_width = 40;
    let source = format!("> words\n>\n> ```rust\n> {}\n> ```\n", "x".repeat(80));
    let lines = render_document(&markdown::parse(&source), &mut ctx, 120);
    let text = text_of(&lines);
    assert_eq!(first_column_of(&text, "words"), 42);
    // Bar, space and panel space (83 columns), centred in 120.
    let bar = text.iter().find(|row| row.contains("xxx")).unwrap();
    assert_eq!(
        bar.find('▎').map(|at| UnicodeWidthStr::width(&bar[..at])),
        Some(18)
    );
    assert_eq!(first_column_of(&text, "xxx"), 21);
}

/// Rows with their common left margin removed, so two renders of the same
/// block compare equal wherever the block was placed.
fn dedented(lines: &[Line<'_>]) -> Vec<String> {
    let rows = text_of(lines);
    let margin = rows
        .iter()
        .filter(|row| !row.trim().is_empty())
        .map(|row| row.len() - row.trim_start().len())
        .min()
        .unwrap_or(0);
    rows.iter()
        .map(|row| row.get(margin..).unwrap_or("").trim_end().to_string())
        .collect()
}

fn first_column_of(rows: &[String], needle: &str) -> usize {
    let row = rows
        .iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no row shows {needle:?}: {rows:#?}"));
    UnicodeWidthStr::width(row.split(needle).next().unwrap())
}

#[test]
fn a_table_that_fits_the_column_lines_up_with_the_prose() {
    let vault = TempVault::new("column-table");
    let index = vault.index();
    let palette = palette();
    let mut ctx = Ctx::text(&palette, &index);
    ctx.reading_width = 100;
    let lines = render_document(
        &markdown::parse("Prose\n\n| A | B |\n|---|---|\n| 1 | 2 |\n"),
        &mut ctx,
        160,
    );
    let rows = text_of(&lines);
    assert_eq!(first_column_of(&rows, "Prose"), 30);
    assert_eq!(first_column_of(&rows, "┌"), 30, "{rows:#?}");
}

#[test]
fn code_that_fits_the_column_is_a_column_wide_panel() {
    let vault = TempVault::new("column-code");
    let index = vault.index();
    let palette = palette();
    let mut ctx = Ctx::text(&palette, &index);
    ctx.reading_width = 100;
    let lines = render_document(
        &markdown::parse("Prose\n\n```rust\nlet a = 1;\n```\n"),
        &mut ctx,
        160,
    );
    let rows = text_of(&lines);
    assert_eq!(first_column_of(&rows, "let a"), 31, "{rows:#?}");
    let code = rows.iter().find(|row| row.contains("let a")).unwrap();
    assert_eq!(UnicodeWidthStr::width(code.as_str()), 130, "{rows:#?}");
}

#[test]
fn content_wider_than_the_column_is_centred_on_the_pane() {
    let vault = TempVault::new("column-wide-code");
    let index = vault.index();
    let palette = palette();
    let mut ctx = Ctx::text(&palette, &index);
    ctx.reading_width = 100;
    let source = format!("```text\n{}\n```\n", "x".repeat(120));
    let lines = render_document(&markdown::parse(&source), &mut ctx, 160);
    let rows = text_of(&lines);
    // One space of panel padding in front of 120 characters: 121 columns.
    assert_eq!(first_column_of(&rows, "xxx"), 20, "{rows:#?}");
    let code = rows.iter().find(|row| row.contains("xxx")).unwrap();
    assert_eq!(UnicodeWidthStr::width(code.as_str()), 19 + 121);
}

#[test]
fn content_wider_than_the_pane_starts_at_its_edge() {
    let vault = TempVault::new("column-huge-code");
    let index = vault.index();
    let palette = palette();
    let mut ctx = Ctx::text(&palette, &index);
    ctx.reading_width = 100;
    let source = format!("```text\n{}\n```\n", "x".repeat(300));
    let lines = render_document(&markdown::parse(&source), &mut ctx, 160);
    assert_eq!(first_column_of(&text_of(&lines), "xxx"), 1);
}

#[test]
fn small_pictures_and_diagrams_stay_in_the_column() {
    let vault = TempVault::new("column-small-embeds");
    vault.write_bytes("dot.png", &png(400, 200));
    vault.write("Flow.mmd", FLOW);
    vault.write("Flow.excalidraw", SCENE);
    let index = vault.index();
    let palette = palette();
    let mut images = images(30);
    let mut ctx = drawing(&palette, &index, &mut images);
    ctx.reading_width = 100;
    let lines = render_document(
        &markdown::parse("![dot](dot.png)\n\n![[Flow.mmd]]\n\n![[Flow.excalidraw]]\n"),
        &mut ctx,
        160,
    );
    assert_eq!(ctx.pictures.len(), 2, "picture and drawing");
    for picture in &ctx.pictures {
        assert_eq!(picture.indent, 30, "{:?}", picture.size);
    }
    let rows = text_of(&lines);
    assert_eq!(first_column_of(&rows, "Vault"), 30 + 3, "{rows:#?}");
}

#[test]
fn column_layout_handles_narrow_and_unrestricted_panes() {
    let vault = TempVault::new("column-widths");
    let index = vault.index();
    let palette = palette();
    for (pane, measure, offset, wrap) in [
        (160, 100, 30, 100),
        (40, 100, 0, 40),
        (160, 0, 0, 160),
        (121, 100, 11, 100),
    ] {
        let mut ctx = Ctx::text(&palette, &index);
        ctx.reading_width = measure;
        let lines = render_document(&markdown::parse(&"word ".repeat(100)), &mut ctx, pane);
        for row in text_of(&lines) {
            assert_eq!(row.find("word"), Some(offset));
            assert!(row.trim().len() <= wrap);
        }
    }
}

#[test]
fn every_wide_block_uses_the_pane_independently_of_prose_width() {
    let vault = TempVault::new("all-breakouts");
    vault.write("Flow.excalidraw", SCENE);
    vault.write("Flow.mmd", FLOW);
    let index = vault.index();
    let palette = palette();
    for source in [
        "| Column | Other |\n|---|---|\n| value | value |".to_string(),
        "$$\\frac{a+b}{c+d}$$".to_string(),
        format!("```mermaid\n{FLOW}\n```"),
        "![[Flow.mmd]]".to_string(),
        "![[Flow.excalidraw]]".to_string(),
        format!("```rust\n{}\n```", "x".repeat(120)),
    ] {
        let blocks = markdown::parse(&source);
        let mut full = Ctx::text(&palette, &index);
        let expected = dedented(&render_document(&blocks, &mut full, 160));
        let mut measured = Ctx::text(&palette, &index);
        measured.reading_width = 40;
        assert_eq!(
            dedented(&render_document(&blocks, &mut measured, 160)),
            expected,
            "{source}"
        );
    }
}

#[test]
fn reading_width_changes_invalidate_the_cache_without_changing_note_content() {
    let vault = TempVault::new("column-cache");
    let source = "word ".repeat(100);
    vault.write("A.md", &source);
    let mut app = open(&vault, "A.md");
    app.focus_mode = true;
    screen_rows(&mut app);
    let measured = app.reading.lines.len();
    app.config.ui.reading_width = 0;
    let rows = screen_rows(&mut app);
    assert_eq!(
        rows.iter()
            .find(|r| r.contains("word"))
            .unwrap()
            .find("word"),
        Some(2)
    );
    assert!(app.reading.lines.len() < measured);
    assert_eq!(app.index.read(app.active_note().unwrap()).unwrap(), source);
}

fn palette() -> Palette {
    Palette::from(&presets::default_theme())
}

#[test]
fn the_reading_layout_is_reused_until_the_note_changes() {
    let vault = TempVault::new("reading-cache");
    vault.write("A.md", "first version\n");
    let mut app = App::new(vault.vault(), crate::config::Config::default()).expect("app");
    let id = app.index.id_of_rel("A.md").expect("indexed");
    app.open_note(id);
    let screen = |app: &mut App| -> String {
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, app))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    };

    assert!(screen(&mut app).contains("first version"));
    let laid_out = app.reading.lines.as_ptr();
    screen(&mut app);
    assert_eq!(
        app.reading.lines.as_ptr(),
        laid_out,
        "a redraw lays nothing out again"
    );

    // Changed on disk and indexed again, as the watcher does.
    vault.write("A.md", "the second version\n");
    app.index.refresh_note(id).expect("refresh");
    assert!(screen(&mut app).contains("the second version"));
}

#[test]
fn the_link_mark_sits_in_the_margin_beside_the_column_without_moving_the_text() {
    let palette = palette();
    let mut line = Line::from(vec![Span::raw(" ".repeat(10)), Span::raw("text with link")]);
    view::mark_selected_link(&mut line, &palette);
    let shown: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(shown, format!("{}★ text with link", " ".repeat(8)));
    assert_eq!(line.width(), 10 + "text with link".width());

    // No margin to put it in: the row keeps its text where it was.
    let mut flush = Line::from("text with link");
    view::mark_selected_link(&mut flush, &palette);
    let shown: String = flush.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(shown, "text with link");
    assert_eq!(flush.width(), "text with link".width());
    assert_eq!(flush.spans[0].style.bg, Some(palette.accent));
}

/// `]`/`[` pick a link; the reading view must mark that one, not the first.
#[test]
fn the_selected_link_is_marked_on_its_row() {
    let vault = TempVault::new("highlight-link");
    vault.write("X.md", "# X\n\n[[Alpha]]\n\n[[Beta]]\n\n[[Gamma]]\n");
    let mut app = App::new(vault.vault(), crate::config::Config::default()).expect("app");
    let id = app.index.id_of_rel("X.md").expect("indexed");
    app.open_note(id);
    app.active_mut().unwrap().selected_link = Some(1);

    let rows = screen_rows(&mut app);
    let beta = rows
        .iter()
        .find(|r| r.contains("Beta"))
        .expect("Beta on screen");
    assert!(
        beta.contains('★'),
        "second link's row should be marked, got: {beta}"
    );
    let alpha = rows
        .iter()
        .find(|r| r.contains("Alpha"))
        .expect("Alpha on screen");
    assert!(!alpha.contains('★'), "first link unmarked, got: {alpha}");
    let gamma = rows
        .iter()
        .find(|r| r.contains("Gamma"))
        .expect("Gamma on screen");
    assert!(!gamma.contains('★'), "third link unmarked, got: {gamma}");
}

/// Scrolling away from a selected link must stay scrolled away. The mark
/// follows the link back into view on the next `]`/`[`, not on a redraw.
#[test]
fn scrolling_away_from_a_selected_link_stays_away() {
    let vault = TempVault::new("scroll-away");
    let body = (0..200)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    vault.write("X.md", &format!("# X\n\n[[Alpha]]\n\n{body}\n"));
    let mut app = App::new(vault.vault(), crate::config::Config::default()).expect("app");
    let id = app.index.id_of_rel("X.md").expect("indexed");
    app.open_note(id);
    app.active_mut().unwrap().selected_link = Some(0);

    let _ = screen_rows(&mut app);
    let brought = app.active().unwrap().scroll;
    app.active_mut().unwrap().scroll = brought + 12;
    let away = app.active().unwrap().scroll;

    let _ = screen_rows(&mut app);
    assert_eq!(
        app.active().unwrap().scroll,
        away,
        "a redraw must not snap the scroll back to the selected link"
    );
}

/// What is on screen after drawing the app at a fixed size, one string per
/// row.
fn screen_rows(app: &mut App) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(200, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    buffer
        .content
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn screen_text(app: &mut App) -> String {
    screen_rows(app).join("\n")
}

fn braille_cells(rows: &[String]) -> usize {
    rows.iter()
        .flat_map(|row| row.chars())
        .filter(|c| ('\u{2801}'..='\u{28ff}').contains(c))
        .count()
}

fn row_with(rows: &[String], needle: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} on screen:\n{}", rows.join("\n")))
}

fn open(vault: &TempVault, note: &str) -> App {
    let mut app = App::new(vault.vault(), crate::config::Config::default()).expect("app");
    let id = app.index.id_of_rel(note).expect("indexed");
    app.open_note(id);
    app
}

const FLOW: &str = "flowchart LR\n  A[Vault] --> B{Index}\n  B --> C[Reader]";
const SCENE: &str = r##"{"type": "excalidraw", "elements": [
        {"type": "rectangle", "x": 0, "y": 0, "width": 240, "height": 96,
         "strokeColor": "#e03131"},
        {"type": "text", "x": 20, "y": 30, "width": 200, "height": 25,
         "text": "Ingest", "fontSize": 20}]}"##;

#[test]
fn a_mermaid_block_is_drawn_among_the_prose() {
    let vault = TempVault::new("mermaid-inline");
    vault.write(
        "A.md",
        &format!("Before\n\n```mermaid\n{FLOW}\n```\n\nAfter\n"),
    );
    let mut app = open(&vault, "A.md");

    let rows = screen_rows(&mut app);
    let shown = rows.join("\n");
    assert!(
        !shown.contains("flowchart LR"),
        "source is replaced:\n{shown}"
    );
    assert!(shown.contains('┌') && shown.contains('▶'), "{shown}");
    let (before, vault_row, after) = (
        row_with(&rows, "Before"),
        row_with(&rows, "Vault"),
        row_with(&rows, "After"),
    );
    assert!(before < vault_row && vault_row < after, "{shown}");
    assert!(rows[vault_row].contains("Index") && rows[vault_row].contains("Reader"));
}

#[test]
fn a_mermaid_block_that_cannot_be_drawn_keeps_its_source_and_says_why() {
    let vault = TempVault::new("mermaid-fallback");
    vault.write(
            "A.md",
            "```mermaid\nsequenceDiagram\n  A->>B: hello\n```\n\n```mermaid\nflowchart TD\n  X -->\n```\n",
        );
    let mut app = open(&vault, "A.md");

    let shown = screen_text(&mut app);
    assert!(shown.contains("A->>B: hello"), "the source stays:\n{shown}");
    assert!(
        shown.contains("sequenceDiagram diagrams are not drawn"),
        "with the reason above it:\n{shown}"
    );
    assert!(shown.contains("X -->"), "{shown}");
    assert!(shown.contains("an edge has no node at its end"), "{shown}");
}

#[test]
fn mermaid_files_open_as_their_drawing() {
    let vault = TempVault::new("mermaid-files");
    vault.write("Flow.mmd", FLOW);
    vault.write("Other.mermaid", "graph TD\n  Top --> Bottom");
    let mut app = open(&vault, "Flow.mmd");
    let shown = screen_text(&mut app);
    assert!(shown.contains("Vault") && shown.contains('▶'), "{shown}");
    assert!(!shown.contains("flowchart LR"), "{shown}");

    let mut other = open(&vault, "Other.mermaid");
    let rows = screen_rows(&mut other);
    assert!(row_with(&rows, "Top") < row_with(&rows, "Bottom"));
}

#[test]
fn a_mermaid_file_that_is_not_a_flowchart_shows_its_source() {
    let vault = TempVault::new("mermaid-file-source");
    vault.write("Seq.mmd", "sequenceDiagram\n  A->>B: hello\n");
    let mut app = open(&vault, "Seq.mmd");
    let shown = screen_text(&mut app);
    assert!(shown.contains("A->>B: hello"), "{shown}");
    assert!(shown.contains("not drawn"), "{shown}");
}

#[test]
fn excalidraw_files_open_as_a_braille_drawing() {
    let vault = TempVault::new("excalidraw-file");
    vault.write("Flow.excalidraw", SCENE);
    let mut app = open(&vault, "Flow.excalidraw");
    let rows = screen_rows(&mut app);
    assert!(braille_cells(&rows) > 20, "{}", rows.join("\n"));
    assert!(
        !rows.join("\n").contains("excalidraw\""),
        "not the JSON it is stored as"
    );
}

#[test]
fn a_drawing_embedded_in_a_note_is_traced_where_it_is_embedded() {
    let vault = TempVault::new("excalidraw-embed");
    vault.write("drawings/Flow.excalidraw", SCENE);
    vault.write(
        "A.md",
        "Look:\n\n![[Flow.excalidraw]]\n\nand\n\n![](drawings/Flow.excalidraw)\n\nEnd\n",
    );
    let mut app = open(&vault, "A.md");

    let rows = screen_rows(&mut app);
    let shown = rows.join("\n");
    assert_eq!(app.reading.pictures.len(), 2, "{shown}");
    assert!(
        app.reading
            .pictures
            .iter()
            .all(|picture| matches!(picture.source, PictureSource::Drawing(_)))
    );
    assert!(braille_cells(&rows) > 40, "{shown}");
    let (look, and, end) = (
        row_with(&rows, "Look:"),
        row_with(&rows, "and"),
        row_with(&rows, "End"),
    );
    assert!(
        look < and && and < end,
        "the prose keeps its place:\n{shown}"
    );
    // The ink is between the words, not over them.
    let ink_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| braille_cells(std::slice::from_ref(row)) > 0)
        .map(|(i, _)| i)
        .collect();
    assert!(
        ink_rows
            .iter()
            .all(|&row| row != look && row != and && row != end)
    );
    assert!(ink_rows.iter().any(|&row| look < row && row < and));
    assert!(ink_rows.iter().any(|&row| and < row && row < end));
}

#[test]
fn an_obsidian_drawing_note_embeds_by_either_name() {
    let vault = TempVault::new("excalidraw-md-embed");
    vault.write(
        "Flow.excalidraw.md",
        &format!("---\nexcalidraw-plugin: parsed\n---\n\n## Drawing\n```json\n{SCENE}\n```\n%%\n"),
    );
    vault.write("A.md", "![[Flow.excalidraw]]\n\n![[Flow.excalidraw.md]]\n");
    let mut app = open(&vault, "A.md");
    let rows = screen_rows(&mut app);
    assert_eq!(app.reading.pictures.len(), 2, "{}", rows.join("\n"));
    assert!(braille_cells(&rows) > 40);
}

#[test]
fn a_mermaid_file_embeds_as_its_drawing() {
    let vault = TempVault::new("mermaid-embed");
    vault.write("Flow.mmd", FLOW);
    vault.write("A.md", "Intro\n\n![[Flow.mmd]]\n\nOutro\n");
    let mut app = open(&vault, "A.md");
    let rows = screen_rows(&mut app);
    let shown = rows.join("\n");
    assert!(shown.contains('┌') && shown.contains("Reader"), "{shown}");
    assert!(row_with(&rows, "Intro") < row_with(&rows, "Vault"));
    assert!(row_with(&rows, "Vault") < row_with(&rows, "Outro"));
}

#[test]
fn an_embed_that_is_missing_or_unreadable_stays_a_link() {
    let vault = TempVault::new("embed-broken");
    vault.write("Bad.excalidraw", "this is not json");
    vault.write("Empty.excalidraw", r#"{"elements": []}"#);
    vault.write(
        "A.md",
        "![[Nope.excalidraw]]\n\n![[Bad.excalidraw]]\n\n![[Empty.excalidraw]]\n\n![[Nope.mmd]]\n",
    );
    let mut app = open(&vault, "A.md");
    let shown = screen_text(&mut app);
    assert!(app.reading.pictures.is_empty(), "{shown}");
    for name in [
        "Nope.excalidraw",
        "Bad.excalidraw",
        "Empty.excalidraw",
        "Nope.mmd",
    ] {
        assert!(shown.contains(name), "{name} is still there:\n{shown}");
    }
}

#[test]
fn an_embedded_mermaid_file_that_cannot_be_drawn_shows_its_source_and_why() {
    let vault = TempVault::new("embed-mermaid-fallback");
    vault.write("Seq.mermaid", "sequenceDiagram\n  A->>B: hello\n");
    vault.write("Broken.mmd", "flowchart TD\n  X -->\n");
    vault.write("A.md", "![[Seq.mermaid]]\n\nbetween\n\n![[Broken.mmd]]\n");
    let mut app = open(&vault, "A.md");
    let rows = screen_rows(&mut app);
    let shown = rows.join("\n");
    assert!(shown.contains("A->>B: hello"), "{shown}");
    assert!(
        shown.contains("sequenceDiagram diagrams are not drawn"),
        "{shown}"
    );
    assert!(shown.contains("X -->"), "{shown}");
    assert!(shown.contains("an edge has no node at its end"), "{shown}");
    assert!(
        row_with(&rows, "A->>B: hello") < row_with(&rows, "between")
            && row_with(&rows, "between") < row_with(&rows, "X -->"),
        "each in its place:\n{shown}"
    );
}

/// A diagram file's source is inert data, the way Obsidian and
/// render-markdown.nvim treat code blocks: a fence inside it closes
/// nothing, and an embed-looking line inside it is never followed.
#[test]
fn a_mermaid_file_that_closes_a_fence_shows_source_instead_of_reentering() {
    let vault = TempVault::new("mermaid-fenced");
    vault.write("Fenced.mmd", "flowchart TD\n```\n![[Fenced.mmd]]\n");
    let mut app = open(&vault, "Fenced.mmd");
    let shown = screen_text(&mut app);
    assert!(shown.contains("flowchart TD"), "the source stays:\n{shown}");
    assert!(
        shown.contains("```"),
        "the closing fence is source, not structure:\n{shown}"
    );
    assert!(shown.contains("![[Fenced.mmd]]"), "{shown}");
    assert!(shown.contains("mermaid:"), "and why:\n{shown}");
}

/// The same file embedded in a note: its source must not become an embed
/// of itself halfway through rendering.
#[test]
fn an_embedded_mermaid_file_with_a_closing_fence_shows_its_source() {
    let vault = TempVault::new("mermaid-fenced-embed");
    vault.write("Fenced.mmd", "flowchart TD\n```\n![[Fenced.mmd]]\n");
    vault.write("A.md", "before\n\n![[Fenced.mmd]]\n\nafter\n");
    let mut app = open(&vault, "A.md");
    let shown = screen_text(&mut app);
    assert!(
        shown.contains("![[Fenced.mmd]]"),
        "the inert source:\n{shown}"
    );
    assert!(shown.contains("mermaid:"), "and why:\n{shown}");
    assert!(
        shown.contains("before") && shown.contains("after"),
        "the prose survives:\n{shown}"
    );
}

/// Two diagram files whose sources name each other through closing fences
/// render once each instead of following the cycle.
#[test]
fn two_mermaid_files_naming_each_other_render_once_without_recursing() {
    let vault = TempVault::new("mermaid-cycle");
    vault.write("B.mmd", "flowchart TD\n```\n![[C.mmd]]\n");
    vault.write("C.mmd", "flowchart TD\n```\n![[B.mmd]]\n");
    vault.write("A.md", "before\n\n![[B.mmd]]\n\nafter\n");
    let mut app = open(&vault, "A.md");
    let shown = screen_text(&mut app);
    assert!(
        shown.contains("![[C.mmd]]"),
        "B's source is inert:\n{shown}"
    );
    assert!(
        shown.contains("before") && shown.contains("after"),
        "{shown}"
    );
}

/// A note that embeds itself gets Obsidian's placeholder rather than a
/// second rendering of itself.
#[test]
fn a_note_embedded_in_itself_says_so_instead_of_opening_itself() {
    let vault = TempVault::new("embed-self");
    vault.write("A.md", "text\n\n![[A.md]]\n\ndone\n");
    let mut app = open(&vault, "A.md");
    let shown = screen_text(&mut app);
    assert!(shown.contains("circular embed"), "placeholder:\n{shown}");
    assert!(shown.contains("text") && shown.contains("done"), "{shown}");
}

#[test]
fn an_embed_beyond_the_depth_limit_is_refused_as_too_deep() {
    let vault = TempVault::new("embed-depth");
    vault.write("Flow.excalidraw", SCENE);
    let index = vault.index();
    let palette = palette();
    let mut images = images(20);
    let mut ctx = drawing(&palette, &index, &mut images);
    ctx.embeds = (0..MAX_EMBED_DEPTH).map(|n| format!("N{n}.md")).collect();

    let shown = match embedded_diagram("Flow.excalidraw", &mut ctx, 60, "") {
        Some(Lift::Lines(lines)) => text_of(&lines).join("\n"),
        Some(Lift::Picture(..)) => panic!("past the depth limit it is not a picture"),
        None => panic!("past the depth limit it is not left as a bare link"),
    };
    assert!(shown.contains("too deep"), "{shown}");
}

#[test]
fn a_self_loop_is_never_a_diagram_that_quietly_lacks_its_loop() {
    let vault = TempVault::new("mermaid-self-loop");
    vault.write(
        "A.md",
        "```mermaid\nflowchart TD\n  A[Again] -->|retry| A\n```\n",
    );
    vault.write("Loop.mmd", "flowchart TD\n  A[Again] -->|retry| A\n");
    for note in ["A.md", "Loop.mmd"] {
        let mut app = open(&vault, note);
        let shown = screen_text(&mut app);
        assert!(
            shown.contains("A[Again] -->|retry| A"),
            "{note}: source:\n{shown}"
        );
        assert!(shown.contains("itself"), "{note}: and why:\n{shown}");
    }
}

#[test]
fn an_embedded_drawing_scrolls_off_the_top_without_spilling() {
    let vault = TempVault::new("excalidraw-scroll");
    vault.write("Flow.excalidraw", SCENE);
    vault.write(
        "A.md",
        &format!(
            "![[Flow.excalidraw]]\n\nAfter the drawing\n{}",
            "filler\n\n".repeat(60)
        ),
    );
    let mut app = open(&vault, "A.md");
    let rows = screen_rows(&mut app);
    let all_ink = braille_cells(&rows);
    let height = app.reading.pictures[0].size.height as usize;
    assert!(height >= 4, "tall enough to cut in half: {height}");

    app.active_mut().expect("a tab").scroll = height / 2;
    let rows = screen_rows(&mut app);
    let some_ink = braille_cells(&rows);
    assert!(
        some_ink > 0 && some_ink < all_ink,
        "half of it is still showing: {some_ink} of {all_ink}"
    );
    assert!(rows.join("\n").contains("After the drawing"));
}

#[test]
fn the_reason_a_mermaid_block_stays_source_wraps_instead_of_running_off_the_pane() {
    let vault = TempVault::new("mermaid-reason-wrap");
    let index = vault.index();
    let palette = palette();
    let document = markdown::parse("```mermaid\nsequenceDiagram\n  A->>B: hi\n```\n");
    let mut ctx = Ctx::text(&palette, &index);
    let rows = text_of(&render_document(&document, &mut ctx, 30));

    assert!(
        rows.iter().all(|row| row.chars().count() <= 30),
        "{rows:#?}"
    );
    let words: Vec<&str> = rows.iter().flat_map(|row| row.split_whitespace()).collect();
    let sentence = words.join(" ");
    assert!(
        sentence.contains(
            "mermaid: sequenceDiagram diagrams are not drawn yet, only flowchart and graph"
        ),
        "every word of it is still there: {sentence}"
    );
    assert!(sentence.contains("A->>B: hi"), "and so is the source");
}

#[test]
fn a_mermaid_diagram_in_a_quote_or_list_is_indented_with_it() {
    let vault = TempVault::new("mermaid-nested");
    let index = vault.index();
    let palette = palette();
    let document = markdown::parse("> ```mermaid\n> graph TD\n>   A --> B\n> ```\n");
    let mut ctx = Ctx::text(&palette, &index);
    let rows = text_of(&render_document(&document, &mut ctx, 60));
    let shown = rows.join("\n");
    assert!(shown.contains('┌') && shown.contains('▼'), "{shown}");
    assert!(
        rows.iter()
            .filter(|row| row.contains('┌') || row.contains('│'))
            .all(|row| row.starts_with(icons::QUOTE_BAR)),
        "every row of it sits behind the quote bar:\n{shown}"
    );
}

fn text_of(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn wrap_spans_preserves_styles_across_lines() {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let spans = vec![
        ("hello ".to_string(), bold),
        ("world again".to_string(), bold),
    ];
    let lines = wrap_spans(&spans, 11, "", "");

    assert!(lines.len() > 1, "should wrap");
    for line in &lines {
        for span in &line.spans {
            if !span.content.trim().is_empty() {
                assert!(span.style.add_modifier.contains(Modifier::BOLD));
            }
        }
    }
}

#[test]
fn wrap_spans_applies_a_hanging_indent() {
    let spans = vec![("one two three four".to_string(), Style::default())];
    let lines = wrap_spans(&spans, 10, "", "    ");
    let rendered = text_of(&lines);

    assert!(rendered.len() > 1);
    assert!(
        rendered[1].starts_with("    "),
        "continuation lines are indented: {rendered:?}"
    );
}

/// A real PNG of the given size, since the layout reads its header.
fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(width, height));
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encode png");
    bytes
}

/// One cell is 10x20 pixels, so a 400x200 picture is 40x10 cells.
///
/// The cap is a share of the pane, so a pane of `max_rows` rows with the
/// share set to all of it caps pictures at exactly that many rows.
fn images(max_rows: u16) -> Images {
    let mut images = Images::halfblocks(100);
    images.set_pane_height(max_rows);
    images
}

fn drawing<'a>(palette: &'a Palette, index: &'a VaultIndex, images: &'a mut Images) -> Ctx<'a> {
    Ctx {
        palette,
        index,
        note_dir: None,
        images: Some(images),
        pictures: Vec::new(),
        anchors: Vec::new(),
        embeds: Vec::new(),
        reading_width: 0,
        breakout: 0,
        links: 0,
    }
}

#[test]
fn a_picture_reserves_its_rows_where_the_text_leaves_off() {
    let vault = TempVault::new("render-image");
    vault.write_bytes("chart.png", &png(400, 200));
    let index = vault.index();
    let palette = palette();
    let mut images = images(20);

    let document = markdown::parse("Before\n\n![a chart](chart.png)\n\nAfter\n");
    let mut ctx = drawing(&palette, &index, &mut images);
    let lines = render_document(&document, &mut ctx, 60);

    let picture = match ctx.pictures.as_slice() {
        [picture] => picture,
        other => panic!("expected one picture, got {}", other.len()),
    };
    assert_eq!(
        (picture.size.width, picture.size.height),
        (40, 10),
        "400x200 pixels is 40x10 cells at this font size"
    );

    let rendered = text_of(&lines);
    assert!(
        rendered[picture.line..picture.line + 10]
            .iter()
            .all(|line| line.is_empty()),
        "the rows it will be drawn over are left blank: {rendered:?}"
    );
    let after = rendered
        .iter()
        .position(|line| line.contains("After"))
        .expect("text after the picture");
    assert!(
        after >= picture.line + 10,
        "text below it is pushed past its rows, so nothing moves when it loads"
    );
}

#[test]
fn a_picture_too_wide_for_the_pane_is_scaled_to_fit() {
    let vault = TempVault::new("render-image-wide");
    vault.write_bytes("wide.png", &png(4000, 1000));
    let index = vault.index();
    let palette = palette();
    let mut images = images(20);

    let mut ctx = drawing(&palette, &index, &mut images);
    let document = markdown::parse("![](wide.png)\n");
    render_document(&document, &mut ctx, 30);

    assert_eq!(ctx.pictures[0].size.width, 30, "bounded by the pane");
    assert_eq!(ctx.pictures[0].size.height, 4, "and keeps its proportions");
}

#[test]
fn a_tall_picture_stops_at_the_configured_row_cap() {
    let vault = TempVault::new("render-image-tall");
    vault.write_bytes("tall.png", &png(400, 4000));
    let index = vault.index();
    let palette = palette();
    let mut images = images(8);

    let mut ctx = drawing(&palette, &index, &mut images);
    render_document(&markdown::parse("![](tall.png)\n"), &mut ctx, 60);

    assert_eq!(
        ctx.pictures[0].size.height, 8,
        "a portrait photo shouldn't take several screens on its own"
    );
}

#[test]
fn a_picture_grows_with_the_pane_rather_than_stopping_at_a_fixed_height() {
    // A cap in rows that suits an 80x24 terminal leaves a diagram
    // unreadable on a full-screen one, which is what made pictures look
    // like mush regardless of how well they were resampled.
    let vault = TempVault::new("render-image-share");
    vault.write_bytes("tall.png", &png(400, 4000));
    let index = vault.index();
    let palette = palette();

    let height_in = |rows: u16| {
        let mut images = Images::halfblocks(50);
        images.set_pane_height(rows);
        let mut ctx = drawing(&palette, &index, &mut images);
        render_document(&markdown::parse("![](tall.png)\n"), &mut ctx, 60);
        ctx.pictures[0].size.height
    };

    assert_eq!(height_in(24), 12, "half of a short pane");
    assert_eq!(height_in(60), 30, "and half of a tall one");
}

#[test]
fn an_obsidian_embed_of_an_image_is_a_picture_but_of_a_note_is_not() {
    let vault = TempVault::new("render-embed");
    vault.write_bytes("assets/chart.png", &png(200, 100));
    vault.write("Other.md", "body\n");
    let index = vault.index();
    let palette = palette();
    let mut images = images(20);

    let mut ctx = drawing(&palette, &index, &mut images);
    // The picture is found by bare filename, from a subfolder, as Obsidian
    // resolves it.
    render_document(&markdown::parse("![[chart.png]]\n"), &mut ctx, 60);
    assert_eq!(ctx.pictures.len(), 1, "an image embed draws");

    let mut ctx = drawing(&palette, &index, &mut images);
    render_document(&markdown::parse("![[Other]]\n"), &mut ctx, 60);
    assert!(ctx.pictures.is_empty(), "a note embed is not a picture");
}

#[test]
fn an_obsidian_width_hint_shrinks_the_picture() {
    let vault = TempVault::new("render-image-hint");
    vault.write_bytes("chart.png", &png(400, 200));
    let index = vault.index();
    let palette = palette();
    let mut images = images(20);

    let mut ctx = drawing(&palette, &index, &mut images);
    render_document(&markdown::parse("![[chart.png]]\n"), &mut ctx, 60);
    assert_eq!(ctx.pictures[0].size.width, 40, "40 cells at its own size");

    // 200 pixels is 20 cells at this font.
    let mut ctx = drawing(&palette, &index, &mut images);
    render_document(&markdown::parse("![[chart.png|200]]\n"), &mut ctx, 60);
    assert_eq!(ctx.pictures[0].size.width, 20, "the hint wins");
    assert_eq!(ctx.pictures[0].size.height, 5, "and it keeps its shape");

    // A hint wider than the pane is still bounded by the pane.
    let mut ctx = drawing(&palette, &index, &mut images);
    render_document(&markdown::parse("![[chart.png|9000]]\n"), &mut ctx, 30);
    assert_eq!(ctx.pictures[0].size.width, 30);

    // Everything else after the pipe is an alias, not a size.
    let mut ctx = drawing(&palette, &index, &mut images);
    render_document(&markdown::parse("![[chart.png|a diagram]]\n"), &mut ctx, 60);
    assert_eq!(ctx.pictures[0].size.width, 40);
}

#[test]
fn a_picture_that_cannot_be_drawn_falls_back_to_its_alt_text() {
    let vault = TempVault::new("render-image-missing");
    vault.write_bytes("chart.png", &png(200, 100));
    let index = vault.index();
    let palette = palette();

    // Every reason a picture might not be drawable, and all of them should
    // leave readable text behind rather than a hole.
    let cases = [
        ("![a chart](chart.png)", "a terminal that cannot draw"),
        ("![gone](missing.png)", "a file that isn't there"),
        ("![remote](https://e.com/x.png)", "a picture on the web"),
    ];
    for (source, reason) in cases {
        let mut images = match reason {
            "a terminal that cannot draw" => Images::disabled(),
            _ => images(20),
        };
        let mut ctx = drawing(&palette, &index, &mut images);
        let lines = render_document(&markdown::parse(source), &mut ctx, 60);

        assert!(ctx.pictures.is_empty(), "nothing is reserved for {reason}");
        let rendered = text_of(&lines).join(" ");
        let alt = source
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(alt, _)| alt)
            .unwrap_or_default();
        assert!(
            rendered.contains(alt),
            "{reason} still reads as {alt:?}: {rendered:?}"
        );
    }
}

#[test]
fn a_picture_inside_a_quote_moves_with_the_quote() {
    let vault = TempVault::new("render-image-quote");
    vault.write_bytes("chart.png", &png(200, 100));
    let index = vault.index();
    let palette = palette();
    let mut images = images(20);

    let mut ctx = drawing(&palette, &index, &mut images);
    let lines = render_document(
        &markdown::parse("Intro\n\n> quoted\n> ![](chart.png)\n"),
        &mut ctx,
        60,
    );

    let picture = &ctx.pictures[0];
    assert!(
        picture.indent >= 2,
        "it sits to the right of the quote bar, not under it"
    );
    let rendered = text_of(&lines);
    assert!(
        rendered[picture.line]
            .trim_end()
            .ends_with(icons::QUOTE_BAR),
        "its first row is a quote bar and then blank: {:?}",
        rendered[picture.line]
    );
}

#[test]
fn headings_are_colored_by_level_and_underlined() {
    let vault = TempVault::new("render-heading");
    let index = vault.index();
    let palette = palette();
    let document = markdown::parse("# Title\n\n### Deep\n");

    let lines = render_document(&document, &mut Ctx::text(&palette, &index), 40);
    let rendered = text_of(&lines);

    assert!(rendered.iter().any(|l| l.contains("Title")));
    assert!(
        rendered.iter().any(|l| l.starts_with('─')),
        "H1 gets a rule under it: {rendered:?}"
    );
    assert!(rendered.iter().any(|l| l.contains("Deep")));
}

#[test]
fn unresolved_wikilinks_render_dimmed() {
    let vault = TempVault::new("render-link");
    vault.write("Real.md", "x");
    let index = vault.index();
    let palette = palette();

    let document = markdown::parse("See [[Real]] and [[Missing]].\n");
    let lines = render_document(&document, &mut Ctx::text(&palette, &index), 60);

    let styles: Vec<_> = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .filter(|s| s.content.contains("Real") || s.content.contains("Missing"))
        .map(|s| (s.content.to_string(), s.style.fg))
        .collect();

    let real = styles
        .iter()
        .find(|(t, _)| t.contains("Real"))
        .expect("Real");
    let missing = styles
        .iter()
        .find(|(t, _)| t.contains("Missing"))
        .expect("Missing");

    assert_eq!(real.1, Some(palette.link));
    assert_eq!(missing.1, Some(palette.link_unresolved));
}

#[test]
fn tasks_render_with_checkboxes_and_strike_completed_ones() {
    let vault = TempVault::new("render-task");
    let index = vault.index();
    let palette = palette();

    let document = markdown::parse("- [ ] todo\n- [x] done\n");
    let lines = render_document(&document, &mut Ctx::text(&palette, &index), 40);
    let rendered = text_of(&lines);

    assert!(rendered.iter().any(|l| l.contains(icons::TASK_TODO)));
    assert!(rendered.iter().any(|l| l.contains(icons::TASK_DONE)));

    let done_line = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.contains("done")))
        .expect("done line");
    assert!(
        done_line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::CROSSED_OUT))
    );
}

/// The style of the first span whose text contains `needle`.
fn style_of(lines: &[Line<'_>], needle: &str) -> Style {
    lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content.contains(needle))
        .unwrap_or_else(|| panic!("no span holds {needle:?}"))
        .style
}

#[test]
fn every_task_state_has_its_own_glyph_colour_and_style() {
    let vault = TempVault::new("task-states");
    let index = vault.index();
    for theme in ["obsidian-dark", "terminal", "catppuccin-latte"] {
        let p = Palette::from(&presets::builtin_by_name(theme).expect("theme"));
        // (state, glyph, colour of the glyph, struck through)
        let table = [
            (' ', icons::TASK_TODO, p.checkbox_todo, false),
            ('x', icons::TASK_DONE, p.checkbox_done, true),
            ('X', icons::TASK_DONE, p.checkbox_done, true),
            ('-', icons::TASK_CANCELLED, p.text_faint, true),
            ('>', icons::TASK_FORWARDED, p.text_info, false),
            ('<', icons::TASK_SCHEDULED, p.callout_tip, false),
            ('?', icons::TASK_QUESTION, p.callout_question, false),
            ('!', icons::TASK_IMPORTANT, p.callout_danger, false),
            ('/', icons::TASK_PROGRESS, p.text_warning, false),
            ('D', icons::TASK_OTHER, p.text_accent, false),
        ];
        let mut glyphs = Vec::new();
        let mut colours = Vec::new();
        for (state, glyph, colour, struck) in table {
            let source = format!("- [{state}] word\n");
            let lines = render_document(&markdown::parse(&source), &mut Ctx::text(&p, &index), 40);
            assert_eq!(lines.len(), 1, "{theme} {state:?}");

            let shown = glyph.to_string();
            let glyph_style = style_of(&lines, &shown);
            assert_eq!(
                glyph_style.fg,
                Some(colour),
                "{theme} {state:?} glyph colour"
            );
            let text = style_of(&lines, "word");
            assert_eq!(
                text.add_modifier.contains(Modifier::CROSSED_OUT),
                struck,
                "{theme} {state:?} struck"
            );
            let text_colour = if struck { p.text_faint } else { p.text_normal };
            assert_eq!(text.fg, Some(text_colour), "{theme} {state:?} text colour");
            glyphs.push(glyph);
            if state != 'X' {
                colours.push(format!("{colour:?}"));
            }
        }
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), 9, "{theme}: nine states, nine glyphs");
        colours.sort();
        colours.dedup();
        assert_eq!(colours.len(), 9, "{theme}: nine states, nine colours");

        let cancelled = render_document(
            &markdown::parse("- [-] gone\n- [x] done\n- [!] now\n"),
            &mut Ctx::text(&p, &index),
            40,
        );
        assert!(
            style_of(&cancelled, "gone")
                .add_modifier
                .contains(Modifier::DIM)
        );
        assert!(
            !style_of(&cancelled, "done")
                .add_modifier
                .contains(Modifier::DIM)
        );
        assert!(
            style_of(&cancelled, &icons::TASK_IMPORTANT.to_string())
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }
}

#[test]
fn task_glyphs_are_nerd_font_material_design_boxes_not_the_missing_ballot_box() {
    let all = [
        icons::TASK_TODO,
        icons::TASK_DONE,
        icons::TASK_CANCELLED,
        icons::TASK_FORWARDED,
        icons::TASK_SCHEDULED,
        icons::TASK_QUESTION,
        icons::TASK_IMPORTANT,
        icons::TASK_PROGRESS,
        icons::TASK_OTHER,
    ];
    for glyph in all {
        // nf-md lives in the supplementary private use area. `fc-list
        // ':charset=<hex>'` confirmed each one is in CaskaydiaMono Nerd Font.
        assert!(
            ('\u{f0001}'..='\u{f1af0}').contains(&glyph),
            "U+{:X} is not a Material Design glyph",
            u32::from(glyph)
        );
        assert!(!matches!(glyph, '\u{2610}'..='\u{2612}'));
    }
}

#[test]
fn hard_breaks_start_a_new_row() {
    let table: &[(&str, &[&str])] = &[
        ("one  \ntwo\n", &["one", "two"]),
        ("one\\\ntwo\n", &["one", "two"]),
        ("a<br>b\n", &["a", "b"]),
        ("a <br/> b\n", &["a", "b"]),
        ("a<br><br>b\n", &["a", "", "b"]),
        ("one\ntwo\n", &["one two"]),
        ("- a<br>b\n", &["• a", "  b"]),
        ("**a  \nb**\n", &["a", "b"]),
    ];
    for (source, expected) in table {
        assert_eq!(shown(source, 40), *expected, "{source:?}");
    }
    // A break is not a newline in a table cell, which is one row.
    let cell = shown("| a<br>b | c |\n|---|---|\n| 1 | 2 |\n", 40);
    assert!(cell.iter().any(|row| row.contains("a b")), "{cell:?}");
}

#[test]
fn kbd_is_a_dim_chip_mark_is_a_highlight_and_sub_sup_are_unicode() {
    let vault = TempVault::new("render-html");
    let index = vault.index();
    let p = palette();
    let lines = render_document(
        &markdown::parse(
            "Press <kbd>Ctrl</kbd> and <mark>this</mark>: H<sub>2</sub>O, m<sup>2</sup>.\n",
        ),
        &mut Ctx::text(&p, &index),
        60,
    );
    let row = text_of(&lines).join("\n").replace('\u{a0}', " ");
    assert!(row.contains("Press  Ctrl  and"), "{row:?}");
    assert!(row.contains("H₂O, m²."), "{row:?}");
    assert!(!row.contains('<'), "no tag shows: {row:?}");

    let chip = style_of(&lines, "\u{a0}Ctrl\u{a0}");
    assert_eq!(chip.fg, Some(p.text_muted), "a dim key");
    assert_eq!(chip.bg, Some(p.tag_bg), "on a chip");
    let mark = style_of(&lines, "this");
    assert_eq!(mark.bg, Some(p.text_highlight_bg), "as ==highlight== is");
    assert_eq!(mark.fg, Some(p.text_highlight_fg));
}

#[test]
fn other_html_shows_as_the_text_it_is() {
    let rows = shown("<span>a</span> <div>b</div> <details>c</details>\n", 60);
    assert_eq!(rows, ["<span>a</span> <div>b</div> <details>c</details>"]);
}

fn screen_rows_in(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    buffer
        .content
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
}

const SHOWCASE: &str = include_str!("showcase.md");
const SHOWCASE_ALPHA: &str = "# Alpha\n\nThe target. ^para1\n";

#[test]
fn the_showcase_note_reads_without_leaks_and_is_never_changed() {
    let vault = TempVault::new("showcase-hygiene");
    vault.write("Showcase.md", SHOWCASE);
    vault.write("Alpha.md", SHOWCASE_ALPHA);
    let mut app = open(&vault, "Showcase.md");
    let rows = screen_rows_in(&mut app, 120, 80);
    let screen = rows.join("\n");

    // Nothing of the syntax is left on the page.
    for leak in [
        "inline comment",
        "never shows",
        "runs over lines",
        "^item1",
        "<br",
        "<kbd>",
        "</kbd>",
        "<mark>",
        "<sub>",
        "<sup>",
        "***",
        "*both",
        "*__",
        "__*",
    ] {
        assert!(!screen.contains(leak), "{leak:?} leaks:\n{screen}");
    }
    // Comments hid text, not the words around them.
    assert!(screen.contains("Before after it."), "{screen}");
    assert!(
        screen.contains("Text before and text after it."),
        "{screen}"
    );
    // Code keeps what it holds: the only `%%` left are the two in code.
    assert!(screen.contains("%% stays in code"), "{screen}");
    assert!(screen.contains("%% not a comment here"), "{screen}");
    assert_eq!(screen.matches("%%").count(), 2, "{screen}");

    // Hard breaks are rows of their own.
    let at = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle:?}:\n{screen}"))
    };
    assert_eq!(at("break the line.") - at("Two trailing spaces"), 1);
    assert_eq!(
        at("breaks it too, and so does") - at("A trailing backslash"),
        1
    );
    assert_eq!(at("an HTML break") - at("breaks it too, and so does"), 1);
    assert_eq!(at("of either kind.") - at("an HTML break"), 1);

    // The block id is not shown, though a link to it is (as Obsidian does).
    let id_row = &rows[at("A paragraph you can link to.")];
    assert!(!id_row.contains('^'), "{id_row:?}");
    assert!(screen.contains("Alpha › ^para1"), "{screen}");

    // HTML with a rendering, and HTML without one.
    assert!(screen.contains("\u{a0}Ctrl\u{a0}"), "{screen}");
    assert!(
        screen.contains("Water is H₂O and an area is m²."),
        "{screen}"
    );
    assert!(screen.contains("Powers: 2¹⁰ is 1024."), "{screen}");
    assert!(screen.contains("x^(n+1) and a_(i)"), "{screen}");
    assert!(
        screen.contains("Tags stay text: <span>x</span> <b>y</b>."),
        "{screen}"
    );

    // Nine task glyphs, none of the ones the font lacks.
    for glyph in [
        icons::TASK_TODO,
        icons::TASK_DONE,
        icons::TASK_CANCELLED,
        icons::TASK_FORWARDED,
        icons::TASK_SCHEDULED,
        icons::TASK_QUESTION,
        icons::TASK_IMPORTANT,
        icons::TASK_PROGRESS,
        icons::TASK_OTHER,
    ] {
        assert!(screen.contains(glyph), "U+{:X} missing", u32::from(glyph));
    }
    assert!(!screen.contains(['\u{2610}', '\u{2611}', '\u{2612}']));

    // Reading is display only: no edit, nothing to save, the file as it was.
    assert_eq!(app.unsaved_count(), 0);
    assert_eq!(
        std::fs::read_to_string(vault.path().join("Showcase.md")).expect("note"),
        SHOWCASE
    );

    // The block id still names its paragraph, and the link to it still goes
    // to the note that holds it.
    let index = vault.index();
    assert!(index.resolve("Alpha").is_some());
    let mut alpha = open(&vault, "Alpha.md");
    let alpha_screen = screen_rows_in(&mut alpha, 80, 20).join("\n");
    assert!(alpha_screen.contains("The target."), "{alpha_screen}");
    assert!(!alpha_screen.contains("^para1"), "{alpha_screen}");
    assert_eq!(
        std::fs::read_to_string(vault.path().join("Alpha.md")).expect("note"),
        SHOWCASE_ALPHA
    );
}

#[test]
fn callouts_render_with_a_colored_bar_and_title() {
    let vault = TempVault::new("render-callout");
    let index = vault.index();
    let palette = palette();

    let document = markdown::parse("> [!warning] Careful\n> body\n");
    let lines = render_document(&document, &mut Ctx::text(&palette, &index), 40);
    let rendered = text_of(&lines);

    assert!(rendered[0].contains(icons::QUOTE_BAR));
    assert!(rendered[0].contains("Careful"));
    assert!(rendered.len() > 1, "the body renders too");
}

#[test]
fn a_callout_without_a_title_uses_its_kind() {
    let vault = TempVault::new("render-callout-2");
    let index = vault.index();
    let document = markdown::parse("> [!tip]\n> body\n");
    let lines = render_document(&document, &mut Ctx::text(&palette(), &index), 40);

    assert!(text_of(&lines)[0].contains("Tip"));
}

#[test]
fn tables_render_with_borders_and_fit_the_width() {
    let vault = TempVault::new("render-table");
    let index = vault.index();
    let document = markdown::parse("| A | B |\n|---|---|\n| 1 | 2 |\n");
    let lines = render_document(&document, &mut Ctx::text(&palette(), &index), 40);
    let rendered = text_of(&lines);

    assert!(rendered.iter().any(|l| l.contains('┌')));
    assert!(rendered.iter().any(|l| l.contains('│')));
    for line in &rendered {
        assert!(line.chars().count() <= 44, "{line:?} overflows");
    }
}

#[test]
fn a_wide_table_keeps_its_columns_rather_than_being_crushed() {
    // Eight columns divided between a pane this narrow used to leave three
    // characters each — a table that fits and says nothing. It is laid out
    // at its own width now and panned across instead.
    let vault = TempVault::new("render-wide-table");
    let index = vault.index();

    let header: Vec<String> = (0..8).map(|i| format!("column {i}")).collect();
    let cells: Vec<String> = (0..8).map(|i| format!("value number {i}")).collect();
    let source = format!(
        "| {} |\n|{}|\n| {} |\n",
        header.join(" | "),
        "---|".repeat(8),
        cells.join(" | ")
    );

    let document = markdown::parse(&source);
    let lines = render_document(&document, &mut Ctx::text(&palette(), &index), 40);
    let rendered = text_of(&lines);

    let row = rendered
        .iter()
        .find(|line| line.contains("value number 0"))
        .expect("the body row");
    for cell in &cells {
        assert!(
            row.contains(cell.as_str()),
            "{cell:?} was truncated: {row:?}"
        );
    }
    assert!(
        row.chars().count() > 40,
        "the table is wider than the pane, which is what makes panning necessary"
    );
}

#[test]
fn a_runaway_column_cannot_push_the_others_out_of_reach() {
    let vault = TempVault::new("render-essay-table");
    let index = vault.index();
    let essay = "e".repeat(300);
    let source = format!("| A | B |\n|---|---|\n| {essay} | end |\n");

    let lines = render_document(
        &markdown::parse(&source),
        &mut Ctx::text(&palette(), &index),
        40,
    );
    let row = text_of(&lines)
        .into_iter()
        .find(|line| line.contains('e'))
        .expect("the body row");
    assert!(
        row.contains("end"),
        "the last column is still reachable: {row:?}"
    );
    assert!(row.chars().count() < 300, "the essay cell was capped");
}

#[test]
fn code_blocks_highlight_keywords() {
    let palette = palette();
    let spans = highlight("let x = 1;", "rust", &palette, Style::default());

    let keyword = spans
        .iter()
        .find(|s| s.content == "let")
        .expect("keyword span");
    assert_eq!(keyword.style.fg, Some(palette.syn_keyword));

    let number = spans
        .iter()
        .find(|s| s.content == "1")
        .expect("number span");
    assert_eq!(number.style.fg, Some(palette.syn_number));
}

#[test]
fn comments_and_strings_are_highlighted() {
    let palette = palette();

    let comment = highlight("# a note", "python", &palette, Style::default());
    assert_eq!(comment[0].style.fg, Some(palette.syn_comment));

    let string = highlight("x = \"hi\"", "python", &palette, Style::default());
    assert!(
        string
            .iter()
            .any(|s| s.style.fg == Some(palette.syn_string) && s.content.contains("hi"))
    );
}

#[test]
fn an_unknown_language_still_renders() {
    let palette = palette();
    let spans = highlight("some text", "brainfuck", &palette, Style::default());
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(joined, "some text");
}

#[test]
fn frontmatter_renders_as_a_metadata_header() {
    let vault = TempVault::new("render-fm");
    let index = vault.index();
    let document = markdown::parse("---\ntitle: T\ntags: [a]\n---\nbody\n");
    let lines = render_document(&document, &mut Ctx::text(&palette(), &index), 40);
    let rendered = text_of(&lines);

    assert!(rendered.iter().any(|l| l.contains("title: T")));
    assert!(rendered.iter().any(|l| l.contains("tags: a")));
    assert!(rendered.iter().any(|l| l.contains("body")));
}

#[test]
fn rendering_never_produces_lines_wider_than_the_pane() {
    let vault = TempVault::new("render-width");
    let index = vault.index();
    let long = "word ".repeat(60);
    let document = markdown::parse(&format!("{long}\n\n- {long}\n\n> {long}\n"));

    for width in [20usize, 40, 80] {
        let lines = render_document(&document, &mut Ctx::text(&palette(), &index), width);
        for line in text_of(&lines) {
            assert!(
                line.chars().count() <= width + 4,
                "width {width}: {:?} is {} chars",
                line,
                line.chars().count()
            );
        }
    }
}

/// The widest rendered row, in display columns, ignoring trailing space.
fn widest(lines: &[String]) -> usize {
    lines
        .iter()
        .map(|line| line.trim_end().width())
        .max()
        .unwrap_or(0)
}

#[test]
fn a_long_heading_wraps_instead_of_running_off_the_pane() {
    // One line of an exported chat, pasted as a single heading. It used to
    // be drawn as one row, so everything past the first few words — maths
    // included — was off the edge of the pane.
    let rows = shown(&format!("# {}\n", "a long heading ".repeat(20)), 40);
    assert!(rows.len() > 5, "{rows:?}");
    assert!(widest(&rows) <= 40, "{rows:?}");
    let rule = rows.iter().position(|row| row.starts_with('─'));
    assert_eq!(
        rule,
        Some(rows.len() - 1),
        "the rule goes under the last row"
    );
}

#[test]
fn maths_in_a_heading_is_typeset_and_display_maths_set_apart() {
    let rows = shown(
        "# Such as $\\pi$, and $$ \\pi = 4\\arctan(1) $$ which takes a few symbols\n",
        40,
    );
    assert!(rows[0].starts_with("Such as π,"), "{rows:?}");
    assert!(
        rows.iter().any(|row| row.trim() == "π = 4 arctan(1)"),
        "the display maths has a row of its own: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.starts_with("which takes")),
        "{rows:?}"
    );
}

#[test]
fn a_word_longer_than_the_pane_is_broken_at_its_edge() {
    let url = "https://docs.example.com/r/en-US/a-very-long-path/Floating-Point-Numerical-Formats";
    let rows = shown(&format!("See {url} for more.\n"), 30);
    assert!(widest(&rows) <= 30, "{rows:?}");
    let joined: String = rows.concat().split_whitespace().collect();
    assert_eq!(
        joined,
        format!("See{url}formore."),
        "nothing is lost along the way"
    );
}

#[test]
fn wide_characters_wrap_by_the_columns_they_take() {
    let rows = shown(&format!("{}\n", "日本語のテキスト ".repeat(6)), 20);
    assert!(widest(&rows) <= 20, "{rows:?}");
}

#[test]
fn a_callout_title_and_a_frontmatter_value_wrap() {
    let title = "a callout title that is far too long for the pane ";
    let rows = shown(&format!("> [!note] {title}\n> body\n"), 24);
    assert!(widest(&rows) <= 24, "{rows:?}");
    assert!(rows.len() > 3, "the title takes several rows: {rows:?}");

    let rows = shown(
        &format!("---\nsummary: {}\n---\nbody\n", "words ".repeat(20)),
        30,
    );
    assert!(widest(&rows) <= 30, "{rows:?}");
    assert!(
        rows[1].starts_with("  "),
        "continued under the value: {rows:?}"
    );
}

#[test]
fn wrapping_inside_an_indent_wider_than_the_pane_still_ends() {
    let spans = [("abcdef".to_string(), Style::default())];
    let lines = wrap_spans(&spans, 2, "", "    ");
    assert!(lines.len() >= 6, "one character a row, without looping");
}

/// Rendered rows with trailing space trimmed and non-breaking spaces shown
/// as the plain ones they look like.
fn shown(source: &str, width: usize) -> Vec<String> {
    let vault = TempVault::new("render-maths");
    let index = vault.index();
    let lines = render_document(
        &markdown::parse(source),
        &mut Ctx::text(&palette(), &index),
        width,
    );
    text_of(&lines)
        .into_iter()
        .map(|line| line.replace('\u{a0}', " ").trim_end().to_string())
        .collect()
}

#[test]
fn inline_maths_is_typeset_in_the_line() {
    assert_eq!(
        shown("Energy is $E=mc^2$, roughly.\n", 60),
        ["Energy is E = mc², roughly."]
    );
    // A price is still a price.
    assert_eq!(shown("From $5 to $10.\n", 60), ["From $5 to $10."]);
}

#[test]
fn inline_maths_is_drawn_in_the_maths_colour() {
    let vault = TempVault::new("render-maths-colour");
    let index = vault.index();
    let palette = palette();
    let lines = render_document(
        &markdown::parse("a $x$ b\n"),
        &mut Ctx::text(&palette, &index),
        40,
    );
    let maths = lines[0]
        .spans
        .iter()
        .find(|span| span.content == "x")
        .expect("the maths has a span of its own");
    assert_eq!(maths.style.fg, Some(palette.text_accent));
}

#[test]
fn a_display_equation_is_centred_on_rows_of_its_own() {
    assert_eq!(
        shown("Before\n\n$$\n\\frac{a+b}{2}\n$$\n\nAfter\n", 21),
        [
            "Before",
            "",
            "        a + b",
            "        ─────",
            "          2",
            "",
            "After"
        ]
    );
}

#[test]
fn display_maths_in_the_middle_of_a_paragraph_is_set_apart() {
    assert_eq!(
        shown("so that $$x^2 = 4$$ which means\n", 20),
        ["so that", "       x² = 4", "which means"]
    );
}

#[test]
fn an_equation_wider_than_the_pane_starts_at_the_margin() {
    let tex = "a + b + c + d + e + f + g + h";
    let rows = shown(&format!("$${tex}$$\n"), 10);
    assert_eq!(rows, [tex], "panned across rather than wrapped");
}

#[test]
fn an_equation_number_sits_at_the_right_margin() {
    assert_eq!(
        shown("$$E = mc^2 \\tag{1}$$\n", 30),
        ["           E = mc²         (1)"]
    );
}

#[test]
fn maths_in_headings_and_tables_is_typeset_too() {
    let rows = shown(
        "# The $\\alpha$ case\n\n| a | b |\n|---|---|\n| $x^2$ | $\\hat{y}$ |\n",
        40,
    );
    assert!(rows.contains(&"The α case".to_string()), "{rows:?}");
    // The combining hat takes no room, so the borders still line up.
    let widths: Vec<usize> = rows
        .iter()
        .filter(|row| row.starts_with(['┌', '│', '└']))
        .map(|row| row.width())
        .collect();
    assert!(widths.windows(2).all(|pair| pair[0] == pair[1]), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("x²")), "{rows:?}");
}
