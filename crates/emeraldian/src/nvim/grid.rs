//! Model of nvim's grid 1 for `ext_linegrid` UI redraw events.
//!
//! `Grid::apply` consumes one `redraw` notification's params (an array of
//! events) and updates the cells, highlight table, default colours, cursor and
//! mode. Pure data: no I/O, no rendering, no channels, so it can be unit-tested
//! against recorded batches.
//!
//! Protocol: `:h ui-linegrid` in nvim's `api-ui-events.txt`. Only grid 1 is
//! modelled; events for other grids are ignored until `ext_multigrid` is used.

use rmpv::Value;
use std::collections::HashMap;

/// The global grid: the whole screen while `ext_multigrid` is off.
const MAIN_GRID: i64 = 1;

/// One cell of the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Cell contents. A double-width character sits in this cell and the cell
    /// to its right holds `""` (the protocol's continuation marker), so a
    /// row's texts concatenate into the visible line.
    pub text: String,
    /// Highlight id from [`Grid::hl`]; `0` is the default highlight.
    pub hl: u64,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            text: " ".to_string(),
            hl: 0,
        }
    }
}

/// Default foreground, background and special colours (`default_colors_set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub fg: u32,
    pub bg: u32,
    pub sp: u32,
}

impl Default for Colors {
    /// White on black, only until nvim's `default_colors_set` arrives at attach.
    fn default() -> Self {
        Colors {
            fg: 0x00FF_FFFF,
            bg: 0x0000_0000,
            sp: 0x00FF_FFFF,
        }
    }
}

/// One entry of the highlight table (`hl_attr_define`). `None` colours mean
/// "use the default", not black: nvim may change its defaults later.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HlAttr {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub sp: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub strikethrough: bool,
    pub reverse: bool,
}

/// Cursor shape for a mode (`'guicursor'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    /// Insert mode's bar.
    Vertical,
    /// Replace mode's underline.
    Horizontal,
}

/// The mode nvim is in, as far as the cursor is concerned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModeInfo {
    pub name: String,
    pub cursor: CursorShape,
}

/// Model of nvim's grid 1. Pure data, fed by [`Grid::apply`].
#[derive(Debug, Clone)]
pub struct Grid {
    pub width: u16,
    pub height: u16,
    /// Row-major, `width * height` cells.
    pub cells: Vec<Cell>,
    /// Cursor as `(column, row)`.
    pub cursor: (u16, u16),
    pub hl: HashMap<u64, HlAttr>,
    pub default: Colors,
    pub mode: ModeInfo,
    /// `busy_start`/`busy_stop`: hide the cursor. Despite the name the events
    /// say nothing about loading — that is what `:h ui-event-busy_start` warns
    /// about — so the renderer only uses this to suppress cursor drawing.
    pub busy: bool,
    /// `mode_info_set` table; `mode_change` indexes into it.
    modes: Vec<ModeInfo>,
    /// Index into `modes` from the last `mode_change`.
    mode_index: usize,
}

impl Grid {
    pub fn new(width: u16, height: u16) -> Grid {
        Grid {
            width,
            height,
            cells: vec![Cell::default(); width as usize * height as usize],
            cursor: (0, 0),
            hl: HashMap::new(),
            default: Colors::default(),
            mode: ModeInfo::default(),
            busy: false,
            modes: Vec::new(),
            mode_index: 0,
        }
    }

    /// Apply one `redraw` notification's events, in order. Returns `true` when
    /// the batch contains a `flush`, i.e. the grid is ready to be drawn.
    ///
    /// A batch element is `[name, [params], [params], ...]`: nvim writes the
    /// event name once and then one parameter tuple per event, so consecutive
    /// events of the same kind share it.
    pub fn apply(&mut self, batch: &[Value]) -> bool {
        let mut flushed = false;
        for event in batch {
            let Some(parts) = event.as_array() else {
                continue;
            };
            let Some(name) = parts.first().and_then(Value::as_str) else {
                continue;
            };
            let params = &parts[1..];
            if params.is_empty() {
                // Zero-argument event, e.g. `["flush"]`.
                flushed |= self.event(name, &[]);
                continue;
            }
            for args in params {
                if let Some(args) = args.as_array() {
                    flushed |= self.event(name, args);
                }
            }
        }
        flushed
    }

    /// Apply one event with its own parameter list; `true` for `flush`.
    fn event(&mut self, name: &str, args: &[Value]) -> bool {
        match name {
            "grid_line" => self.grid_line(args),
            "grid_scroll" => self.grid_scroll(args),
            "grid_clear" => self.grid_clear(args),
            "grid_resize" => self.grid_resize(args),
            "grid_cursor_goto" => self.cursor_goto(args),
            "hl_attr_define" => self.hl_attr_define(args),
            "default_colors_set" => self.default_colors_set(args),
            "mode_info_set" => self.mode_info_set(args),
            "mode_change" => self.mode_change(args),
            "busy_start" => self.busy = true,
            "busy_stop" => self.busy = false,
            "flush" => return true,
            // Everything else (popupmenu, msg_show, win_viewport, ...) is
            // either irrelevant here or handled by later tasks.
            _ => {}
        }
        false
    }

    /// `grid_scroll`: copy a rectangle of cells inside a scroll region.
    /// `rows > 0` moves content up, `rows < 0` moves it down. The area that
    /// scrolls in stays stale; nvim sends `grid_line` for it right afterwards,
    /// so clearing it here would only flicker.
    fn grid_scroll(&mut self, args: &[Value]) {
        if !main_grid(args) {
            return;
        }
        let (Some(top), Some(bottom), Some(left), Some(right)) =
            (num(args, 1), num(args, 2), num(args, 3), num(args, 4))
        else {
            return;
        };
        let Some(rows) = args.get(5).and_then(Value::as_i64) else {
            return;
        };
        // `cols` is always 0 in this nvim version; left in the protocol only.
        let bottom = bottom.min(self.height);
        let right = right.min(self.width);
        if top >= bottom || left >= right || rows == 0 {
            return;
        }
        let (width, region_h, region_w) = (
            self.width as usize,
            (bottom - top) as usize,
            (right - left) as usize,
        );
        let step = (rows.unsigned_abs() as usize).min(region_h);
        // Source and destination overlap, so the copy direction matters:
        // moving up reads rows below the write position (ascending is safe),
        // moving down reads rows above it, so it has to run bottom-up.
        for i in 0..region_h - step {
            let dst_row = if rows > 0 { i } else { region_h - step - 1 - i };
            let (dst, src) = if rows > 0 {
                (dst_row, dst_row + step)
            } else {
                (dst_row + step, dst_row)
            };
            for col in 0..region_w {
                let src_index = (top as usize + src) * width + left as usize + col;
                let dst_index = (top as usize + dst) * width + left as usize + col;
                self.cells[dst_index] = self.cells[src_index].clone();
            }
        }
    }

    /// `grid_clear`: blank every cell. The highlight table and the cursor stay:
    /// nvim redefines highlights as needed and sends `grid_cursor_goto` itself.
    fn grid_clear(&mut self, args: &[Value]) {
        if !main_grid(args) {
            return;
        }
        self.cells.fill(Cell::default());
    }

    /// `grid_resize`: resize, keeping the content that still fits. A grid that
    /// shrinks below the cursor must pull the cursor back inside, or later
    /// draws would index out of the cells.
    fn grid_resize(&mut self, args: &[Value]) {
        if !main_grid(args) {
            return;
        }
        let (Some(width), Some(height)) = (num(args, 1), num(args, 2)) else {
            return;
        };
        if width == self.width && height == self.height {
            return;
        }
        let mut cells = vec![Cell::default(); width as usize * height as usize];
        let copy_width = self.width.min(width) as usize;
        for row in 0..self.height.min(height) as usize {
            let src = row * self.width as usize;
            let dst = row * width as usize;
            cells[dst..dst + copy_width].clone_from_slice(&self.cells[src..src + copy_width]);
        }
        self.width = width;
        self.height = height;
        self.cells = cells;
        self.cursor = (
            self.cursor.0.min(width.saturating_sub(1)),
            self.cursor.1.min(height.saturating_sub(1)),
        );
    }

    /// `grid_cursor_goto`: `(column, row)` of the visible cursor.
    fn cursor_goto(&mut self, args: &[Value]) {
        if !main_grid(args) {
            return;
        }
        let (Some(row), Some(column)) = (num(args, 1), num(args, 2)) else {
            return;
        };
        self.cursor = (column, row);
    }

    /// `hl_attr_define`: replace the entry for `id` with the RGB attributes.
    /// The 256-colour dict and the `hlstate` info array are not needed because
    /// the UI was attached with `rgb: true`.
    fn hl_attr_define(&mut self, args: &[Value]) {
        let (Some(id), Some(attrs)) = (
            args.first().and_then(Value::as_u64),
            args.get(1).and_then(Value::as_map),
        ) else {
            return;
        };
        let attr = HlAttr {
            fg: color(attrs, "foreground"),
            bg: color(attrs, "background"),
            sp: color(attrs, "special"),
            bold: flag(attrs, "bold"),
            italic: flag(attrs, "italic"),
            underline: flag(attrs, "underline"),
            undercurl: flag(attrs, "undercurl"),
            strikethrough: flag(attrs, "strikethrough"),
            reverse: flag(attrs, "reverse"),
        };
        self.hl.insert(id, attr);
    }

    /// `default_colors_set`: `-1` means "unset" (`ext_termcolors`), so those
    /// values stay as they are instead of turning black.
    fn default_colors_set(&mut self, args: &[Value]) {
        let pick = |index: usize| {
            args.get(index)
                .and_then(Value::as_i64)
                .and_then(|value| u32::try_from(value).ok())
        };
        if let Some(fg) = pick(0) {
            self.default.fg = fg;
        }
        if let Some(bg) = pick(1) {
            self.default.bg = bg;
        }
        if let Some(sp) = pick(2) {
            self.default.sp = sp;
        }
    }

    /// `mode_info_set`: the `'guicursor'` table that `mode_change` indexes.
    fn mode_info_set(&mut self, args: &[Value]) {
        let Some(modes) = args.get(1).and_then(Value::as_array) else {
            return;
        };
        self.modes = modes.iter().map(mode_from).collect();
        // A mode_change may have arrived first; re-derive the shape now.
        self.refresh_mode();
    }

    /// `mode_change`: the current mode's name and the table index it uses.
    fn mode_change(&mut self, args: &[Value]) {
        let Some(name) = args.first().and_then(Value::as_str) else {
            return;
        };
        if let Some(index) = args.get(1).and_then(Value::as_u64) {
            self.mode_index = index as usize;
        }
        self.mode.name = name.to_string();
        self.refresh_mode();
    }

    /// Take the cursor shape from the `mode_info_set` entry `mode_index`.
    fn refresh_mode(&mut self) {
        if let Some(mode) = self.modes.get(self.mode_index) {
            self.mode.cursor = mode.cursor;
        }
    }

    /// `grid_line`: a run of cells in one row. `hl_id` sticks to the following
    /// cells of the same event when omitted; `repeat` counts the cell itself.
    fn grid_line(&mut self, args: &[Value]) {
        if !main_grid(args) {
            return;
        }
        let (Some(row), Some(start)) = (num(args, 1), num(args, 2)) else {
            return;
        };
        let Some(cells) = args.get(3).and_then(Value::as_array) else {
            return;
        };
        let (row, mut column) = (row as usize, start as usize);
        if row >= self.height as usize {
            return;
        }
        let row_start = row * self.width as usize;
        // The protocol always sends hl_id for the first cell of the event.
        let mut hl = 0;
        for cell in cells {
            let Some(cell) = cell.as_array() else {
                continue;
            };
            let Some(text) = cell.first().and_then(Value::as_str) else {
                continue;
            };
            if let Some(id) = cell.get(1).and_then(Value::as_u64) {
                hl = id;
            }
            let repeat = cell.get(2).and_then(Value::as_u64).unwrap_or(1).max(1);
            for _ in 0..repeat {
                if column >= self.width as usize {
                    break;
                }
                self.cells[row_start + column] = Cell {
                    text: text.to_string(),
                    hl,
                };
                column += 1;
            }
        }
    }
}

/// Whether the event addresses grid 1.
fn main_grid(args: &[Value]) -> bool {
    args.first().and_then(Value::as_i64) == Some(MAIN_GRID)
}

/// Colour from an attribute map. Absent (or a negative sentinel) means "use
/// the default colour", which is why this returns `Option`.
fn color(attrs: &[(Value, Value)], key: &str) -> Option<u32> {
    attrs
        .iter()
        .find(|(name, _)| name.as_str() == Some(key))
        .and_then(|(_, value)| value.as_i64())
        .and_then(|value| u32::try_from(value).ok())
}

/// Boolean attribute; nvim only sends the keys that are true.
fn flag(attrs: &[(Value, Value)], key: &str) -> bool {
    attrs
        .iter()
        .find(|(name, _)| name.as_str() == Some(key))
        .and_then(|(_, value)| value.as_bool())
        .unwrap_or(false)
}

/// One `mode_info_set` entry.
fn mode_from(entry: &Value) -> ModeInfo {
    let Some(attrs) = entry.as_map() else {
        return ModeInfo::default();
    };
    let name = attrs
        .iter()
        .find(|(key, _)| key.as_str() == Some("name"))
        .and_then(|(_, value)| value.as_str())
        .unwrap_or_default()
        .to_string();
    let cursor = match attrs
        .iter()
        .find(|(key, _)| key.as_str() == Some("cursor_shape"))
        .and_then(|(_, value)| value.as_str())
    {
        Some("vertical") => CursorShape::Vertical,
        Some("horizontal") => CursorShape::Horizontal,
        _ => CursorShape::Block,
    };
    ModeInfo { name, cursor }
}

/// Unsigned parameter of an event, `None` when missing or out of range.
fn num(parts: &[Value], index: usize) -> Option<u16> {
    parts
        .get(index)
        .and_then(Value::as_i64)
        .and_then(|n| u16::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmpv::Value;

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    fn int(n: i64) -> Value {
        Value::from(n)
    }

    fn text(s: &str) -> Value {
        Value::from(s)
    }

    /// One element of a `redraw` batch. nvim writes the event name once and
    /// then one parameter tuple per event, so consecutive events of the same
    /// kind share a name.
    fn ev(name: &str, params: Vec<Vec<Value>>) -> Value {
        let mut items = vec![text(name)];
        items.extend(params.into_iter().map(Value::Array));
        arr(items)
    }

    /// A batch element carrying a single event.
    fn one(name: &str, params: Vec<Value>) -> Value {
        ev(name, vec![params])
    }

    /// A msgpack map from string keys, as nvim sends its attribute dicts.
    fn map(items: &[(&str, Value)]) -> Value {
        Value::Map(items.iter().map(|(k, v)| (text(k), v.clone())).collect())
    }

    /// The row's cell texts joined, as a test would read them off the screen.
    fn row_text(grid: &Grid, row: usize) -> String {
        let width = grid.width as usize;
        grid.cells[row * width..(row + 1) * width]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    fn hl_row(grid: &Grid, row: usize) -> Vec<u64> {
        let width = grid.width as usize;
        grid.cells[row * width..(row + 1) * width]
            .iter()
            .map(|cell| cell.hl)
            .collect()
    }

    /// Text of one cell, for assertions about position rather than whole rows.
    fn cell(grid: &Grid, row: usize, col: usize) -> &str {
        &grid.cells[row * grid.width as usize + col].text
    }

    /// `grid_line` writing `s` at `col`, one cell per character, default hl.
    fn line(row: i64, col: i64, s: &str) -> Value {
        let cells = s
            .chars()
            .map(|ch| arr(vec![text(&ch.to_string()), int(0)]))
            .collect();
        one(
            "grid_line",
            vec![int(1), int(row), int(col), arr(cells), Value::from(false)],
        )
    }

    fn scroll(rows: i64) -> Value {
        one(
            "grid_scroll",
            vec![int(1), int(0), int(5), int(0), int(10), int(rows), int(0)],
        )
    }

    fn cursor_goto(row: i64, col: i64) -> Value {
        one("grid_cursor_goto", vec![int(1), int(row), int(col)])
    }

    fn resize(width: i64, height: i64) -> Value {
        one("grid_resize", vec![int(1), int(width), int(height)])
    }

    /// 12x5 grid with `r<row>` in the scroll region and `x` outside it, at
    /// column 11.
    fn scrolled_candidate() -> Grid {
        let mut grid = Grid::new(12, 5);
        let mut batch = Vec::new();
        for row in 0..5 {
            batch.push(line(row, 0, &format!("r{row}")));
            batch.push(line(row, 11, "x"));
        }
        grid.apply(&batch);
        grid
    }

    #[test]
    fn grid_line_repeat_and_hl_carry() {
        let mut grid = Grid::new(10, 2);
        // Two grid_line events under one name, as nvim groups them.
        let batch = vec![ev(
            "grid_line",
            vec![
                vec![
                    int(1),
                    int(0),
                    int(0),
                    arr(vec![
                        arr(vec![text("a"), int(1)]),
                        arr(vec![text("b")]),
                        arr(vec![text(" "), int(2), int(3)]),
                    ]),
                    Value::from(false),
                ],
                vec![
                    int(1),
                    int(1),
                    int(0),
                    arr(vec![arr(vec![text("x"), int(4), int(2)])]),
                    Value::from(false),
                ],
            ],
        )];

        assert!(!grid.apply(&batch), "no flush in this batch");
        // "b" has no hl_id, so it keeps 1; the space repeats three times with 2.
        assert_eq!(row_text(&grid, 0), "ab   ".to_string() + &" ".repeat(5));
        assert_eq!(hl_row(&grid, 0), vec![1, 1, 2, 2, 2, 0, 0, 0, 0, 0]);
        // `repeat` counts the cell itself, so "x" covers two columns.
        assert_eq!(row_text(&grid, 1), "xx".to_string() + &" ".repeat(8));
        assert_eq!(hl_row(&grid, 1), vec![4, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn grid_scroll_up_region() {
        let mut grid = scrolled_candidate();
        grid.apply(&[scroll(2)]);

        // Content moves up by two rows inside cols 0..10.
        assert_eq!((cell(&grid, 0, 0), cell(&grid, 0, 1)), ("r", "2"));
        assert_eq!(cell(&grid, 1, 1), "3");
        assert_eq!(cell(&grid, 2, 1), "4");
        // The bottom two rows of the region are stale: nvim refills the
        // scrolled-in area with grid_line right after the scroll.
        assert_eq!((cell(&grid, 3, 0), cell(&grid, 3, 1)), ("r", "3"));
        assert_eq!((cell(&grid, 4, 0), cell(&grid, 4, 1)), ("r", "4"));
        // Column 11 is outside the region.
        for row in 0..5 {
            assert_eq!(cell(&grid, row, 11), "x", "row {row} outside the region");
        }
    }

    #[test]
    fn grid_scroll_down_region() {
        let mut grid = scrolled_candidate();
        grid.apply(&[scroll(-2)]);

        // Content moves down by two rows; the top two rows are stale.
        assert_eq!((cell(&grid, 0, 0), cell(&grid, 0, 1)), ("r", "0"));
        assert_eq!((cell(&grid, 1, 0), cell(&grid, 1, 1)), ("r", "1"));
        assert_eq!((cell(&grid, 2, 0), cell(&grid, 2, 1)), ("r", "0"));
        assert_eq!(cell(&grid, 3, 1), "1");
        assert_eq!(cell(&grid, 4, 1), "2");
        for row in 0..5 {
            assert_eq!(cell(&grid, row, 11), "x", "row {row} outside the region");
        }
    }

    #[test]
    fn grid_clear_resets_cells() {
        let mut grid = Grid::new(6, 2);
        grid.apply(&[line(0, 0, "abc"), line(1, 0, "x")]);
        assert_eq!(cell(&grid, 0, 0), "a");

        grid.apply(&[one("grid_clear", vec![int(1)])]);
        assert!(grid.cells.iter().all(|c| c.text == " " && c.hl == 0));
    }

    #[test]
    fn grid_resize_smaller_clamps_cursor() {
        let mut grid = Grid::new(40, 10);
        grid.apply(&[line(0, 0, "abc"), cursor_goto(5, 30)]);
        assert_eq!(grid.cursor, (30, 5));

        grid.apply(&[resize(20, 3)]);

        assert_eq!((grid.width, grid.height), (20, 3));
        assert_eq!(grid.cells.len(), 60);
        // A grid shrinking below the cursor must not leave it outside.
        assert_eq!(grid.cursor, (19, 2));
        // Cells inside the new size keep their content.
        assert_eq!(cell(&grid, 0, 1), "b");
    }

    #[test]
    fn grid_resize_larger_keeps_cells() {
        let mut grid = Grid::new(4, 2);
        grid.apply(&[line(0, 0, "ab")]);
        grid.apply(&[resize(6, 3)]);

        assert_eq!((grid.width, grid.height), (6, 3));
        assert_eq!(grid.cells.len(), 18);
        assert_eq!(cell(&grid, 0, 1), "b");
        // New cells start blank rather than reusing stale text.
        assert_eq!(cell(&grid, 2, 5), " ");
        assert_eq!(grid.cursor, (0, 0));
    }

    #[test]
    fn grid_hl_attr_define_parses_rgb() {
        let mut grid = Grid::new(4, 1);
        let attrs = map(&[
            ("foreground", int(0x11_22_33)),
            ("background", int(0x44_55_66)),
            ("special", int(0x77_88_99)),
            ("bold", Value::from(true)),
            ("italic", Value::from(false)),
            ("underline", Value::from(true)),
            ("undercurl", Value::from(true)),
            ("reverse", Value::from(true)),
        ]);
        grid.apply(&[one(
            "hl_attr_define",
            vec![int(7), attrs, map(&[]), arr(vec![])],
        )]);

        let attr = grid.hl.get(&7).expect("highlight 7 is defined");
        assert_eq!(
            (attr.fg, attr.bg, attr.sp),
            (Some(0x11_22_33), Some(0x44_55_66), Some(0x77_88_99))
        );
        assert!(attr.bold && attr.underline && attr.undercurl && attr.reverse);
        assert!(!attr.italic && !attr.strikethrough);

        // Absent colours stay unset so a later default colour change applies.
        grid.apply(&[one(
            "hl_attr_define",
            vec![
                int(8),
                map(&[("foreground", int(1))]),
                map(&[]),
                arr(vec![]),
            ],
        )]);
        let attr = grid.hl.get(&8).expect("highlight 8 is defined");
        assert_eq!((attr.fg, attr.bg, attr.sp), (Some(1), None, None));
    }

    #[test]
    fn grid_default_colors_set_keeps_unset_values() {
        let mut grid = Grid::new(4, 1);
        grid.apply(&[one(
            "default_colors_set",
            vec![
                int(0x01_02_03),
                int(0x04_05_06),
                int(0x07_08_09),
                int(-1),
                int(-1),
            ],
        )]);
        assert_eq!(
            grid.default,
            Colors {
                fg: 0x01_02_03,
                bg: 0x04_05_06,
                sp: 0x07_08_09,
            }
        );

        // -1 means "unset" with `ext_termcolors`: keep what we had.
        grid.apply(&[one(
            "default_colors_set",
            vec![int(-1), int(-1), int(-1), int(-1), int(-1)],
        )]);
        assert_eq!(grid.default.bg, 0x04_05_06);
    }

    #[test]
    fn grid_mode_change_sets_cursor_shape() {
        let mut grid = Grid::new(10, 2);
        let modes = arr(vec![
            map(&[
                ("name", text("normal")),
                ("short_name", text("n")),
                ("cursor_shape", text("block")),
            ]),
            map(&[("name", text("insert")), ("cursor_shape", text("vertical"))]),
            map(&[
                ("name", text("replace")),
                ("cursor_shape", text("horizontal")),
            ]),
        ]);
        grid.apply(&[one("mode_info_set", vec![Value::from(true), modes])]);

        grid.apply(&[one("mode_change", vec![text("insert"), int(1)])]);
        assert_eq!(grid.mode.name, "insert");
        assert_eq!(grid.mode.cursor, CursorShape::Vertical);

        grid.apply(&[one("mode_change", vec![text("replace"), int(2)])]);
        assert_eq!(grid.mode.name, "replace");
        assert_eq!(grid.mode.cursor, CursorShape::Horizontal);
    }

    #[test]
    fn grid_flush_busy_and_foreign_grids() {
        let mut grid = Grid::new(6, 2);
        // `flush` is the signal that the grid may be drawn; it carries an
        // empty parameter tuple.
        assert!(grid.apply(&[line(0, 0, "ab"), one("flush", vec![])]));
        assert!(!grid.apply(&[line(0, 0, "cd")]));
        // A bare `["flush"]` without a parameter tuple still counts.
        assert!(grid.apply(&[arr(vec![text("flush")])]));

        grid.apply(&[arr(vec![text("busy_start")])]);
        assert!(grid.busy);
        grid.apply(&[arr(vec![text("busy_stop")])]);
        assert!(!grid.busy);

        // Grid 2 is not modelled: neither its cells nor its cursor may leak in.
        grid.apply(&[
            one(
                "grid_line",
                vec![
                    int(2),
                    int(0),
                    int(0),
                    arr(vec![arr(vec![text("z"), int(0)])]),
                    Value::from(false),
                ],
            ),
            one("grid_cursor_goto", vec![int(2), int(1), int(1)]),
        ]);
        assert_eq!(cell(&grid, 0, 0), "c");
        assert_eq!(grid.cursor, (0, 0));
    }

    /// Recorded from `nvim --clean --embed` at 40x10 by [`record_grid_fixtures`].
    ///
    /// Read at runtime rather than with `include_bytes!`, because the recorder
    /// needs the crate to build before the files exist.
    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nvim")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Replay a fixture: an array of `redraw` batch parameter lists.
    fn replay(fixture: &[u8]) -> Grid {
        let mut reader = std::io::Cursor::new(fixture);
        let batches = rmpv::decode::read_value(&mut reader).expect("fixture decodes");
        let mut grid = Grid::new(40, 10);
        for batch in batches.as_array().expect("array of batches") {
            grid.apply(batch.as_array().expect("array of events"));
        }
        grid
    }

    #[test]
    fn grid_fixture_hello_matches_text() {
        let grid = replay(&fixture("hello.msgpack"));

        assert_eq!((grid.width, grid.height), (40, 10));
        assert!(
            row_text(&grid, 0).starts_with("hello"),
            "row 0 was {:?}",
            row_text(&grid, 0).trim_end()
        );
        // `<Esc>` is back in normal mode with a block cursor.
        assert_eq!(grid.mode.name, "normal");
        assert_eq!(grid.mode.cursor, CursorShape::Block);
        assert_eq!(grid.cursor, (4, 0));
    }

    #[test]
    fn grid_wide_chars_take_two_cells() {
        let grid = replay(&fixture("wide.msgpack"));

        // Concatenated cell texts give the line back, including the
        // double-width characters.
        assert!(
            row_text(&grid, 0).starts_with("äöü 😀 漢字"),
            "row 0 was {:?}",
            row_text(&grid, 0).trim_end()
        );

        // A double-width character sits in its first cell; the continuation
        // cell is empty so the following text starts two columns later.
        let expected = ["ä", "ö", "ü", " ", "😀", "", " ", "漢", "", "字", ""];
        let actual: Vec<&str> = (0..expected.len()).map(|col| cell(&grid, 0, col)).collect();
        assert_eq!(actual, expected);

        // The cells cover as many columns as the text is wide, so the cursor
        // after `<Esc>` lands on the first cell of the last character.
        let width: usize = (0..expected.len())
            .map(|col| unicode_width::UnicodeWidthStr::width(cell(&grid, 0, col)))
            .sum();
        assert_eq!(width, 11);
        assert_eq!(grid.cursor, (9, 0));
    }

    /// Regenerate the committed fixtures from a real nvim:
    /// `cargo test -p emeraldian record_grid_fixtures -- --ignored`.
    #[test]
    #[ignore = "records the committed redraw fixtures from a real nvim"]
    fn record_grid_fixtures() {
        record("hello.msgpack", "ihello<Esc>");
        record("wide.msgpack", "iäöü 😀 漢字<Esc>");
    }

    fn record(name: &str, keys: &str) {
        use crate::nvim::rpc::{Incoming, Rpc};
        use std::time::{Duration, Instant};

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nvim");
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let rpc = Rpc::spawn(
            "nvim",
            "",
            &std::env::temp_dir(),
            &["--clean", "--cmd", "set noswapfile"],
        )
        .expect("spawn nvim --embed");

        let options = Value::Map(vec![
            (text("ext_linegrid"), Value::from(true)),
            (text("rgb"), Value::from(true)),
        ]);
        rpc.request(
            "nvim_ui_attach",
            vec![Value::from(40), Value::from(10), options],
            Duration::from_secs(5),
        )
        .expect("nvim_ui_attach");
        rpc.notify("nvim_input", vec![Value::from(keys)])
            .expect("nvim_input");

        // Collect until nvim has been quiet for a moment; there is no "done"
        // marker for input.
        let mut batches = Vec::new();
        let mut last = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(5);
        while last.elapsed() < Duration::from_millis(300) && Instant::now() < deadline {
            match rpc.try_recv() {
                Some(Incoming::Notification { method, params }) if method == "redraw" => {
                    batches.push(Value::Array(params));
                    last = Instant::now();
                }
                Some(_) => last = Instant::now(),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert!(!batches.is_empty(), "nvim sent no redraw for {name}");

        let mut buffer = Vec::new();
        rmpv::encode::write_value(&mut buffer, &Value::Array(batches)).expect("encode fixture");
        std::fs::write(dir.join(name), &buffer).expect("write fixture");
        rpc.shutdown(Duration::from_secs(2));

        let grid = replay(&buffer);
        let row: Vec<&str> = (0..12).map(|col| cell(&grid, 0, col)).collect();
        println!("{name}: row 0 = {row:?}, cursor = {:?}", grid.cursor);
    }
}
