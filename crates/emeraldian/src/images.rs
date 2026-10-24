//! Drawing pictures in the terminal.
//!
//! Terminals that speak Kitty's graphics protocol, iTerm2's, or sixel can show
//! real pixels; the rest get half-block mosaics, which are coarse but always
//! work. [`ratatui_image`] handles the encoding, and [`Picker`] asks the
//! terminal which of those it supports. That question has to be asked *before*
//! the alternate screen is entered, because it is answered on stdin.
//!
//! Two things make this awkward in a scrolling reading pane, and both are
//! handled here:
//!
//! * Encoding an image takes long enough to drop frames, so it happens on a
//!   worker thread and the pane draws a placeholder until it lands.
//! * The layout has to know how tall a picture is *before* it is decoded, or
//!   the text below it would jump when it arrives. Image headers carry the
//!   pixel size and are cheap to read, so the height is known from the first
//!   frame and the picture fades in without moving anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use ratatui::layout::Size;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use ratatui_image::{FilterType, FontSize, Resize};

use crate::diagram::{self, Kind, Look, Svg};

/// Extensions worth opening. Anything else — a PDF, an embedded note — is left
/// to the text fallback rather than handed to a decoder that will refuse it.
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// How pixels are thrown away when a picture is shrunk to fit.
///
/// [`ratatui_image`] defaults to nearest-neighbour, which simply drops the
/// pixels that don't land on a sample — fine for a solid-colour icon, ruinous
/// for a screenshot, where it deletes most of the strokes that make text
/// legible. Lanczos averages the neighbourhood instead, and the cost is paid
/// once on a worker thread rather than per frame.
const FILTER: Option<FilterType> = Some(FilterType::Lanczos3);

/// How many encoded images to hold on to.
///
/// Every width gets its own encoding, so dragging a terminal edge from 80 to
/// 200 columns walks through a hundred of them. Without a cap that is a hundred
/// copies of the picture sitting in memory.
const MAX_CACHED: usize = 24;

/// What the config asked for, when it asked for anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Choice {
    /// Believe the terminal's own answer.
    Auto,
    /// Draw this way whatever the terminal claims.
    Use(ProtocolType),
    /// A name nobody recognises; the caller should say so and carry on.
    Unknown,
}

/// Reads a protocol name out of the config.
#[must_use]
pub fn choice(name: &str) -> Choice {
    match name.trim().to_ascii_lowercase().as_str() {
        "auto" | "" => Choice::Auto,
        "kitty" => Choice::Use(ProtocolType::Kitty),
        "iterm2" => Choice::Use(ProtocolType::Iterm2),
        "sixel" => Choice::Use(ProtocolType::Sixel),
        "halfblocks" | "half-blocks" => Choice::Use(ProtocolType::Halfblocks),
        _ => Choice::Unknown,
    }
}

/// Whether a path is worth trying to draw.
#[must_use]
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Where a picture comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// An image file on disk.
    File(PathBuf),
    /// A diagram drawn from text, named by a digest of what it was drawn from
    /// and the colours it was drawn in (see [`Images::diagram`]).
    Diagram(u64),
}

/// How much room a picture may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sizing {
    /// One picture among prose: capped at a share of the pane, never enlarged.
    Inline,
    /// The picture is the page: as wide as the pane allows however tall that
    /// makes it, enlarged if it is vector art. The reading view scrolls it.
    Page,
}

/// How far a diagram has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status<'a> {
    /// Being laid out on the worker thread.
    Pending,
    Ready,
    /// The renderer's complaint, short enough for one line.
    Failed(&'a str),
}

enum Diagram {
    Pending,
    Ready { svg: Arc<Svg>, look: Look },
    Failed(String),
}

/// Diagrams kept at once; past that the finished ones are dropped. They are
/// named by their content, so dropping one only costs drawing it again.
const MAX_DIAGRAMS: usize = 64;

/// The terminal's picture-drawing ability, plus everything already drawn.
pub struct Images {
    /// `None` when images are switched off, or the terminal was never asked.
    picker: Option<Picker>,
    /// Ceiling on how tall one picture may be drawn, as a percentage of the
    /// reading pane. A share rather than a row count, so a picture is as big as
    /// the window allows: a fixed cap that leaves a diagram readable on a
    /// 24-row terminal wastes most of a full-screen one.
    max_height_percent: u16,
    /// Height of the pane pictures are being drawn into, set each frame.
    pane_rows: u16,
    /// Pixel dimensions by path. `None` records a file that could not be read,
    /// so a missing image is not re-opened on every frame.
    dims: HashMap<PathBuf, Option<(u32, u32)>>,
    diagrams: HashMap<u64, Diagram>,
    cache: HashMap<Key, Entry>,
    clock: u64,
    worker: Option<Worker>,
    /// Changed by [`Images::forget`], since what a picture measures may have
    /// changed with it. Drawn from one counter, so a new `Images` never
    /// repeats an old one's number either.
    generation: u64,
}

static GENERATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_generation() -> u64 {
    GENERATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

type Key = (Source, u16, u16);

struct Entry {
    state: State,
    used: u64,
}

enum State {
    Pending,
    Ready(Box<SlicedProtocol>),
    Failed,
}

struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

enum Job {
    /// Encode a picture for the terminal. A diagram brings its drawing along.
    Encode {
        key: Key,
        picker: Picker,
        diagram: Option<(Arc<Svg>, Look)>,
    },
    /// Lay a diagram out, which is the slow half of drawing one.
    Prepare {
        id: u64,
        kind: Kind,
        text: String,
        look: Look,
    },
}

enum Done {
    Encoded(Key, Option<Box<SlicedProtocol>>),
    Prepared(u64, Look, Result<Arc<Svg>, String>),
}

impl Images {
    /// Asks the terminal what it can draw.
    ///
    /// Must be called before the alternate screen is entered: the query is
    /// written to stdout and the answer read back from stdin, which only works
    /// while the terminal is still in its normal mode.
    ///
    /// A terminal that doesn't answer gets half-blocks, which need no support
    /// at all. A `max_height_percent` of 0, or a terminal that can't even do
    /// half-blocks, switches images off and leaves the alt text in their place.
    ///
    /// `wanted` overrules the answer. Some terminals claim a protocol they
    /// don't paint — a recorder, or a multiplexer eating the escapes — and the
    /// picture then goes missing rather than looking wrong, which is the one
    /// failure the terminal cannot be asked about. The cell size still comes
    /// from the query, because that part of the answer is right even when the
    /// protocol isn't.
    #[must_use]
    pub fn probe(enabled: bool, max_height_percent: u16, wanted: Choice) -> Self {
        if !enabled || max_height_percent == 0 {
            return Self::with_picker(None, max_height_percent);
        }
        let queried = Picker::from_query_stdio().ok();
        let picker = match (queried, wanted) {
            (Some(mut picker), Choice::Use(protocol)) => {
                picker.set_protocol_type(protocol);
                Some(picker)
            }
            // A silent terminal can still be told what to draw; half-blocks
            // carry the cell size the library assumes when nobody says.
            (None, Choice::Use(protocol)) => {
                let mut picker = Picker::halfblocks();
                picker.set_protocol_type(protocol);
                Some(picker)
            }
            (queried, _) => queried,
        };
        Self::with_picker(picker, max_height_percent)
    }

    /// An `Images` that draws nothing, for tests and headless runs.
    #[must_use]
    pub fn disabled() -> Self {
        Self::with_picker(None, 0)
    }

    /// An `Images` with no terminal behind it, for tests.
    ///
    /// Half-blocks assume a 10x20 cell, which is what the tests measure
    /// against.
    #[cfg(test)]
    #[must_use]
    pub fn halfblocks(max_height_percent: u16) -> Self {
        Self::with_picker(Some(Picker::halfblocks()), max_height_percent)
    }

    /// An `Images` that claims the kitty protocol with no terminal behind it,
    /// so diagrams draw and encode. Same 10x20 cells as [`Self::halfblocks`].
    #[cfg(test)]
    #[must_use]
    pub fn kitty(max_height_percent: u16) -> Self {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        Self::with_picker(Some(picker), max_height_percent)
    }

    fn with_picker(picker: Option<Picker>, max_height_percent: u16) -> Self {
        Self {
            picker,
            max_height_percent,
            pane_rows: 0,
            dims: HashMap::new(),
            diagrams: HashMap::new(),
            cache: HashMap::new(),
            clock: 0,
            worker: None,
            generation: next_generation(),
        }
    }

    /// How the terminal draws pictures, and how big one cell is.
    ///
    /// `None` when nothing can be drawn at all. Reported at startup, because
    /// half-blocks look mushy next to a real graphics protocol and there is
    /// otherwise no way to tell a terminal that quietly fell back to them from
    /// one that drew the picture badly.
    #[must_use]
    pub fn describe(&self) -> Option<String> {
        let picker = self.picker.as_ref()?;
        let protocol = match picker.protocol_type() {
            ProtocolType::Kitty => "kitty",
            ProtocolType::Iterm2 => "iTerm2",
            ProtocolType::Sixel => "sixel",
            ProtocolType::Halfblocks => "half-blocks",
        };
        let font = picker.font_size();
        Some(format!("{protocol}, {}x{} cells", font.width, font.height))
    }

    /// Whether pictures are being drawn as text mosaics rather than as pixels.
    ///
    /// Half-blocks are the fallback for a terminal with no graphics protocol at
    /// all — Apple's Terminal.app is the common one. Each cell becomes two
    /// coloured blocks, so a picture is drawn at roughly its width in *cells*:
    /// a screenshot ends up around eighty pixels across, which no amount of
    /// resampling can make readable. Worth saying plainly, because the result
    /// looks like a bug in the app rather than a limit of the terminal.
    #[must_use]
    pub fn is_coarse(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(|picker| picker.protocol_type() == ProtocolType::Halfblocks)
    }

    /// Whether a diagram can be drawn legibly: the terminal has a graphics
    /// protocol, not just half-blocks.
    ///
    /// Diagrams are small text on thin lines, and half-blocks turn that into
    /// grey mush, so a terminal without pixels keeps the source (Mermaid) or
    /// the braille drawing (Excalidraw) it had before.
    #[must_use]
    pub fn draws_diagrams(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(|picker| picker.protocol_type() != ProtocolType::Halfblocks)
            && self.max_height_percent > 0
    }

    /// Records the pane pictures are drawn into, which sets their height cap.
    pub fn set_pane_height(&mut self, rows: u16) {
        self.pane_rows = rows;
    }

    /// Tallest a single picture may be drawn, in rows.
    fn max_rows(&self) -> u16 {
        (u32::from(self.pane_rows) * u32::from(self.max_height_percent) / 100)
            .try_into()
            .unwrap_or(u16::MAX)
            .max(1)
    }

    /// How much room a picture will take, in terminal cells.
    ///
    /// Read from the file's header rather than a decoded image, so the answer
    /// is available on the first frame and the layout never has to move once
    /// the picture arrives. `None` means there is nothing to draw and the
    /// caller should fall back to text.
    pub fn measure(&mut self, source: &Source, available: Size, sizing: Sizing) -> Option<Size> {
        let max_rows = match sizing {
            Sizing::Inline => self.max_rows(),
            Sizing::Page => u16::MAX,
        };
        let picker = self.picker.as_ref()?;
        if available.width == 0 {
            return None;
        }

        let (pixels, vector) = match source {
            Source::File(path) => {
                if !is_image(path) {
                    return None;
                }
                let pixels = *self
                    .dims
                    .entry(path.clone())
                    .or_insert_with(|| read_dimensions(path));
                (pixels?, false)
            }
            Source::Diagram(id) => match self.diagrams.get(id)? {
                Diagram::Ready { svg, .. } => {
                    ((svg.width.ceil() as u32, svg.height.ceil() as u32), true)
                }
                Diagram::Pending | Diagram::Failed(_) => return None,
            },
        };

        let room = Size::new(available.width, available.height.min(max_rows));
        // Pictures are never enlarged, except vector art the reader asked to
        // have as the page: it has no resolution to run out of.
        let enlarge = vector && sizing == Sizing::Page;
        Some(fit(pixels, picker.font_size(), room, enlarge))
    }

    /// Asks for a diagram to be drawn, and says which picture it will be.
    ///
    /// The drawing happens on the worker thread; until it lands, [`status`]
    /// says `Pending` and [`measure`] finds nothing. The id covers the colours
    /// as well as the source, so a theme switch asks for a new drawing and an
    /// edit to the source can never show a stale one.
    ///
    /// [`status`]: Self::status
    /// [`measure`]: Self::measure
    pub fn diagram(&mut self, kind: Kind, text: &str, look: &Look) -> Source {
        let id = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (kind, text, look).hash(&mut hasher);
            hasher.finish()
        };
        if !self.diagrams.contains_key(&id) {
            if self.diagrams.len() >= MAX_DIAGRAMS {
                self.diagrams
                    .retain(|_, diagram| matches!(diagram, Diagram::Pending));
                self.cache
                    .retain(|(source, ..), _| !matches!(source, Source::Diagram(_)));
            }
            self.diagrams.insert(id, Diagram::Pending);
            let worker = self.worker.get_or_insert_with(spawn_worker);
            let _ = worker.jobs.send(Job::Prepare {
                id,
                kind,
                text: text.to_string(),
                look: *look,
            });
        }
        Source::Diagram(id)
    }

    /// How far a picture has got. A file is always ready to be tried.
    #[must_use]
    pub fn status(&self, source: &Source) -> Status<'_> {
        let Source::Diagram(id) = source else {
            return Status::Ready;
        };
        match self.diagrams.get(id) {
            Some(Diagram::Ready { .. }) => Status::Ready,
            Some(Diagram::Failed(why)) => Status::Failed(why),
            // Asked for and not heard back from, or dropped to make room.
            Some(Diagram::Pending) | None => Status::Pending,
        }
    }

    /// How many cells wide a pixel width is, for Obsidian's `![[x.png|400]]`.
    #[must_use]
    pub fn cells_wide(&self, pixels: u32) -> Option<u16> {
        let font = self.picker.as_ref()?.font_size();
        let cells = pixels / u32::from(font.width.max(1));
        Some(u16::try_from(cells).unwrap_or(u16::MAX).max(1))
    }

    /// The encoded picture, if it is ready.
    ///
    /// A miss starts the work and returns `None`; the next frame after it lands
    /// will get it. Callers should draw a placeholder in the space [`measure`]
    /// reserved.
    ///
    /// [`measure`]: Self::measure
    pub fn get(&mut self, source: &Source, size: Size) -> Option<&SlicedProtocol> {
        let picker = self.picker.as_ref()?.clone();
        let diagram = match source {
            Source::File(_) => None,
            Source::Diagram(id) => match self.diagrams.get(id)? {
                Diagram::Ready { svg, look } => Some((svg.clone(), *look)),
                Diagram::Pending | Diagram::Failed(_) => return None,
            },
        };
        let key = (source.clone(), size.width, size.height);
        self.clock += 1;

        if !self.cache.contains_key(&key) {
            self.evict();
            self.cache.insert(
                key.clone(),
                Entry {
                    state: State::Pending,
                    used: self.clock,
                },
            );
            let worker = self.worker.get_or_insert_with(spawn_worker);
            // A dead worker leaves the entry pending forever, which draws the
            // placeholder rather than crashing the reader.
            let _ = worker.jobs.send(Job::Encode {
                key: key.clone(),
                picker,
                diagram,
            });
        }

        let entry = self.cache.get_mut(&key)?;
        entry.used = self.clock;
        match &entry.state {
            State::Ready(protocol) => Some(protocol),
            State::Pending | State::Failed => None,
        }
    }

    /// Takes in finished work. Returns whether anything new arrived.
    pub fn poll(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let mut arrived = false;
        let mut measured = false;
        loop {
            match worker.done.try_recv() {
                Ok(Done::Encoded(key, protocol)) => {
                    if let Some(entry) = self.cache.get_mut(&key) {
                        entry.state = match protocol {
                            Some(protocol) => State::Ready(protocol),
                            None => State::Failed,
                        };
                        arrived = true;
                    }
                }
                Ok(Done::Prepared(id, look, result)) => {
                    // Dropped to make room while it was away: nobody is waiting.
                    if let Some(slot) = self.diagrams.get_mut(&id) {
                        *slot = match result {
                            Ok(svg) => Diagram::Ready { svg, look },
                            Err(why) => Diagram::Failed(why),
                        };
                        arrived = true;
                        measured = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    break;
                }
            }
        }
        // A diagram that has just been laid out has a size, which the reading
        // layout was waiting on.
        if measured {
            self.generation = next_generation();
        }
        arrived
    }

    /// Drops everything remembered, so edited files are read again.
    pub fn forget(&mut self) {
        self.dims.clear();
        self.cache.clear();
        self.generation = next_generation();
    }

    /// A number that changes whenever pictures may measure differently.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Drops the least recently drawn half once the cache is full.
    ///
    /// Half at a time rather than one at a time, so a resize that invalidates
    /// every entry doesn't evict on every single frame.
    fn evict(&mut self) {
        if self.cache.len() < MAX_CACHED {
            return;
        }
        let mut used: Vec<u64> = self.cache.values().map(|entry| entry.used).collect();
        used.sort_unstable();
        let cutoff = used[used.len() / 2];
        self.cache.retain(|_, entry| entry.used > cutoff);
    }
}

/// Scales `pixels` to fit `available`, in cells.
///
/// Pictures are not scaled up unless `enlarge` is set: a 16-pixel icon stays an
/// icon rather than being blown across the pane.
fn fit(pixels: (u32, u32), font: FontSize, available: Size, enlarge: bool) -> Size {
    let (width, height) = pixels;
    if width == 0 || height == 0 {
        return Size::new(0, 0);
    }

    let room = |cells: u16, per_cell: u16| f64::from(cells) * f64::from(per_cell.max(1));
    let scale = (room(available.width, font.width) / f64::from(width))
        .min(room(available.height, font.height) / f64::from(height))
        .min(if enlarge { f64::MAX } else { 1.0 });

    let cells = |pixels: u32, per_cell: u16, limit: u16| {
        let cells = (f64::from(pixels) * scale / f64::from(per_cell.max(1))).ceil();
        (cells as u16).clamp(1, limit.max(1))
    };
    Size::new(
        cells(width, font.width, available.width),
        cells(height, font.height, available.height),
    )
}

/// Shrinks a decoded image to the pixels `size` cells hold.
///
/// Done here rather than left to [`ratatui_image`] because that crate's
/// iTerm2 path resizes with nearest-neighbour regardless of the [`Resize`] it
/// is handed, and nearest-neighbour downscaling of a screenshot or a diagram
/// drops whole rows of pixels — which is what made text in a picture
/// unreadable. Handing it an image already at the target size leaves that
/// resize with nothing to do, so every protocol gets the good filter.
fn downscale(image: image::DynamicImage, size: Size, font: FontSize) -> image::DynamicImage {
    let width = u32::from(size.width) * u32::from(font.width.max(1));
    let height = u32::from(size.height) * u32::from(font.height.max(1));
    if width == 0 || height == 0 || (image.width() <= width && image.height() <= height) {
        return image;
    }
    // `resize` fits inside the box and keeps the aspect ratio, which is the
    // same rule `measure` sized the cells with.
    image.resize(width, height, FILTER.unwrap_or(FilterType::Lanczos3))
}

/// Reads an image's pixel size without decoding it.
fn read_dimensions(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Draws a diagram's text to SVG.
fn prepare(kind: Kind, text: &str, look: &Look) -> Result<Svg, String> {
    match kind {
        Kind::Mermaid => diagram::mermaid_svg(text, look),
        Kind::Excalidraw => {
            let drawing = emeraldian_core::excalidraw::parse(text)
                .ok_or("this is not a readable Excalidraw drawing")?;
            diagram::excalidraw_svg(&drawing, look).ok_or_else(|| "this drawing is empty".into())
        }
    }
}

/// Runs decoding and encoding away from the draw loop.
///
/// One thread, so a note full of pictures fills in top to bottom rather than
/// starting every decode at once and finishing none of them.
fn spawn_worker() -> Worker {
    let (jobs, rx) = mpsc::channel::<Job>();
    let (tx, done) = mpsc::channel();

    std::thread::Builder::new()
        // Linux caps a thread name at 15 bytes and refuses anything longer,
        // so this is deliberately shorter than the crate name.
        .name("emerald-images".into())
        .spawn(move || {
            for job in rx {
                let done = match job {
                    Job::Encode {
                        key,
                        picker,
                        diagram,
                    } => {
                        let size = Size::new(key.1, key.2);
                        let font = picker.font_size();
                        let image = match (&key.0, &diagram) {
                            (Source::Diagram(_), Some((svg, look))) => {
                                // Drawn at exactly the pixels the cells hold, so
                                // text in a diagram is never resampled.
                                let width = u32::from(size.width) * u32::from(font.width.max(1));
                                let height = u32::from(size.height) * u32::from(font.height.max(1));
                                diagram::rasterize(&svg.text, look, width, height)
                            }
                            (Source::File(path), _) => image::ImageReader::open(path)
                                .ok()
                                .and_then(|reader| reader.with_guessed_format().ok())
                                .and_then(|reader| reader.decode().ok()),
                            _ => None,
                        };
                        let protocol =
                            image
                                .map(|image| downscale(image, size, font))
                                .and_then(|image| {
                                    SlicedProtocol::new_with_resize(
                                        &picker,
                                        image,
                                        size,
                                        Resize::Fit(FILTER),
                                    )
                                    .ok()
                                });
                        Done::Encoded(key, protocol.map(Box::new))
                    }
                    Job::Prepare {
                        id,
                        kind,
                        text,
                        look,
                    } => Done::Prepared(id, look, prepare(kind, &text, &look).map(Arc::new)),
                };
                if tx.send(done).is_err() {
                    return;
                }
            }
        })
        // A machine that cannot spawn a thread has bigger problems than
        // pictures; the channel simply stays empty and placeholders remain.
        .ok();

    Worker { jobs, done }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT: FontSize = FontSize {
        width: 10,
        height: 20,
    };

    #[test]
    fn a_wide_picture_is_bounded_by_the_pane_width() {
        // 1000x200 pixels is 100x10 cells at this font, wider than the pane.
        let size = fit((1000, 200), FONT, Size::new(40, 30), false);
        assert_eq!(size.width, 40, "fills the width it is given");
        assert_eq!(size.height, 4, "and keeps its shape: 200 * 0.4 / 20");
    }

    #[test]
    fn a_tall_picture_is_bounded_by_the_row_cap() {
        let size = fit((200, 1000), FONT, Size::new(40, 10), false);
        assert_eq!(size.height, 10, "clipped to the rows allowed");
        assert_eq!(size.width, 4, "200 * 0.2 / 10");
    }

    #[test]
    fn a_small_picture_is_left_alone() {
        let size = fit((100, 40), FONT, Size::new(80, 20), false);
        assert_eq!(
            (size.width, size.height),
            (10, 2),
            "an icon stays an icon rather than being blown up to fill the pane"
        );
    }

    #[test]
    fn a_picture_always_gets_at_least_one_cell() {
        let size = fit((3, 3), FONT, Size::new(80, 20), false);
        assert_eq!(
            (size.width, size.height),
            (1, 1),
            "never rounds away to nothing"
        );
    }

    #[test]
    fn only_decodable_files_are_offered_to_the_decoder() {
        assert!(
            is_image(Path::new("a/b/chart.PNG")),
            "extensions vary in case"
        );
        assert!(is_image(Path::new("chart.jpeg")));
        assert!(
            !is_image(Path::new("notes/other.md")),
            "an embedded note is not a picture"
        );
        assert!(!is_image(Path::new("paper.pdf")));
        assert!(!is_image(Path::new("no-extension")));
    }

    #[test]
    fn shrinking_a_picture_averages_pixels_instead_of_dropping_them() {
        // Fine vertical stripes are what text in a screenshot looks like to a
        // resampler. Nearest-neighbour keeps whichever stripe a sample lands
        // on and throws the rest away, so the strokes that make the text
        // readable disappear and what's left is noise. Averaging turns them
        // into an even mid-grey, which is the honest answer at this size.
        let mut striped = image::RgbImage::new(64, 8);
        for (x, _, pixel) in striped.enumerate_pixels_mut() {
            *pixel = if x % 2 == 0 {
                image::Rgb([0, 0, 0])
            } else {
                image::Rgb([255, 255, 255])
            };
        }
        let striped = image::DynamicImage::ImageRgb8(striped);

        // Eight cells of a 4x8 font is 32 pixels: half the width, so every
        // output pixel covers one black and one white stripe.
        let shrunk = downscale(
            striped,
            Size::new(8, 1),
            FontSize {
                width: 4,
                height: 8,
            },
        );
        assert_eq!(shrunk.width(), 32, "shrunk to the pixels the cells hold");

        let samples = shrunk.to_rgb8();
        let extreme = samples
            .pixels()
            .filter(|p| p.0[0] < 40 || p.0[0] > 215)
            .count();
        assert!(
            extreme * 4 < samples.pixels().count(),
            "{extreme} of {} pixels stayed pure black or white — the stripes were \
             dropped rather than averaged",
            samples.pixels().count()
        );
    }

    #[test]
    fn a_picture_that_already_fits_is_not_resampled() {
        let small = image::DynamicImage::ImageRgb8(image::RgbImage::new(20, 10));
        let font = FontSize {
            width: 10,
            height: 20,
        };
        let same = downscale(small, Size::new(80, 24), font);
        assert_eq!(
            (same.width(), same.height()),
            (20, 10),
            "an icon is left exactly as it is rather than blown up"
        );
    }

    #[test]
    fn a_protocol_can_be_named_instead_of_detected() {
        assert_eq!(choice("auto"), Choice::Auto);
        assert_eq!(choice(""), Choice::Auto, "an empty setting is no setting");
        assert_eq!(
            choice(" Halfblocks "),
            Choice::Use(ProtocolType::Halfblocks),
            "written by hand, so spacing and capitals are not the user's problem"
        );
        assert_eq!(
            choice("half-blocks"),
            Choice::Use(ProtocolType::Halfblocks),
            "the startup banner spells it with a hyphen"
        );
        assert_eq!(choice("kitty"), Choice::Use(ProtocolType::Kitty));
        assert_eq!(choice("iterm2"), Choice::Use(ProtocolType::Iterm2));
        assert_eq!(choice("sixel"), Choice::Use(ProtocolType::Sixel));
    }

    #[test]
    fn an_unrecognised_protocol_is_reported_rather_than_obeyed() {
        // Distinct from `Auto` so the caller can say so: silently ignoring a
        // misspelling leaves someone staring at a blank hole where the picture
        // should be, which is the exact problem the setting exists to fix.
        assert_eq!(choice("iterm"), Choice::Unknown);
        assert_eq!(choice("kitty2"), Choice::Unknown);
    }

    #[test]
    fn naming_a_protocol_does_not_switch_pictures_back_on() {
        let images = Images::probe(false, 66, Choice::Use(ProtocolType::Halfblocks));
        assert!(
            images.describe().is_none(),
            "images = false still means no pictures at all"
        );
        let images = Images::probe(true, 0, Choice::Use(ProtocolType::Halfblocks));
        assert!(images.describe().is_none(), "and so does no room for them");
    }

    #[test]
    fn a_diagram_is_laid_out_off_thread_then_measured_and_encoded() {
        let mut images = Images::kitty(100);
        images.set_pane_height(40);
        let look = Look {
            dark: true,
            background: [0x1e, 0x1e, 0x2e],
        };
        let source = images.diagram(Kind::Mermaid, "flowchart LR\n  A --> B", &look);
        assert_eq!(images.status(&source), Status::Pending);
        assert_eq!(
            images.measure(&source, Size::new(80, 30), Sizing::Inline),
            None,
            "a size is not known until the worker has laid the diagram out"
        );

        let before = images.generation();
        wait(&mut images, |images| {
            images.status(&source) != Status::Pending
        });
        assert_eq!(images.status(&source), Status::Ready);
        assert_ne!(
            images.generation(),
            before,
            "layouts that were waiting on the size are told to run again"
        );

        let inline = images
            .measure(&source, Size::new(200, 40), Sizing::Inline)
            .expect("measured");
        let page = images
            .measure(&source, Size::new(200, 40), Sizing::Page)
            .expect("measured");
        assert!(
            page.width > inline.width,
            "as the page it is enlarged ({page:?} vs {inline:?}); vector art has no resolution to lose"
        );
        assert!(page.height <= 40 && page.width <= 200);

        // Encoding is the second job on the same thread.
        let mut encoded = false;
        for _ in 0..500 {
            if images.get(&source, page).is_some() {
                encoded = true;
                break;
            }
            images.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(encoded, "the picture reaches the terminal protocol");
    }

    fn wait(images: &mut Images, done: impl Fn(&Images) -> bool) {
        for _ in 0..500 {
            images.poll();
            if done(images) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the worker never answered");
    }

    #[test]
    fn a_diagram_that_cannot_be_drawn_says_why() {
        let mut images = Images::kitty(100);
        let look = Look {
            dark: false,
            background: [255; 3],
        };
        let source = images.diagram(Kind::Excalidraw, "{ not a drawing", &look);
        wait(&mut images, |images| {
            images.status(&source) != Status::Pending
        });
        assert!(
            matches!(images.status(&source), Status::Failed(why) if why.contains("Excalidraw")),
            "{:?}",
            images.status(&source)
        );
        assert_eq!(
            images.measure(&source, Size::new(80, 20), Sizing::Page),
            None
        );
    }

    #[test]
    fn a_diagram_is_named_by_its_source_and_its_colours() {
        let mut images = Images::kitty(100);
        let dark = Look {
            dark: true,
            background: [0, 0, 0],
        };
        let light = Look {
            dark: false,
            background: [255; 3],
        };
        let a = images.diagram(Kind::Mermaid, "flowchart LR\n  A --> B", &dark);
        assert_eq!(
            a,
            images.diagram(Kind::Mermaid, "flowchart LR\n  A --> B", &dark)
        );
        assert_ne!(
            a,
            images.diagram(Kind::Mermaid, "flowchart LR\n  A --> B", &light),
            "a theme switch draws it again"
        );
        assert_ne!(
            a,
            images.diagram(Kind::Mermaid, "flowchart LR\n  A --> C", &dark),
            "and so does an edit"
        );
    }

    #[test]
    fn only_a_terminal_with_pixels_draws_diagrams() {
        assert!(Images::kitty(66).draws_diagrams());
        assert!(
            !Images::halfblocks(66).draws_diagrams(),
            "half-blocks turn thin lines and small text into mush"
        );
        assert!(!Images::disabled().draws_diagrams());
        assert!(!Images::kitty(0).draws_diagrams(), "no room, no pictures");
    }

    #[test]
    fn a_disabled_terminal_measures_nothing() {
        let mut images = Images::disabled();
        assert!(images.describe().is_none(), "nothing to report or to draw");
        assert_eq!(
            images.measure(
                &Source::File("chart.png".into()),
                Size::new(80, 20),
                Sizing::Inline
            ),
            None
        );
        assert!(!images.poll(), "and never has work to collect");
    }
}
