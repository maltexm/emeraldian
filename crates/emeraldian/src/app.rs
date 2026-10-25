//! Application state and the actions that change it.
//!
//! Everything the user or the agent can do is an [`Action`], and every action
//! flows through [`App::dispatch`]. Keys, the command palette, mouse clicks and
//! agent tools all funnel into that one place, which is why a command works
//! identically however it was invoked — and why the agent gets the same
//! capabilities the user has, not a parallel set.

use std::path::PathBuf;

use ratatui::layout::Rect;

use emeraldian_core::graph::{Graph, GraphOptions, Simulation, Vec2};
use emeraldian_core::index::{NoteId, VaultIndex};
use emeraldian_core::vault::{ScanOptions, Vault};
use emeraldian_theme::{ActiveTheme, presets};

use crate::agent::Chat;
use crate::config::Config;
use crate::editor::Editor;
use crate::explorer::Explorer;
use crate::modal::{Confirm, Modal};
use crate::nvim::bridge::LinkTarget;

/// Which top-level surface is showing in the main pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Notes,
    Graph,
}

/// Whether the open note is being read or edited. Obsidian's `Ctrl+E` toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Reading,
    Editing,
}

/// Which pane takes keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Explorer,
    Note,
    Sidebar,
    Chat,
    Graph,
}

/// The right sidebar's active tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidePanel {
    Outline,
    Backlinks,
    Tags,
}

impl SidePanel {
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Outline => "Outline",
            Self::Backlinks => "Backlinks",
            Self::Tags => "Tags",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Outline => Self::Backlinks,
            Self::Backlinks => Self::Tags,
            Self::Tags => Self::Outline,
        }
    }
}

/// An open note.
pub struct Tab {
    pub note: NoteId,
    pub mode: Mode,
    /// Scroll offset in reading mode.
    pub scroll: usize,
    /// Columns scrolled off the left in reading mode.
    ///
    /// Prose is wrapped to the pane and never needs this, but a table, a code
    /// block or a long link can be wider than the window, and squeezing them to
    /// fit makes them unreadable. They are laid out at their natural width and
    /// panned across instead.
    pub hscroll: usize,
    /// Built lazily, the first time the tab is edited.
    pub editor: Option<Editor>,
    /// Index into the note's links of the one the reading view has selected
    /// for `Enter`; `None` until the user picks one.
    pub selected_link: Option<usize>,
}

impl Tab {
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.editor.as_ref().is_some_and(Editor::is_modified)
    }
}

/// A transient message in the status bar.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub text: String,
    pub is_error: bool,
}

/// The notes the tabs showed, by path in the vault, from before an operation
/// that renumbers the index. See [`App::tab_snapshot`].
struct TabSnapshot {
    /// Per tab; `None` for a note the index did not know.
    notes: Vec<Option<String>>,
    active: Option<usize>,
    /// The back and forward trails, the same way.
    history: Vec<Option<String>>,
    forward: Vec<Option<String>>,
}

/// What a note is called, as the rename rewrites links.
struct NoteName {
    rel: String,
    stem: String,
    path: PathBuf,
}

impl NoteName {
    fn of(meta: &emeraldian_core::note::NoteMeta) -> Self {
        Self {
            rel: meta.rel.clone(),
            stem: meta.stem.clone(),
            path: meta.path.clone(),
        }
    }
}

/// The result of [`App::rename_note`].
pub struct Renamed {
    /// The note's id after the rename: ids are positions, and a rename moves
    /// them.
    pub id: NoteId,
    /// Set when the note was renamed but nvim could not follow in every
    /// respect.
    pub warning: Option<String>,
}

/// The graph view's simulation and viewport.
pub struct GraphView {
    pub simulation: Simulation,
    /// Node under the cursor.
    pub selected: Option<usize>,
    /// Center of the viewport in graph space.
    pub center: Vec2,
    pub zoom: f32,
    /// When set, the graph shows only this note's neighborhood.
    pub local_root: Option<NoteId>,
    /// World-space extent the view frames at zoom 1.
    ///
    /// Held rather than measured each frame: the layout grows while it settles,
    /// so recomputing the fit from live bounds rescales the whole picture on
    /// every tick and the graph appears to breathe. It is refreshed when the
    /// layout stops moving and whenever the user asks to refit.
    pub span: f32,
    /// Node currently held by the mouse.
    pub dragging: Option<usize>,
}

/// Zoom limits. Below the lower bound the graph is a smudge; above the upper
/// one a single node fills the pane.
pub const MIN_ZOOM: f32 = 0.2;
pub const MAX_ZOOM: f32 = 20.0;

/// Padding around the laid-out graph when the view is fitted to it.
const FIT_PADDING: f32 = 1.15;

impl GraphView {
    /// Frames the whole layout.
    ///
    /// The origin is not where the nodes end up — the layout drifts as it
    /// settles — so resetting the view has to recentre on the graph's actual
    /// bounds rather than on `(0, 0)`.
    pub fn fit(&mut self) {
        let (min_x, min_y, max_x, max_y) = self.simulation.graph.bounds();
        self.center = Vec2::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0);
        self.zoom = 1.0;
        self.refit_span();
    }

    /// Recomputes the framed extent from the layout's current bounds.
    pub fn refit_span(&mut self) {
        let (min_x, min_y, max_x, max_y) = self.simulation.graph.bounds();
        self.span = (max_x - min_x).max(max_y - min_y).max(1.0) * FIT_PADDING;
    }

    /// Centres on a node and selects it.
    pub fn focus_node(&mut self, index: usize) {
        if let Some(node) = self.simulation.graph.nodes.get(index) {
            self.center = node.pos;
            self.selected = Some(index);
        }
    }

    /// Steps the selection through the graph, centring as it goes so the tour
    /// stays on screen at any zoom level.
    pub fn cycle_selection(&mut self, delta: isize) {
        let count = self.simulation.graph.nodes.len();
        if count == 0 {
            return;
        }
        let next = match self.selected {
            Some(current) => (current as isize + delta).rem_euclid(count as isize) as usize,
            // Starting from the best-connected node makes the first Tab land
            // somewhere worth looking at.
            None => (0..count)
                .max_by_key(|&i| self.simulation.graph.nodes[i].degree)
                .unwrap_or(0),
        };
        self.focus_node(next);
    }

    /// Moves the selection to the nearest node in a direction.
    ///
    /// Nearest *overall* is the wrong answer: pressing right should reach the
    /// note to the right even when a closer one sits just above, or the arrow
    /// keys stop feeling like movement through the picture. Candidates behind
    /// the cursor are rejected outright, and the rest are scored by distance
    /// plus how far off-axis they sit, so alignment beats a small head start.
    pub fn select_in_direction(&mut self, dx: f32, dy: f32) {
        let nodes = &self.simulation.graph.nodes;
        if nodes.is_empty() {
            return;
        }
        let Some(current) = self.selected.and_then(|i| nodes.get(i)).map(|n| n.pos) else {
            // Nothing selected yet, so an arrow key means "start somewhere
            // worth looking at" — the same entry point Tab uses.
            self.cycle_selection(0);
            return;
        };

        let mut best: Option<(usize, f32)> = None;
        for (index, node) in nodes.iter().enumerate() {
            if Some(index) == self.selected {
                continue;
            }
            let ox = node.pos.x - current.x;
            let oy = node.pos.y - current.y;
            if ox * dx + oy * dy <= 0.0 {
                continue;
            }
            let off_axis = (ox * dy - oy * dx).abs();
            let score = ox.hypot(oy) + off_axis * 2.0;
            if best.is_none_or(|(_, b)| score < b) {
                best = Some((index, score));
            }
        }

        if let Some((index, _)) = best {
            self.selected = Some(index);
        }
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    }
}

/// Everything the user can ask the app to do.
///
/// Kept as data rather than closures so the command palette can list them, the
/// key map can name them, and the agent can invoke them by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Navigation
    OpenNote(NoteId),
    FollowLink(LinkTarget),
    RevealInExplorer,
    Back,
    /// Undoes a `Back`. Vim's `Ctrl+I`, against the note history.
    Forward,
    /// Moves keyboard focus to a named pane. `Ctrl+W h` and friends.
    FocusPane(Focus),

    // Notes
    NewNote,
    InsertWikiLink,
    NewFolder,
    DailyNote,
    Save,
    SaveAll,
    RenameNote,
    DeleteNote,

    // Tabs
    CloseTab,
    /// Closes the open tab and drops the unsaved edits in it: what a "close it
    /// anyway" confirmation runs.
    DiscardAndCloseTab,
    NextTab,
    PreviousTab,

    // Views
    ToggleMode,
    ToggleFocusMode,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    ToggleChat,
    ToggleRibbon,
    ToggleHints,
    /// Steps the explorer through the sort orders and remembers the choice.
    CycleSortOrder,
    /// Switches between emeraldian mode and vim mode, and writes the config.
    ToggleVimMode,
    /// Asks before putting every shortcut back on its default key.
    ResetKeys,
    /// Empties `[keys]` and writes the config. What `ResetKeys` confirms.
    ClearRemappedKeys,
    CycleSidePanel,
    OpenGraph,
    OpenLocalGraph,
    OpenNotesView,

    // Modals
    OpenPalette,
    OpenSwitcher,
    OpenSearch,
    /// `Ctrl+F`: the lines of the open note that contain what is typed.
    OpenFind,
    /// Jumps to `text`, found at `column` of `line` in the open note, and
    /// selects it when the note is being edited.
    GotoLine {
        line: usize,
        column: usize,
        text: String,
    },
    /// `Ctrl+Shift+T`, as in a browser.
    ReopenClosedTab,
    OpenThemePicker,
    OpenVaultPicker,
    OpenHelp,
    /// The list of backends the app knows how to reach.
    OpenProviderPicker,
    /// Asks the current provider what models it has, then offers them.
    OpenModelPicker,
    /// Types in an API key for the current provider.
    PromptApiKey,

    // Settings
    SetTheme(String),
    /// Switch backend, taking the endpoint and default model with it.
    SetProvider(String),
    SetModel(String),
    OpenVault(PathBuf),
    /// `OpenVault` after the question about unsaved edits was answered.
    ForceOpenVault(PathBuf),
    ToggleLineNumbers,
    ToggleGraphLabels,
    ToggleGraphUnresolved,
    ToggleGraphTags,
    ToggleGraphAttachments,
    ToggleGraphOrphans,

    // Vault
    /// Hand the open note to the Obsidian desktop app, via its CLI.
    OpenInObsidian,
    Refresh,
    SaveSettings,
    /// Asks for confirmation first; `ForceQuit` is what actually exits.
    Quit,
    ForceQuit,
}

/// Where each interactive element was drawn, recorded every frame.
///
/// The renderer knows the geometry; the mouse handler needs it. Rather than
/// duplicating the layout arithmetic in two places and letting them drift, the
/// draw pass writes down what it put where and clicks are resolved against
/// that.
#[derive(Debug, Clone, Default)]
pub struct Regions {
    /// Ribbon icons and the action each one runs.
    pub ribbon: Vec<(Rect, Action)>,
    /// The explorer's inner area, and the row index its first line shows.
    pub explorer: Option<(Rect, usize)>,
    /// Document tabs, with the index each one selects.
    pub tabs: Vec<(Rect, usize)>,
    /// The outline/backlinks/tags selector.
    pub side_tabs: Vec<(Rect, SidePanel)>,
    /// The sidebar's list area and the row index its first line shows.
    pub sidebar: Option<(Rect, usize)>,
    /// The editor's text column — gutter and scrollbar excluded — and the row
    /// its first line shows.
    ///
    /// Wrapping means the cursor's position depends on how wide the text was
    /// drawn, so keys that move between rows read this too, not just the mouse.
    pub editor: Option<(Rect, usize)>,
    /// The area the embedded Neovim's grid is drawn into, which is also the
    /// size nvim is told its screen has.
    pub nvim: Option<Rect>,
    /// Which rendered row each block of the reading pane started on, as
    /// `(source line, row)`, so the outline can scroll to a heading.
    pub anchors: Vec<(usize, usize)>,
    pub main: Option<Rect>,
    pub chat: Option<Rect>,
    /// The graph canvas with its coordinate bounds, for hit-testing nodes.
    pub graph: Option<(Rect, [f64; 2], [f64; 2])>,
    /// The whole screen, which overlays are sized against.
    pub screen: Option<Rect>,
}

/// Text found in a note, for the reading view to scroll to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reveal {
    /// The source line it is on.
    pub line: usize,
    pub text: String,
    /// How many times it occurs before this one in the same block. A
    /// paragraph or a code block can show the same words several times, and
    /// it is the one picked that should come into view.
    pub nth: usize,
}

pub struct App {
    pub index: VaultIndex,
    pub config: Config,
    pub theme: ActiveTheme,
    /// Every theme available in the picker, built-ins plus user files.
    pub themes: Vec<emeraldian_theme::Theme>,

    pub explorer: Explorer,
    pub tabs: Vec<Tab>,
    pub active_tab: Option<usize>,
    /// Recently visited notes, newest last — powers `Back`.
    pub history: Vec<NoteId>,
    /// Notes stepped back *from*, newest last — powers `Forward`.
    ///
    /// The browser rule: `Back` fills this, and opening a note any other way
    /// clears it, because once you have gone somewhere new there is no longer a
    /// forward to return to.
    pub forward: Vec<NoteId>,
    /// Notes whose tabs were closed, newest last, by path so that a rename or
    /// a reload in between can't point one at the wrong note.
    pub closed: Vec<String>,
    /// Text the reading view should bring into sight on its next frame. Only
    /// a drawn frame knows which row that is.
    pub reveal: Option<Reveal>,

    pub view: View,
    pub focus: Focus,
    pub side_panel: SidePanel,
    pub side_selected: usize,
    pub graph: Option<GraphView>,

    /// Hit regions from the last frame, for mouse input.
    pub regions: Regions,
    /// The overlay on top of everything, if any.
    pub modal: Option<crate::modal::Modal>,
    pub chat: Chat,
    /// API keys typed in rather than exported. Read once at startup, since it is
    /// only ever written by this app.
    pub auth: crate::auth::Auth,
    /// A model list on its way back from a provider.
    pub lookup: crate::agent::Lookup,
    pub status: Status,
    /// Pictures drawn in the reading pane. Starts switched off, and is replaced
    /// once the terminal has been asked what it can draw — a question that has
    /// to be put before the alternate screen is entered.
    pub images: crate::images::Images,
    /// The Excalidraw scene currently on screen, kept parsed between frames.
    pub scenes: crate::ui::drawing::Scenes,
    /// The note last laid out for reading, reused while nothing it depends on
    /// has changed.
    pub reading: crate::ui::note::ReadingCache,
    /// Modal-editing state. Inert unless `config.editor.vim` is on.
    ///
    /// One per app rather than one per tab: only one editor has focus at a
    /// time, and a register shared across tabs is what vim does — yanking in
    /// one note and putting it in another is the whole point.
    pub vim: crate::vim::Vim,
    /// The `[keys]` section, read once at startup.
    pub keymap: crate::keymap::Keymap,
    /// Where `config.toml` and `state.json` are written, when it isn't the
    /// real config directory.
    ///
    /// `None` everywhere but in tests. Toggling vim mode writes the config
    /// immediately, so without a seam every test that presses `F4` would edit
    /// the settings of whoever ran it — the same hazard `OTUI_STATE_FILE`
    /// exists for, expressed as a field because the crate forbids the `unsafe`
    /// that setting an environment variable now needs.
    pub config_dir: Option<PathBuf>,
    /// Just the note on screen, the sidebars hidden: `F8`.
    ///
    /// Kept apart from the `show_*` settings rather than switching them off,
    /// so leaving it puts back exactly what was there, and saving settings
    /// while in it doesn't save an empty layout.
    pub focus_mode: bool,
    /// The embedded Neovim, started the first time a note is edited with
    /// `[editor] backend = "nvim"`. One per app; every tab being edited is one
    /// of its buffers.
    pub nvim: Option<crate::nvim::session::Session>,
    /// Neovim could not be started, so the built-in editor is used instead.
    /// Kept for the rest of the run: trying again on every frame would block
    /// the loop on a program that is not there.
    pub nvim_failed: bool,
    pub quit: bool,
}

impl App {
    /// Builds the app over a vault.
    pub fn new(vault: Vault, config: Config) -> Result<Self, std::io::Error> {
        let scan = ScanOptions {
            include_hidden: config.ui.show_hidden,
            ..Default::default()
        };
        let index = VaultIndex::build(vault, scan)?;

        let mut themes = presets::builtin();
        if let Some(dir) = Config::themes_dir() {
            let (custom, _errors) = presets::load_custom(&dir);
            themes.extend(custom);
        }
        let theme = themes
            .iter()
            .find(|t| t.name == config.theme)
            .cloned()
            .unwrap_or_else(presets::default_theme);

        let chat = Chat::new(&config.agent);
        let mut explorer = Explorer::default();
        explorer.set_sort(config.ui.sort_order());
        let mut app = Self {
            explorer,
            tabs: Vec::new(),
            active_tab: None,
            history: Vec::new(),
            closed: Vec::new(),
            reveal: None,
            forward: Vec::new(),
            view: View::Notes,
            focus: Focus::Explorer,
            side_panel: SidePanel::Outline,
            side_selected: 0,
            graph: None,
            regions: Regions::default(),
            modal: None,
            chat,
            auth: crate::auth::Auth::load(),
            lookup: crate::agent::Lookup::default(),
            status: Status::default(),
            images: crate::images::Images::disabled(),
            scenes: crate::ui::drawing::Scenes::default(),
            reading: crate::ui::note::ReadingCache::default(),
            vim: crate::vim::Vim::default(),
            keymap: crate::keymap::Keymap::new(&config.keys),
            config_dir: None,
            focus_mode: false,
            nvim: None,
            nvim_failed: false,
            quit: false,
            theme: ActiveTheme::new(theme),
            themes,
            config,
            index,
        };

        // A vault opens with its folders shut, so the explorer is a short list
        // of top-level groupings rather than every note at once. `main` calls
        // `restore_ui_state` straight after to reopen whatever was left open;
        // constructing an `App` deliberately touches no disk beyond the vault,
        // so tests aren't steered by whatever is in the real config directory.
        app.explorer.collapse_all(&app.index);
        app.explorer.rebuild(&app.index);
        Ok(app)
    }

    /// Writes the settings, wherever this app has been told to keep them.
    pub fn save_config(&self) -> std::io::Result<PathBuf> {
        match &self.config_dir {
            Some(dir) => self.config.save_to(&dir.join("config.toml")),
            None => self.config.save(),
        }
    }

    /// Writes one change into the config file, leaving the rest of it exactly
    /// as it was. With no file yet, the whole config is written instead, so
    /// the first one is the documented file `ensure_exists` would have made.
    pub fn edit_config(
        &self,
        edit: impl FnOnce(&mut toml_edit::DocumentMut),
    ) -> std::io::Result<PathBuf> {
        let path = match &self.config_dir {
            Some(dir) => dir.join("config.toml"),
            None => Config::path().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no config directory on this platform",
                )
            })?,
        };
        if path.exists() {
            Config::edit_file(&path, edit)
        } else {
            self.config.save_to(&path)
        }
    }

    /// Reads the persistent UI state from wherever this app keeps it.
    pub fn load_state(&self) -> crate::state::State {
        match &self.config_dir {
            Some(dir) => crate::state::State::load_from(&dir.join("state.json")),
            None => crate::state::State::load(),
        }
    }

    /// The counterpart write.
    pub fn store_state(&self, state: &crate::state::State) {
        match &self.config_dir {
            Some(dir) => state.save_to(&dir.join("state.json")),
            None => state.save(),
        }
    }

    /// Reopens the folders left open last time this vault was used.
    ///
    /// A vault with no saved entry keeps the collapsed-by-default state set up
    /// in `new`, which is what a first run should look like.
    pub fn restore_ui_state(&mut self) {
        self.apply_ui_state(&crate::state::State::load());
    }

    /// Writes down how the explorer was left, for the next run.
    pub fn save_ui_state(&self) {
        let mut state = crate::state::State::load();
        self.record_ui_state(&mut state);
        state.save();
    }

    /// The same pair, against a named file rather than the default one, so a
    /// test can exercise the round trip without touching the real config
    /// directory or the process environment.
    #[cfg(test)]
    pub fn restore_ui_state_from(&mut self, path: &std::path::Path) {
        self.apply_ui_state(&crate::state::State::load_from(path));
    }

    #[cfg(test)]
    pub fn save_ui_state_to(&self, path: &std::path::Path) {
        let mut state = crate::state::State::load_from(path);
        self.record_ui_state(&mut state);
        state.save_to(path);
    }

    fn apply_ui_state(&mut self, state: &crate::state::State) {
        let Some(expanded) = state
            .vault(&self.index.vault.path)
            .map(|saved| saved.expanded_folders.clone())
        else {
            return;
        };
        self.explorer.set_expanded(&expanded, &self.index);
    }

    fn record_ui_state(&self, state: &mut crate::state::State) {
        state.set_vault(
            &self.index.vault.path,
            crate::state::VaultState {
                expanded_folders: self.explorer.expanded(&self.index),
            },
        );
    }

    // ---- accessors -------------------------------------------------------

    #[must_use]
    pub fn active(&self) -> Option<&Tab> {
        self.active_tab.and_then(|i| self.tabs.get(i))
    }

    #[must_use]
    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        match self.active_tab {
            Some(index) => self.tabs.get_mut(index),
            None => None,
        }
    }

    #[must_use]
    pub fn active_note(&self) -> Option<NoteId> {
        self.active().map(|t| t.note)
    }

    /// Whether the open note is being edited rather than read.
    ///
    /// Vim's modes only exist inside the editor, so several places need to ask
    /// this before deciding a key or a label means anything vim-related.
    #[must_use]
    pub fn editing(&self) -> bool {
        self.active().is_some_and(|tab| tab.mode == Mode::Editing)
    }

    /// Whether the open note is being edited in the embedded Neovim rather
    /// than in the built-in editor.
    #[must_use]
    pub fn nvim_editing(&self) -> bool {
        self.config.editor.backend == crate::config::EditorBackend::Nvim
            && !self.nvim_failed
            && self.view == View::Notes
            && self.editing()
    }

    /// Whether the tab at `index` has changes that are not on disk.
    ///
    /// Asks nvim when it holds the note, so it is right at the moment a close,
    /// a quit or a write is decided; drawing uses [`App::tab_marked_modified`].
    #[must_use]
    pub fn tab_modified(&self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        tab.is_modified() || self.nvim_buffer_modified(tab.note)
    }

    /// [`App::tab_modified`] for drawing: cheap, and a few milliseconds behind
    /// what nvim holds.
    #[must_use]
    pub fn tab_marked_modified(&self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        tab.is_modified()
            || self
                .note_path(tab.note)
                .zip(self.nvim.as_ref())
                .is_some_and(|(path, session)| session.shows_modified(&path))
    }

    /// How many notes have changes that are not on disk: the tabs', and those
    /// of buffers nvim holds for a file that has no tab (a file the index does
    /// not know, opened with `:edit`).
    #[must_use]
    pub fn unsaved_count(&self) -> usize {
        let in_tabs = (0..self.tabs.len())
            .filter(|&index| self.tab_modified(index))
            .count();
        let Some(session) = self.nvim.as_ref() else {
            return in_tabs;
        };
        let shown: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|tab| self.note_path(tab.note))
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
            .collect();
        in_tabs
            + session
                .modified_notes()
                .iter()
                .filter(|path| !shown.contains(path))
                .count()
    }

    /// Whether nvim holds unsaved edits to the note. The built-in editor's
    /// edits are not asked about here: its tabs carry them themselves.
    #[must_use]
    pub fn nvim_buffer_modified(&self, id: NoteId) -> bool {
        self.note_path(id)
            .zip(self.nvim.as_ref())
            .is_some_and(|(path, session)| session.is_modified(&path))
    }

    /// The file behind a note.
    fn note_path(&self, id: NoteId) -> Option<PathBuf> {
        self.index.note(id).map(|note| note.meta.path.clone())
    }

    #[must_use]
    pub fn note_title(&self, id: NoteId) -> String {
        self.index
            .note(id)
            .map(|n| n.meta.title.clone())
            .unwrap_or_else(|| "(missing)".into())
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.status = Status {
            text: message.into(),
            is_error: false,
        };
    }

    /// Reports a yank, which is otherwise completely invisible.
    ///
    /// `yy` changes nothing on screen, so without a word in the status bar it
    /// is indistinguishable from a key that did nothing at all.
    pub fn info_yank(&mut self, lines: usize) {
        self.info(if lines == 1 {
            "yanked 1 line".to_string()
        } else {
            format!("yanked {lines} lines")
        });
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.status = Status {
            text: message.into(),
            is_error: true,
        };
    }

    // ---- tabs ------------------------------------------------------------

    /// Opens a note, reusing an existing tab if it's already open.
    pub fn open_note(&mut self, id: NoteId) {
        if self.index.note(id).is_none() {
            self.error("that note no longer exists");
            return;
        }

        if let Some(current) = self.active_note()
            && current != id
        {
            self.history.push(current);
            // A long session shouldn't accumulate unbounded history.
            if self.history.len() > 100 {
                self.history.remove(0);
            }
            // Navigating somewhere new ends any forward trail, as it does in a
            // browser. `Back` puts its own entry back afterwards.
            self.forward.clear();
        }

        if let Some(existing) = self.tabs.iter().position(|t| t.note == id) {
            self.active_tab = Some(existing);
        } else {
            self.tabs.push(Tab {
                note: id,
                mode: if self.config.ui.reading_mode {
                    Mode::Reading
                } else {
                    Mode::Editing
                },
                scroll: 0,
                hscroll: 0,
                editor: None,
                selected_link: None,
            });
            self.active_tab = Some(self.tabs.len() - 1);
        }

        self.view = View::Notes;
        // The graph is laid out afresh whenever it opens, so once a note has
        // replaced it there is nothing to keep it for.
        self.graph = None;
        self.focus = Focus::Note;
        self.side_selected = 0;
        // A note opens in Normal mode, never mid-insert: arriving in a fresh
        // note with typing already live is how you edit the wrong file.
        self.vim.reset();
    }

    /// Opens the note a link leads to, creating it if missing.
    ///
    /// Following a link to a note that doesn't exist yet is how notes get
    /// written in Obsidian, so an unresolved target creates rather than errors.
    /// A relative markdown link is read from the folder of the note it was
    /// written in, and an anchor is jumped to once the note is being edited in
    /// nvim.
    pub fn open_or_create(&mut self, target: &LinkTarget) {
        let relative = target.source.as_ref().and_then(|source| {
            crate::nvim::bridge::relative_to(source, &self.index.vault.path, &target.note)
        });
        let name = match &relative {
            Some(rel) if self.index.resolve(rel).is_some() => rel.clone(),
            // Nothing there: a link such as `[x](Beta.md)` may still name a
            // note elsewhere in the vault, which is how Obsidian reads it.
            Some(rel) if self.index.resolve(&target.note).is_none() => rel.clone(),
            _ => target.note.clone(),
        };
        if let Some(id) = self.index.resolve(&name) {
            self.open_note(id);
            self.jump_to_anchor(target);
            return;
        }
        // The heading is the note's name, not its path — `Daily/2026-07-27`
        // should open with `# 2026-07-27`.
        let title = name.rsplit('/').next().unwrap_or(&name);
        let title = title.strip_suffix(".md").unwrap_or(title);
        match self.create_note(&name, &format!("# {title}\n\n")) {
            Ok(id) => {
                self.explorer.rebuild(&self.index);
                self.open_note(id);
                self.info(format!("created {name}"));
            }
            Err(err) => self.error(format!("could not create {name}: {err}")),
        }
    }

    /// Moves the embedded nvim to the heading or block a link named, if the
    /// note it opened is being edited there. A jump for a note that is only
    /// read would sit queued until the next time it is edited, so it is not
    /// sent.
    pub fn jump_to_anchor(&mut self, target: &LinkTarget) {
        let Some(anchor) = target.anchor.clone() else {
            return;
        };
        if !self.nvim_editing() {
            return;
        }
        let Some(path) = self
            .active_note()
            .and_then(|id| self.index.note(id))
            .map(|note| note.meta.path.clone())
        else {
            return;
        };
        if let Some(session) = self.nvim.as_mut() {
            session.goto(&path, anchor);
        }
    }

    /// Closes the open tab. A note with unsaved edits is saved first when
    /// auto-save is on and stays open if that fails; without auto-save the
    /// edits nvim holds are asked about (see [`Action::DiscardAndCloseTab`]).
    pub fn close_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        if self.tab_modified(index) {
            if self.config.editor.auto_save {
                if !self.try_save_tab(index) {
                    return;
                }
            } else {
                let title = self.note_title(self.tabs[index].note);
                self.modal = Some(Modal::Confirm(Confirm {
                    message: format!("\"{title}\" has unsaved changes. Close it and discard them?"),
                    action: Action::DiscardAndCloseTab,
                }));
                return;
            }
        }
        self.remove_tab(index, false);
    }

    /// Closes the open tab and drops its unsaved edits, for after the user
    /// said so.
    pub fn close_tab_discarding(&mut self) {
        if let Some(index) = self.active_tab {
            self.remove_tab(index, true);
        }
    }

    /// Whether nvim, and not the built-in editor, is what edits the tab.
    fn tab_is_in_nvim(&self, index: usize) -> bool {
        self.nvim.is_some() && self.tabs.get(index).is_some_and(|tab| tab.editor.is_none())
    }

    fn remove_tab(&mut self, index: usize, discard: bool) {
        // The buffer goes first: if nvim keeps it (edits typed since the check),
        // so does the tab, and nothing is written back to the note later by a
        // buffer nobody can see.
        if let Some(path) = self.note_path(self.tabs[index].note)
            && let Some(session) = self.nvim.as_mut()
            && let Err(err) = session.close(&path, discard)
        {
            self.error(format!("could not close the note in nvim: {err}"));
            return;
        }
        let closed = self.tabs.remove(index);
        if let Some(note) = self.index.note(closed.note) {
            self.closed.push(note.meta.rel.clone());
            // Enough to undo a run of closes; not a second history.
            if self.closed.len() > 20 {
                self.closed.remove(0);
            }
        }
        self.active_tab = if self.tabs.is_empty() {
            None
        } else {
            Some(index.min(self.tabs.len() - 1))
        };
        if self.tabs.is_empty() {
            self.focus = Focus::Explorer;
        }
    }

    pub fn cycle_tab(&mut self, delta: isize) {
        if self.tabs.is_empty() {
            return;
        }
        let count = self.tabs.len() as isize;
        let current = self.active_tab.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(count);
        self.active_tab = Some(next as usize);
        self.side_selected = 0;
        self.vim.reset();
    }

    /// The editor for the active tab, created on first use.
    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        let index = self.active_tab?;
        let note = self.tabs[index].note;
        if self.tabs[index].editor.is_none() {
            let text = self.index.read(note).unwrap_or_default();
            self.tabs[index].editor = Some(Editor::new(
                &text,
                self.config.editor.tab_width,
                self.config.editor.expand_tabs,
            ));
        }
        self.tabs[index].editor.as_mut()
    }

    pub fn save_tab(&mut self, index: usize) {
        self.try_save_tab(index);
    }

    /// Writes the tab's note. `true` when there is nothing left unsaved in it:
    /// it was written, or there was nothing to write.
    pub fn try_save_tab(&mut self, index: usize) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return true;
        };
        let note = tab.note;
        if self.tab_is_in_nvim(index) {
            return self.save_nvim_note(note);
        }
        let Some(text) = tab.editor.as_ref().map(Editor::text) else {
            return true;
        };
        match self.index.write_note(note, &text) {
            Ok(()) => {
                if let Some(editor) = self.tabs[index].editor.as_mut() {
                    editor.mark_saved();
                }
                let title = self.note_title(note);
                self.info(format!("saved {title}"));
                self.explorer.rebuild(&self.index);
                true
            }
            Err(err) => {
                self.error(format!("save failed: {err}"));
                false
            }
        }
    }

    /// Has nvim write the note's buffer and reads the file into the index, so
    /// that the reading view, the backlinks and the graph show what was typed.
    fn save_nvim_note(&mut self, note: NoteId) -> bool {
        let Some(path) = self.note_path(note) else {
            return true;
        };
        let Some(session) = self.nvim.as_mut() else {
            return true;
        };
        if let Err(err) = session.save(&path) {
            self.error(format!("save failed: {err}"));
            return false;
        }
        if let Err(err) = self.index.refresh_note(note) {
            self.error(format!("saved, but could not read the note again: {err}"));
            return true;
        }
        let title = self.note_title(note);
        self.info(format!("saved {title}"));
        self.explorer.rebuild(&self.index);
        true
    }

    // ---- graph -----------------------------------------------------------

    fn graph_options(&self) -> GraphOptions {
        GraphOptions {
            show_unresolved: self.config.graph.show_unresolved,
            show_tags: self.config.graph.show_tags,
            show_attachments: self.config.graph.show_attachments,
            show_orphans: self.config.graph.show_orphans,
            tag_filter: None,
        }
    }

    /// Opens the graph, optionally restricted to one note's neighborhood.
    pub fn open_graph(&mut self, local_root: Option<NoteId>) {
        let mut graph = Graph::build(&self.index, &self.graph_options());

        // The local graph is cut down to a graph of its own before anything is
        // laid out, so the neighbourhood settles into the pane it will be shown
        // in. Filtering at draw time instead leaves it laid out for a picture
        // it never gets, and drawn with edges running off to nodes that aren't
        // there.
        if let Some(root) = local_root.and_then(|id| graph.node_of_note(id)) {
            let neighborhood = graph.neighborhood(root, self.config.graph.local_depth);
            graph = graph.subgraph(&neighborhood);
        }

        let mut simulation = Simulation::new(graph);
        // Most of the layout runs before the first frame, so the graph opens
        // readable rather than as an exploding cloud. What's deliberately left
        // is a short tail — about a second of visibly decaying motion — which
        // reads as the graph arranging itself rather than as a jump cut.
        simulation.run(200);

        let selected = local_root.and_then(|id| simulation.graph.node_of_note(id));
        // Centre on the focused note if there is one, else on the middle of the
        // laid-out graph — the origin is not where the nodes end up.
        let center = selected
            .and_then(|n| simulation.graph.nodes.get(n))
            .map(|n| n.pos)
            .unwrap_or_else(|| {
                let (min_x, min_y, max_x, max_y) = simulation.graph.bounds();
                Vec2::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0)
            });

        let mut view = GraphView {
            center,
            simulation,
            selected,
            zoom: 1.0,
            local_root,
            span: 1.0,
            dragging: None,
        };
        view.refit_span();

        self.graph = Some(view);
        self.view = View::Graph;
        self.focus = Focus::Graph;
    }

    /// Rebuilds the graph in place, keeping the viewport.
    pub fn refresh_graph(&mut self) {
        if self.graph.is_none() {
            return;
        }
        let local_root = self.graph.as_ref().and_then(|g| g.local_root);
        let (center, zoom) = self
            .graph
            .as_ref()
            .map_or((Vec2::default(), 1.0), |g| (g.center, g.zoom));
        self.open_graph(local_root);
        if let Some(graph) = self.graph.as_mut() {
            graph.center = center;
            graph.zoom = zoom;
        }
    }

    // ---- vault -----------------------------------------------------------

    /// Re-reads the whole vault from disk.
    pub fn refresh(&mut self) {
        let snapshot = self.tab_snapshot();
        if let Err(err) = self.index.rebuild() {
            self.error(format!("refresh failed: {err}"));
            return;
        }
        // A reload is the one moment the user has said the files on disk have
        // changed, so it is also when an edited picture should be read again.
        self.images.forget();
        self.after_renumbering(snapshot, None);
        // A reload can bring in notes the trail never knew; it starts over.
        self.history.clear();
        self.forward.clear();
        // And the notes nvim shows: those without unsaved edits show the files
        // again.
        if let Some(session) = self.nvim.as_mut() {
            session.checktime();
        }
    }

    /// The notes the tabs show, by their path in the vault.
    ///
    /// Note ids are positions in the sorted list, so anything holding one must
    /// be re-resolved after an operation that rebuilds or reshapes the index,
    /// or it points at whatever note moved into its place. Taken *before* that
    /// operation: afterwards the ids can no longer be told what they were.
    fn tab_snapshot(&self) -> TabSnapshot {
        let rel_of = |id: &NoteId| self.index.note(*id).map(|n| n.meta.rel.clone());
        TabSnapshot {
            notes: self.tabs.iter().map(|tab| rel_of(&tab.note)).collect(),
            active: self.active_tab,
            history: self.history.iter().map(rel_of).collect(),
            forward: self.forward.iter().map(rel_of).collect(),
        }
    }

    /// Puts the tabs and the back/forward trail back on the notes `snapshot`
    /// names, with the ids the index has now. `renamed` is a note whose path
    /// changed, `(from, to)`; a note that is gone loses its tab and its place
    /// in the trail.
    fn restore_tabs(&mut self, snapshot: TabSnapshot, renamed: Option<(&str, &str)>) {
        let resolve = |index: &VaultIndex, rel: String| {
            let rel = match renamed {
                Some((from, to)) if rel == from => to.to_string(),
                _ => rel,
            };
            index.id_of_rel(&rel)
        };
        let mut tabs: Vec<Tab> = Vec::new();
        let mut active = None;
        for (position, (tab, rel)) in std::mem::take(&mut self.tabs)
            .into_iter()
            .zip(snapshot.notes)
            .enumerate()
        {
            let Some(id) = rel.and_then(|rel| resolve(&self.index, rel)) else {
                continue;
            };
            if snapshot.active == Some(position) {
                active = Some(tabs.len());
            }
            tabs.push(Tab {
                note: id,
                selected_link: None,
                ..tab
            });
        }
        self.active_tab = active.or(if tabs.is_empty() { None } else { Some(0) });
        self.tabs = tabs;
        self.history = snapshot
            .history
            .into_iter()
            .filter_map(|rel| resolve(&self.index, rel?))
            .collect();
        self.forward = snapshot
            .forward
            .into_iter()
            .filter_map(|rel| resolve(&self.index, rel?))
            .collect();
    }

    /// Everything that holds note ids, after the index was rebuilt.
    fn after_renumbering(&mut self, snapshot: TabSnapshot, renamed: Option<(&str, &str)>) {
        self.restore_tabs(snapshot, renamed);
        self.explorer.rebuild(&self.index);
        if self.graph.is_some() {
            self.refresh_graph();
        }
    }

    /// Creates a note. Notes are kept sorted, so one that sorts before the open
    /// tabs' notes moves their ids; the tabs are put back on their notes.
    pub fn create_note(
        &mut self,
        name: &str,
        content: &str,
    ) -> emeraldian_core::error::Result<NoteId> {
        let snapshot = self.tab_snapshot();
        let id = self.index.create_note(name, content)?;
        self.restore_tabs(snapshot, None);
        Ok(id)
    }

    /// Renames a note and everything that shows it: the tabs follow it, links
    /// to it are rewritten in the files and in the buffers nvim holds, and the
    /// note's own buffer moves to the new path.
    pub fn rename_note(&mut self, id: NoteId, new_name: &str) -> Result<Renamed, String> {
        let Some(old) = self.index.note(id).map(|note| NoteName::of(&note.meta)) else {
            return Err("that note no longer exists".to_string());
        };
        let snapshot = self.tab_snapshot();
        // Taken now: what a rename rewrites in the files cannot reach text
        // that only exists in nvim.
        let unsaved = self.unsaved_texts();

        let new_id = self
            .index
            .rename_note(id, new_name)
            .map_err(|err| err.to_string())?;
        let Some(new) = self.index.note(new_id).map(|note| NoteName::of(&note.meta)) else {
            return Err("the renamed note is gone".to_string());
        };
        if new.rel == old.rel {
            return Ok(Renamed {
                id: new_id,
                warning: None,
            });
        }
        self.after_renumbering(snapshot, Some((&old.rel, &new.rel)));
        let warning = self.move_nvim_buffers(&old, &new, unsaved);
        Ok(Renamed {
            id: new_id,
            warning,
        })
    }

    /// The text of every note nvim holds unsaved edits to: where it is, and
    /// what it says.
    fn unsaved_texts(&self) -> Vec<(String, PathBuf, String)> {
        let Some(session) = self.nvim.as_ref() else {
            return Vec::new();
        };
        self.tabs
            .iter()
            .filter_map(|tab| {
                let note = &self.index.note(tab.note)?.meta;
                let text = session
                    .is_modified(&note.path)
                    .then(|| session.buffer_text(&note.path))??;
                Some((note.rel.clone(), note.path.clone(), text))
            })
            .collect()
    }

    /// After a rename on disk: has nvim follow it. The renamed note's buffer
    /// takes the new name, links to it are rewritten in the buffers with unsaved
    /// edits (the files were rewritten, the buffers cannot be), and the buffers
    /// without are reloaded. `Some(text)` when nvim could not do all of it.
    fn move_nvim_buffers(
        &mut self,
        old: &NoteName,
        new: &NoteName,
        unsaved: Vec<(String, PathBuf, String)>,
    ) -> Option<String> {
        let session = self.nvim.as_mut()?;
        let mut problems = Vec::new();
        if let Err(err) = session.rename(&old.path, &new.path) {
            problems.push(format!("could not move the note's buffer: {err}"));
        }
        for (rel, path, text) in unsaved {
            // A diagram's source has no wikilinks in it; `A[[Old]]` is a node.
            if emeraldian_core::note::own_extension(&rel).is_some() {
                continue;
            }
            let path = if rel == old.rel {
                new.path.clone()
            } else {
                path
            };
            let rewritten = emeraldian_core::ops::rewrite_wikilinks(
                &text, &old.stem, &new.stem, &old.rel, &new.rel,
            );
            if rewritten != text
                && let Err(err) = session.set_lines(&path, &rewritten)
            {
                problems.push(format!("could not rewrite the links in {rel}: {err}"));
            }
        }
        session.checktime();
        (!problems.is_empty()).then(|| problems.join("; "))
    }

    /// Moves a note to the vault's trash and closes everything that showed it.
    pub fn delete_note(&mut self, id: NoteId) -> Result<std::path::PathBuf, String> {
        let Some(path) = self.note_path(id) else {
            return Err("that note no longer exists".to_string());
        };
        let snapshot = self.tab_snapshot();
        let trashed = self.index.delete_note(id).map_err(|err| err.to_string())?;
        // The note is gone, and so are the edits in it: the buffer must not
        // survive to write it back.
        let closed = self
            .nvim
            .as_mut()
            .map_or(Ok(()), |session| session.close(&path, true));
        self.after_renumbering(snapshot, None);
        match closed {
            Ok(()) => Ok(trashed),
            Err(err) => {
                self.error(format!("moved to trash, but nvim kept the buffer: {err}"));
                Ok(trashed)
            }
        }
    }

    /// The one question before anything that throws unsaved text away: when
    /// auto-save is off and there is text that is not on disk, in a tab or in
    /// a buffer nvim holds, asks `lead` plus what is at stake and runs `then`
    /// only once that is confirmed; `false` then. `true` when `then` may run
    /// at once: nothing is unsaved, or auto-save is on, in which case `then`
    /// writes the text first and stays where it is if that fails.
    ///
    /// Every path that ends the nvim session or drops tabs on purpose goes
    /// through here, so that a new one cannot forget to ask.
    #[must_use]
    pub fn confirm_unsaved(&mut self, lead: &str, then: Action) -> bool {
        if self.config.editor.auto_save {
            return true;
        }
        let message = match self.unsaved_count() {
            0 => return true,
            1 => format!("{lead} 1 note has unsaved changes."),
            n => format!("{lead} {n} notes have unsaved changes."),
        };
        self.modal = Some(Modal::Confirm(Confirm {
            message,
            action: then,
        }));
        false
    }

    /// Writes every tab that has unsaved edits. `false` (with the reason in
    /// the status bar, prefixed by `refusal`) when one cannot be written.
    pub fn save_modified_tabs(&mut self, refusal: &str) -> bool {
        for index in 0..self.tabs.len() {
            if self.tab_modified(index) && !self.try_save_tab(index) {
                let title = self.note_title(self.tabs[index].note);
                self.error(format!(
                    "{refusal}: \"{title}\" could not be saved ({})",
                    self.status.text
                ));
                return false;
            }
        }
        true
    }

    /// Switches to another vault, keeping settings and theme. Asks first
    /// while there are unsaved edits that auto-save would not write.
    pub fn open_vault(&mut self, path: PathBuf) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let lead = format!("Open vault {name}?");
        if self.confirm_unsaved(&lead, Action::ForceOpenVault(path.clone())) {
            self.switch_vault(path);
        }
    }

    /// [`App::open_vault`] after the question: with auto-save on the edits are
    /// written first, and whatever cannot be written keeps the switch from
    /// happening; with it off they are dropped, the user having said so.
    pub fn switch_vault(&mut self, path: PathBuf) {
        // Nothing is torn down until the new vault is known to open.
        let scan = ScanOptions {
            include_hidden: self.config.ui.show_hidden,
            ..Default::default()
        };
        let built = VaultIndex::build(Vault::from_path(&path), scan);
        if let Err(err) = &built {
            self.error(format!("could not open vault: {err}"));
            return;
        }
        if self.config.editor.auto_save && !self.save_modified_tabs("not switching vaults") {
            return;
        }
        // nvim runs in the old vault's folder and knows its notes by their
        // paths; the new vault gets a session of its own, started when a note
        // is edited. Not before the edits in it are safe.
        if let Err(err) = crate::nvim::end_session(self) {
            self.error(format!("not switching vaults: {err}"));
            return;
        }
        match built {
            Ok(index) => {
                self.index = index;
                self.tabs.clear();
                self.active_tab = None;
                self.history.clear();
                self.forward.clear();
                // Paths, but of the other vault, where they may name other notes.
                self.closed.clear();
                self.graph = None;
                self.view = View::Notes;
                self.focus = Focus::Explorer;
                // Keep the chosen order across a vault switch; it's a
                // preference about the app, not about the vault.
                let sort = self.explorer.sort();
                self.explorer = Explorer::default();
                self.explorer.set_sort(sort);
                self.explorer.rebuild(&self.index);
                let name = self.index.vault.name.clone();
                self.info(format!("opened vault {name}"));
            }
            Err(err) => self.error(format!("could not open vault: {err}")),
        }
    }

    pub fn set_theme(&mut self, name: &str) {
        if let Some(theme) = self.themes.iter().find(|t| t.name == name).cloned() {
            self.theme = ActiveTheme::new(theme);
            self.config.theme = name.to_string();
            self.info(format!("theme: {name}"));
        } else {
            self.error(format!("no theme named {name}"));
        }
    }

    /// Today's daily note, created if needed.
    pub fn daily_note_name(&self) -> String {
        let name = format_date(&self.config.editor.daily_format);
        let folder = self.config.editor.daily_folder.trim_matches('/');
        if folder.is_empty() {
            name
        } else {
            format!("{folder}/{name}")
        }
    }
}

/// Formats today's date, supporting the `%Y`/`%m`/`%d` placeholders that cover
/// every daily-note format in practice.
///
/// Computed from the system clock with a plain civil-date conversion rather
/// than pulling in a date library for three substitutions.
fn format_date(format: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    format
        .replace("%Y", &format!("{year:04}"))
        .replace("%m", &format!("{month:02}"))
        .replace("%d", &format!("{day:02}"))
}

/// Days since the Unix epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emeraldian_core::test_support::TempVault;

    fn app(vault: &TempVault) -> App {
        App::new(vault.vault(), Config::default()).expect("build app")
    }

    fn sample() -> TempVault {
        let vault = TempVault::new("app");
        vault.write("A.md", "# A\n\nlinks to [[B]]\n");
        vault.write("B.md", "# B\n");
        vault.write("Folder/C.md", "# C\n");
        vault
    }

    #[test]
    fn the_open_folders_survive_a_restart() {
        // The seam between the explorer and the file on disk: everything either
        // side of it is covered, but nothing proved the two ends met.
        let vault = sample();
        let state_file = vault.vault().path.join("state.json");

        let mut first = app(&vault);
        assert!(first.explorer.expanded(&first.index).is_empty());

        // Open a folder, then quit.
        let folder = first
            .explorer
            .rows()
            .iter()
            .position(|row| row.name().trim() == "Folder")
            .expect("Folder is listed");
        first.explorer.selected = folder;
        first.explorer.toggle(&first.index);
        assert_eq!(first.explorer.expanded(&first.index), vec!["Folder"]);
        first.save_ui_state_to(&state_file);

        // Start again: `new` collapses everything, `restore_ui_state` reopens
        // what was left open.
        let mut second = app(&vault);
        assert!(
            second.explorer.expanded(&second.index).is_empty(),
            "a fresh App is collapsed until the saved state is applied"
        );
        second.restore_ui_state_from(&state_file);
        assert_eq!(
            second.explorer.expanded(&second.index),
            vec!["Folder"],
            "the folder left open was not reopened"
        );
    }

    #[test]
    fn a_vault_opens_with_its_folders_closed() {
        // The explorer used to list every note in the vault at once, which
        // buries the top-level structure the folders exist to express.
        let vault = sample();
        let app = app(&vault);

        assert!(
            app.explorer.expanded(&app.index).is_empty(),
            "nothing is open on a first run"
        );
        let rows: Vec<String> = app
            .explorer
            .rows()
            .iter()
            .map(|row| row.name().trim().to_string())
            .collect();
        assert!(rows.iter().any(|n| n == "Folder"), "{rows:?}");
        assert!(
            !rows.iter().any(|n| n == "C"),
            "a note inside a folder stays hidden until it is opened: {rows:?}"
        );
    }

    #[test]
    fn the_local_graph_holds_only_the_neighborhood() {
        let vault = TempVault::new("local-graph");
        vault.write("Hub.md", "[[Near]]\n");
        vault.write("Near.md", "[[Far]]\n");
        vault.write("Far.md", "end\n");
        vault.write("Unrelated.md", "nothing\n");
        let mut app = app(&vault);

        let hub = app.index.id_of_rel("Hub.md").unwrap();
        app.open_graph(Some(hub));

        let graph = app.graph.as_ref().expect("graph");
        let labels: Vec<&str> = graph
            .simulation
            .graph
            .nodes
            .iter()
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec!["Hub", "Near"],
            "depth 1 from Hub, and nothing else in the vault"
        );
        assert_eq!(
            graph.selected,
            Some(0),
            "the note the graph was opened for is selected"
        );
    }

    #[test]
    fn the_local_graph_is_framed_on_its_own_extent() {
        // The bug this guards: fitting the view to the whole vault's bounds
        // while drawing only a neighbourhood, which leaves the local graph a
        // speck in the corner of an empty pane.
        let vault = TempVault::new("local-frame");
        let mut wide = String::new();
        for i in 0..40 {
            wide.push_str(&format!("[[N{i}]]\n"));
            vault.write(&format!("N{i}.md"), "spread out\n");
        }
        vault.write("Wide.md", &wide);
        vault.write("Pair.md", "[[Mate]]\n");
        vault.write("Mate.md", "back\n");
        let mut app = app(&vault);

        app.open_graph(None);
        let whole = app.graph.as_ref().expect("graph").span;

        let pair = app.index.id_of_rel("Pair.md").unwrap();
        app.open_graph(Some(pair));
        let local = app.graph.as_ref().expect("graph").span;

        assert!(
            local < whole,
            "two linked notes must frame tighter than a 40-spoke vault: {local} vs {whole}"
        );
    }

    #[test]
    fn framing_holds_still_while_the_layout_settles() {
        let vault = sample();
        let mut app = app(&vault);
        app.open_graph(None);

        let graph = app.graph.as_mut().expect("graph");
        let before = graph.span;
        graph.simulation.reheat();
        graph.simulation.step();
        assert_eq!(
            graph.span, before,
            "a step must not rescale the view, or the graph appears to breathe"
        );
    }

    #[test]
    fn arrows_walk_to_the_node_they_point_at() {
        let vault = sample();
        let mut app = app(&vault);
        app.open_graph(None);
        let graph = app.graph.as_mut().expect("graph");

        // A closer node off to the side must lose to an aligned one further
        // away, or arrow keys stop tracking the direction pressed.
        graph.simulation.drag(0, Vec2::new(0.0, 0.0));
        graph.simulation.drag(1, Vec2::new(40.0, 0.0));
        graph.simulation.drag(2, Vec2::new(4.0, 30.0));
        graph.selected = Some(0);

        graph.select_in_direction(1.0, 0.0);
        assert_eq!(
            graph.selected,
            Some(1),
            "right reaches the node to the right"
        );

        graph.selected = Some(0);
        graph.select_in_direction(0.0, 1.0);
        assert_eq!(graph.selected, Some(2), "up reaches the node above");

        // Nothing lies left of the origin node, so the selection stays put
        // rather than wrapping to the far side of the graph.
        graph.selected = Some(0);
        graph.select_in_direction(-1.0, 0.0);
        assert_eq!(graph.selected, Some(0));
    }

    #[test]
    fn an_arrow_with_nothing_selected_starts_at_a_hub() {
        let vault = sample();
        let mut app = app(&vault);
        app.open_graph(None);
        let graph = app.graph.as_mut().expect("graph");
        graph.selected = None;

        graph.select_in_direction(1.0, 0.0);
        assert!(
            graph.selected.is_some(),
            "an arrow key always lands somewhere"
        );
    }

    #[test]
    fn opening_a_note_creates_one_tab_and_reuses_it() {
        let vault = sample();
        let mut app = app(&vault);
        let a = app.index.id_of_rel("A.md").unwrap();

        app.open_note(a);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_note(), Some(a));
        assert_eq!(app.focus, Focus::Note);

        app.open_note(a);
        assert_eq!(app.tabs.len(), 1, "reopening focuses the existing tab");
    }

    #[test]
    fn following_a_link_to_a_missing_note_creates_it() {
        let vault = sample();
        let mut app = app(&vault);

        app.open_or_create(&LinkTarget::plain("Brand New"));
        assert!(vault.exists("Brand New.md"));
        assert_eq!(
            app.note_title(app.active_note().unwrap()),
            "Brand New",
            "the new note is opened, not just created"
        );
    }

    #[test]
    fn a_note_created_in_a_folder_gets_a_bare_heading() {
        let vault = sample();
        let mut app = app(&vault);

        app.open_or_create(&LinkTarget::plain("Daily/2026-07-27"));
        assert_eq!(
            vault.read("Daily/2026-07-27.md"),
            "# 2026-07-27\n\n",
            "the heading is the note name, not its path"
        );
    }

    #[test]
    fn following_a_link_to_an_existing_note_opens_it() {
        let vault = sample();
        let mut app = app(&vault);
        let b = app.index.id_of_rel("B.md").unwrap();

        app.open_or_create(&LinkTarget::plain("B"));
        assert_eq!(app.active_note(), Some(b));
        assert_eq!(app.tabs.len(), 1, "nothing was created");
    }

    #[test]
    fn follow_relative_md_link_resolves_from_source_folder() {
        let vault = sample();
        vault.write("sub/Gamma.md", "# Gamma\n");
        vault.write("sub/B.md", "# Sub B\n");
        let mut app = app(&vault);
        let source = vault.path().join("sub/Gamma.md");
        let sub_b = app.index.id_of_rel("sub/B.md").unwrap();
        let root_b = app.index.id_of_rel("B.md").unwrap();

        // `[x](B.md)` in sub/ means sub/B.md, not the B.md at the root...
        app.open_or_create(&LinkTarget {
            note: "B.md".to_string(),
            anchor: None,
            source: Some(source.clone()),
        });
        assert_eq!(app.active_note(), Some(sub_b));

        // ... and `[x](../B.md)` is the one at the root.
        app.open_or_create(&LinkTarget {
            note: "../B.md".to_string(),
            anchor: None,
            source: Some(source),
        });
        assert_eq!(app.active_note(), Some(root_b));
        assert_eq!(app.tabs.len(), 2, "nothing was created");
    }

    #[test]
    fn a_relative_md_link_to_a_missing_note_creates_it_next_to_the_source() {
        let vault = sample();
        vault.write("sub/Gamma.md", "# Gamma\n");
        let mut app = app(&vault);

        app.open_or_create(&LinkTarget {
            note: "Fresh.md".to_string(),
            anchor: None,
            source: Some(vault.path().join("sub/Gamma.md")),
        });

        assert_eq!(vault.read("sub/Fresh.md"), "# Fresh\n\n");
        assert!(!vault.exists("Fresh.md"));
    }

    #[test]
    fn a_relative_md_link_out_of_the_vault_falls_back_to_the_vault_lookup() {
        let vault = sample();
        let mut app = app(&vault);
        let b = app.index.id_of_rel("B.md").unwrap();

        app.open_or_create(&LinkTarget {
            note: "../../B.md".to_string(),
            anchor: None,
            source: Some(vault.path().join("A.md")),
        });

        assert_eq!(app.active_note(), Some(b));
    }

    #[test]
    fn editing_and_saving_writes_through_to_disk() {
        let vault = sample();
        let mut app = app(&vault);
        let b = app.index.id_of_rel("B.md").unwrap();
        app.open_note(b);

        let editor = app.editor_mut().expect("editor");
        editor.move_document_end(false);
        editor.insert_str("appended");
        assert!(app.active().unwrap().is_modified());

        app.save_tab(app.active_tab.unwrap());
        assert!(vault.read("B.md").contains("appended"));
        assert!(!app.active().unwrap().is_modified());
    }

    #[test]
    fn saving_reindexes_new_links() {
        let vault = sample();
        let mut app = app(&vault);
        let b = app.index.id_of_rel("B.md").unwrap();
        let a = app.index.id_of_rel("A.md").unwrap();
        app.open_note(b);

        app.editor_mut().unwrap().insert_str("[[A]]\n");
        app.save_tab(app.active_tab.unwrap());

        assert!(
            app.index.backlinks(a).iter().any(|bl| bl.source == b),
            "the new link must be in the index immediately"
        );
    }

    #[test]
    fn closing_a_tab_selects_a_neighbor() {
        let vault = sample();
        let mut app = app(&vault);
        let a = app.index.id_of_rel("A.md").unwrap();
        let b = app.index.id_of_rel("B.md").unwrap();

        app.open_note(a);
        app.open_note(b);
        app.close_tab();

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_note(), Some(a));
    }

    #[test]
    fn closing_the_last_tab_returns_focus_to_the_explorer() {
        let vault = sample();
        let mut app = app(&vault);
        let a = app.index.id_of_rel("A.md").unwrap();

        app.open_note(a);
        app.close_tab();

        assert!(app.tabs.is_empty());
        assert_eq!(app.active_tab, None);
        assert_eq!(app.focus, Focus::Explorer);
    }

    #[test]
    fn tab_cycling_wraps_in_both_directions() {
        let vault = sample();
        let mut app = app(&vault);
        for rel in ["A.md", "B.md", "Folder/C.md"] {
            let id = app.index.id_of_rel(rel).unwrap();
            app.open_note(id);
        }

        app.cycle_tab(1);
        assert_eq!(app.active_tab, Some(0), "wraps past the end");
        app.cycle_tab(-1);
        assert_eq!(app.active_tab, Some(2), "wraps past the start");
    }

    #[test]
    fn refresh_keeps_open_tabs_pointing_at_the_right_notes() {
        let vault = sample();
        let mut app = app(&vault);
        let c = app.index.id_of_rel("Folder/C.md").unwrap();
        app.open_note(c);

        // Adding a note earlier in sort order shifts every id.
        vault.write("AAA.md", "first\n");
        app.refresh();

        let title = app.note_title(app.active_note().expect("still open"));
        assert_eq!(title, "C", "the tab must follow the note, not the id");
    }

    #[test]
    fn refresh_drops_tabs_for_deleted_notes() {
        let vault = sample();
        let mut app = app(&vault);
        let b = app.index.id_of_rel("B.md").unwrap();
        app.open_note(b);

        std::fs::remove_file(vault.path().join("B.md")).expect("delete");
        app.refresh();

        assert!(app.tabs.is_empty(), "a tab for a deleted note is closed");
    }

    #[test]
    fn opening_the_graph_places_the_focused_note() {
        let vault = sample();
        let mut app = app(&vault);
        let a = app.index.id_of_rel("A.md").unwrap();

        app.open_graph(Some(a));
        let graph = app.graph.as_ref().expect("graph built");

        assert_eq!(app.view, View::Graph);
        assert!(
            graph.selected.is_some(),
            "the note is selected in the graph"
        );
        assert!(graph.simulation.is_settled(), "opens already laid out");
    }

    #[test]
    fn theme_switching_updates_the_palette_and_config() {
        let vault = sample();
        let mut app = app(&vault);

        app.set_theme("nord");
        assert_eq!(app.theme.name(), "nord");
        assert_eq!(app.config.theme, "nord");

        app.set_theme("not-a-theme");
        assert_eq!(
            app.theme.name(),
            "nord",
            "a bad name leaves the theme alone"
        );
        assert!(app.status.is_error);
    }

    #[test]
    fn history_records_where_you_came_from() {
        let vault = sample();
        let mut app = app(&vault);
        let a = app.index.id_of_rel("A.md").unwrap();
        let b = app.index.id_of_rel("B.md").unwrap();

        app.open_note(a);
        app.open_note(b);
        assert_eq!(app.history, vec![a]);

        app.open_note(b);
        assert_eq!(app.history, vec![a], "reopening the same note adds nothing");
    }

    #[test]
    fn daily_note_name_uses_the_configured_folder_and_format() {
        let vault = sample();
        let app = app(&vault);
        let name = app.daily_note_name();

        assert!(name.starts_with("Daily/"));
        // `Daily/YYYY-MM-DD`
        assert_eq!(name.len(), "Daily/".len() + 10, "got {name}");
    }

    #[test]
    fn civil_dates_match_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // A leap day, which is where naive date math usually breaks.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    // ---- ids are positions in the sorted list -----------------------------
    //
    // An operation that adds, renames or removes a note moves the ids of the
    // notes after it. Everything that holds one has to follow, or a tab shows
    // another note than it says.

    fn titled(app: &App) -> Vec<String> {
        app.tabs
            .iter()
            .map(|tab| app.note_title(tab.note))
            .collect()
    }

    fn reorderable() -> TempVault {
        let vault = TempVault::new("app-ids");
        vault.write("Alpha.md", "# Alpha\n");
        vault.write("Control.md", "# Control\n");
        vault.write("Omega.md", "# Omega\n");
        vault
    }

    #[test]
    fn creating_a_note_keeps_the_open_tabs_on_their_notes() {
        let vault = reorderable();
        let mut app = app(&vault);
        let omega = app.index.id_of_rel("Omega.md").unwrap();
        app.open_note(omega);

        // Sorts before Omega, so Omega's id moves up by one.
        app.open_or_create(&LinkTarget::plain("Beta"));

        assert_eq!(titled(&app), ["Omega", "Beta"]);
        assert_eq!(
            app.active_note().map(|id| app.note_title(id)).as_deref(),
            Some("Beta")
        );
    }

    #[test]
    fn creating_a_note_keeps_the_history() {
        let vault = reorderable();
        let mut app = app(&vault);
        let omega = app.index.id_of_rel("Omega.md").unwrap();
        app.open_note(omega);
        app.open_or_create(&LinkTarget::plain("Beta"));

        // Back goes to Omega, not to whatever now has Omega's old id.
        let back: Vec<String> = app.history.iter().map(|&id| app.note_title(id)).collect();
        assert_eq!(back, ["Omega"]);
    }

    #[test]
    fn renaming_a_note_to_a_later_name_keeps_the_tabs_on_their_notes() {
        let vault = reorderable();
        let mut app = app(&vault);
        for rel in ["Alpha.md", "Control.md", "Omega.md", "Control.md"] {
            let id = app.index.id_of_rel(rel).unwrap();
            app.open_note(id);
        }

        let control = app.index.id_of_rel("Control.md").unwrap();
        let renamed = app.rename_note(control, "Zulu").expect("rename");

        assert_eq!(titled(&app), ["Alpha", "Zulu", "Omega"]);
        assert_eq!(app.active_note(), Some(renamed.id));
        assert_eq!(app.note_title(renamed.id), "Zulu");
        let back: Vec<String> = app.history.iter().map(|&id| app.note_title(id)).collect();
        assert_eq!(
            back,
            ["Alpha", "Zulu", "Omega"],
            "history follows the rename"
        );
    }

    #[test]
    fn deleting_a_note_keeps_the_other_tabs_on_their_notes() {
        let vault = reorderable();
        let mut app = app(&vault);
        for rel in ["Alpha.md", "Control.md", "Omega.md"] {
            let id = app.index.id_of_rel(rel).unwrap();
            app.open_note(id);
        }
        let alpha = app.index.id_of_rel("Alpha.md").unwrap();

        app.delete_note(alpha).expect("delete");

        assert_eq!(titled(&app), ["Control", "Omega"]);
    }
}
