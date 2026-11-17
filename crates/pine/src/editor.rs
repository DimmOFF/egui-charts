//! The user's Pine scripts: the study factory they feed, the editor window and the store that
//! keeps them between runs.
//!
//! The editor follows TradingView's Pine Editor: a script menu with recently used scripts, an
//! "Open my script" dialog, a gutter with the current line marked, indent guides, a minimap, a
//! console and a status bar. Syntax colouring lives in `syntax.rs` (`editor/syntax.rs`).
//!
//! The scripts are the store's: the panel reads them from [`ScriptStore`] and calls it for every
//! save, rename and delete, so what the editor shows is what a restart finds.

mod syntax;

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use egui::{Align, Color32, Layout, Pos2, Rect, Sense, Stroke, TextStyle, UiBuilder, pos2, vec2};
use egui_charts::icons::icons;
use egui_charts::studies::{CustomIndicator, Indicator, IndicatorFactory};
use egui_charts::ui_kit::buttons::{Button, ButtonSize, IconButton};
use egui_charts::widget::{Chart, SeriesId};

use crate::PineStudy;
use crate::store::{
    FsScriptStore, MemoryScriptStore, ScriptId, ScriptKind, ScriptMeta, ScriptStore, StoreConfig,
    VersionId,
};
use syntax::{Palette, Token};

/// What a new script starts as.
const NEW_INDICATOR: &str = "//@version=6\nindicator(\"My script\")\nplot(close)\n";
const NEW_STRATEGY: &str = "//@version=6\nstrategy(\"My strategy\", overlay = true)\n\n";
const NEW_LIBRARY: &str = "//@version=6\n// @description My library\nlibrary(\"MyLibrary\")\n";

/// How many entries "Recently used" remembers.
const RECENT: usize = 8;
/// The height of the console and the status bar, in points.
const CONSOLE_HEIGHT: f32 = 112.0;
const STATUS_HEIGHT: f32 = 24.0;
/// The minimap column, and how many points a source character takes in it.
const MINIMAP_WIDTH: f32 = 76.0;
const MINIMAP_CHAR: f32 = 0.8;

/// One script of the store, with the source the editor keeps and the facts the dialogs show.
#[derive(Clone)]
struct SavedScript {
    id: ScriptId,
    /// The title the script and its `.pine` file carry.
    name: String,
    source: String,
    /// The newest version of the store's history, which moves only when the source did.
    version: VersionId,
    /// Unix seconds of the last save; 0 when unknown.
    updated: i64,
    /// Whether the "Open my script" dialog keeps it at the top.
    starred: bool,
}

impl SavedScript {
    /// A script the store already holds, `name` being the title its source declares.
    fn new(meta: &ScriptMeta, name: String, source: String, version: VersionId, starred: bool) -> Self {
        Self {
            id: meta.id,
            name,
            source,
            version: version.max(1),
            updated: chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
                .map_or(0, |at| at.timestamp()),
            starred,
        }
    }
}

pub struct PineScripts {
    /// Where the scripts live; [`MemoryScriptStore`] when no folder could be opened.
    store: Arc<dyn ScriptStore>,
    /// Where the editor's own state, the pin and the recent list, lives.
    state_path: PathBuf,
    scripts: Vec<SavedScript>,
    /// Whether the editor window is open.
    pub open: bool,
    /// Whether the editor is docked beside the terminal's right rail instead of floating.
    pinned: bool,
    draft: String,
    /// The index of the selected script, or `None` while a new one is edited.
    editing: Option<usize>,
    /// What the editor reports, oldest first.
    console: Vec<ConsoleLine>,
    console_open: bool,
    /// The names of the scripts opened last, newest first.
    recent: Vec<String>,
    /// The "Open my script" dialog, while it is shown.
    dialog: Option<OpenDialog>,
    /// Lines (1-based) the last compile reported a problem on; cleared by the next edit.
    marks: Vec<usize>,
    /// Where the minimap asked the code to scroll to.
    pending_scroll: Option<f32>,
    /// Whether the code field had the focus in the last frame.
    code_focused: bool,
    /// The caret, as a character index, in the last frame.
    cursor_char: usize,
    /// The compiled script the host should put on the chart.
    pending: Option<ChartPush>,
    /// Whether the script name is being edited in place.
    renaming: bool,
    rename_draft: String,
    /// Whether the rename field still has to take the focus.
    rename_focus: bool,
    /// The frame context of the terminal, so a script's finished background run asks for a frame.
    ctx: Option<egui::Context>,
}

impl Default for PineScripts {
    fn default() -> Self {
        Self {
            store: Arc::new(MemoryScriptStore::new()),
            state_path: PathBuf::new(),
            scripts: Vec::new(),
            open: false,
            pinned: false,
            draft: String::new(),
            editing: None,
            console: Vec::new(),
            console_open: true,
            recent: Vec::new(),
            dialog: None,
            marks: Vec::new(),
            pending_scroll: None,
            code_focused: false,
            cursor_char: 0,
            pending: None,
            renaming: false,
            rename_draft: String::new(),
            rename_focus: false,
            ctx: None,
        }
    }
}


/// A compiled script for the host to add to the chart.
pub struct ChartPush {
    /// The series this script replaces, for an update.
    pub replace: Option<SeriesId>,
    pub indicator: Box<dyn Indicator>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Info,
    Warn,
    Error,
}

struct ConsoleLine {
    time: String,
    text: String,
    level: Level,
}

/// The state of the "Open my script" dialog.
#[derive(Default)]
struct OpenDialog {
    query: String,
    /// The script whose trash button was clicked once and waits for the second click.
    confirm_delete: Option<usize>,
    /// Whether the search field has taken the focus yet.
    focused: bool,
}

/// What the top row offers besides the script menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChartAction {
    Add,
    Update,
}

/// What "Create new" starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewKind {
    Indicator,
    Strategy,
    Library,
}

impl NewKind {
    fn source(self) -> &'static str {
        match self {
            Self::Indicator => NEW_INDICATOR,
            Self::Strategy => NEW_STRATEGY,
            Self::Library => NEW_LIBRARY,
        }
    }
}

/// A script menu entry clicked in the current frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAction {
    New(NewKind),
    Rename,
    Save,
    Copy,
    Import,
    Export,
    Delete,
    Add,
    Update,
    Open(usize),
    OpenDialog,
}

/// The editor state kept beside the scripts.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct EditorState {
    pinned: bool,
    recent: Vec<String>,
    starred: Vec<ScriptId>,
}

/// The colours of the editor around the code. Measured from TradingView's dark theme where
/// noted; the rest are neighbours of it.
struct Chrome {
    /// Code, gutter, minimap, console: pure black in TradingView.
    code_bg: Color32,
    border: Color32,
    /// Line numbers (not sampled) and the number of the caret line (not sampled).
    gutter: Color32,
    gutter_active: Color32,
    /// The caret line's band, sampled: `#182041`.
    line_hl: Color32,
    error_hl: Color32,
    guide: Color32,
    muted: Color32,
    console_text: Color32,
}

impl Chrome {
    fn new(dark: bool) -> Self {
        if dark {
            Self {
                code_bg: Color32::from_rgb(0x00, 0x00, 0x00),
                border: Color32::from_rgb(0x22, 0x22, 0x22),
                gutter: Color32::from_rgb(0x7a, 0x7a, 0x7a),
                gutter_active: Color32::from_rgb(0xdb, 0xdb, 0xdb),
                line_hl: Color32::from_rgb(0x18, 0x20, 0x41),
                error_hl: Color32::from_rgba_unmultiplied(0xf2, 0x36, 0x45, 36),
                guide: Color32::from_white_alpha(22),
                muted: Color32::from_rgb(0x8f, 0x8f, 0x8f),
                console_text: Color32::from_rgb(0xb8, 0xb8, 0xb8),
            }
        } else {
            Self {
                code_bg: Color32::WHITE,
                border: Color32::from_rgb(0xe0, 0xe3, 0xeb),
                gutter: Color32::from_rgb(0x9c, 0x9f, 0xa8),
                gutter_active: Color32::from_rgb(0x13, 0x17, 0x22),
                line_hl: Color32::from_rgb(0xe8, 0xee, 0xff),
                error_hl: Color32::from_rgba_unmultiplied(0xf2, 0x36, 0x45, 30),
                guide: Color32::from_black_alpha(22),
                muted: Color32::from_rgb(0x78, 0x7b, 0x86),
                console_text: Color32::from_rgb(0x43, 0x46, 0x51),
            }
        }
    }
}

/// What the code field reported in a frame.
struct Shown {
    galley: Arc<egui::Galley>,
    pos: Pos2,
    cursor_char: Option<usize>,
    focused: bool,
    scroll_y: f32,
    view_height: f32,
}

impl PineScripts {
    /// Opens the script folder `app` resolves and reads its scripts.
    ///
    /// The folder is `config/pinescript/` beside a portable executable, otherwise the per-user
    /// config folder of `app`. When no folder can be written, the editor still works in memory
    /// and says so in its console.
    pub fn load(app: &str) -> Self {
        let config = match StoreConfig::resolve(app) {
            Ok(config) => config,
            Err(error) => {
                log::error!("Pine scripts: {error}; scripts are kept for this run only");
                return Self::unpersisted(error);
            }
        };
        let state_path = config.dir.with_file_name("pine_editor.json");
        match FsScriptStore::open(config) {
            Ok(store) => Self::with_store(Arc::new(store), state_path),
            Err(error) => {
                log::error!("Pine scripts: {error}; scripts are kept for this run only");
                Self::unpersisted(error)
            }
        }
    }

    /// An editor that keeps its scripts for one run, with the reason in its console.
    fn unpersisted(error: crate::store::StoreError) -> Self {
        let mut pine = Self::default();
        pine.log(Level::Error, format!("Scripts are not saved: {error}"));
        pine
    }

    /// The editor over a store the host owns; `state_path` holds the pin, the recent list and the
    /// stars.
    pub fn with_store(store: Arc<dyn ScriptStore>, state_path: PathBuf) -> Self {
        let state: EditorState = std::fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let scripts = match store.list(None) {
            Ok(metas) => metas
                .into_iter()
                .filter_map(|meta| {
                    let source = match store.load(meta.id) {
                        Ok(source) => source.source,
                        Err(error) => {
                            log::warn!("Pine scripts: {error}");
                            return None;
                        }
                    };
                    // The title in the source is what the chart lists and `on_chart` looks for;
                    // the stored title follows on the next save.
                    let name = PineStudy::new(&source)
                        .map_or_else(|_| meta.title.clone(), |study| study.name().to_owned());
                    let version = store
                        .history(meta.id)
                        .ok()
                        .and_then(|versions| versions.last().map(|version| version.n))
                        .unwrap_or(1);
                    let starred = state.starred.contains(&meta.id);
                    Some(SavedScript::new(&meta, name, source, version, starred))
                })
                .collect(),
            Err(error) => {
                log::error!("Pine scripts: {error}");
                Vec::new()
            }
        };
        let draft = scripts
            .first()
            .map(|script| script.source.clone())
            .unwrap_or_default();
        let editing = (!scripts.is_empty()).then_some(0);
        Self {
            store,
            state_path,
            scripts,
            draft,
            editing,
            pinned: state.pinned,
            recent: state.recent,
            ..Self::default()
        }
    }

    /// The built-in studies plus one entry per saved script, under My scripts.
    pub fn factory(&self) -> IndicatorFactory {
        let mut factory = IndicatorFactory::new();
        for script in &self.scripts {
            let source = script.source.clone();
            let fallback = script.name.clone();
            let ctx = self.ctx.clone();
            factory.register_in(IndicatorFactory::PERSONAL, script.name.clone(), move || {
                match PineStudy::new(&source) {
                    Ok(mut study) => {
                        study.set_context(ctx.clone());
                        Box::new(study)
                    }
                    // A saved script that no longer parses stays listed instead of taking the
                    // terminal down; the editor reports the syntax error when it is opened.
                    Err(_) => Box::new(CustomIndicator::new(fallback.clone(), |_| Vec::new())),
                }
            });
        }
        factory
    }

    /// Keeps the terminal's frame context before the terminal draws, so a script added from the
    /// study list runs on a thread of its own: the factory is built before the first frame.
    pub fn attach(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
    }

    /// Draws the editor. Returns whether the scripts changed, so the caller rebuilds its factory.
    /// A pinned editor claims its width from `ui` before the terminal draws its frame, which
    /// docks it to the right of the terminal's right rail; otherwise it floats in a window.
    /// `on_chart` is the series of the selected script the chart already carries.
    pub fn show(&mut self, ui: &mut egui::Ui, on_chart: Option<SeriesId>) -> bool {
        if !self.open {
            return false;
        }
        self.ctx = Some(ui.ctx().clone());
        if self.pinned {
            self.show_docked(ui, on_chart)
        } else {
            self.show_window(ui.ctx(), on_chart)
        }
    }

    /// The series of the selected script on `chart`, if the chart carries it.
    pub fn on_chart(&self, chart: &Chart) -> Option<SeriesId> {
        let name = self.current()?.name.as_str();
        chart
            .indicators()
            .find(|(_, indicator)| indicator.name() == name)
            .map(|(id, _)| id)
    }

    /// Takes the script the editor compiled for the chart.
    pub fn take_chart_push(&mut self) -> Option<ChartPush> {
        self.pending.take()
    }

    fn current(&self) -> Option<&SavedScript> {
        self.editing.and_then(|at| self.scripts.get(at))
    }

    fn show_window(&mut self, ctx: &egui::Context, on_chart: Option<SeriesId>) -> bool {
        let mut changed = false;
        let mut close = false;
        let chrome = Chrome::new(ctx.theme() == egui::Theme::Dark);
        // The window's own title bar has no room for the pin, so the editor draws the row.
        egui::Window::new("Pine Editor")
            .title_bar(false)
            .default_size([860.0, 640.0])
            .min_size([420.0, 320.0])
            .max_size([1100.0, 900.0])
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(chrome.code_bg)
                    .stroke(Stroke::new(1.0, chrome.border))
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ctx, |ui| {
                self.title_row(ui, &mut close);
                changed = self.body(ui, on_chart);
            });
        if close {
            self.open = false;
        }
        changed
    }

    fn show_docked(&mut self, ui: &mut egui::Ui, on_chart: Option<SeriesId>) -> bool {
        let mut changed = false;
        let mut close = false;
        let max_width = (ui.available_width() * 0.6).max(320.0);
        let chrome = Chrome::new(ui.visuals().dark_mode);
        egui::Panel::right("egui_charts_pine_editor")
            .resizable(true)
            .default_size(480.0)
            .min_size(320.0)
            .max_size(max_width)
            .frame(
                egui::Frame::new()
                    .fill(chrome.code_bg)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ui, |ui| {
                self.title_row(ui, &mut close);
                changed = self.body(ui, on_chart);
            });
        if close {
            self.open = false;
        }
        changed
    }

    /// The title row: the title, the pin that docks the editor and the close button.
    fn title_row(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        // The buttons first, so a long title is elided in the width left rather than pushing
        // them out of a narrow panel.
        let row = egui::Vec2::new(ui.available_width(), ButtonSize::MD.pixels());
        ui.allocate_ui_with_layout(
            row,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                *close |= IconButton::new(&icons::CLOSE)
                    .size(ButtonSize::SM)
                    .tooltip("Close")
                    .show(ui)
                    .clicked();
                if IconButton::new(&icons::PIN)
                    .size(ButtonSize::SM)
                    .selected(self.pinned)
                    .tooltip(if self.pinned { "Undock" } else { "Dock" })
                    .show(ui)
                    .clicked()
                {
                    self.pinned = !self.pinned;
                    self.save_state();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new("Pine Editor").strong()).truncate(),
                    );
                });
            },
        );
        ui.add_space(2.0);
    }

    /// The script row, the code, the console and the status bar.
    fn body(&mut self, ui: &mut egui::Ui, on_chart: Option<SeriesId>) -> bool {
        let chrome = Chrome::new(ui.visuals().dark_mode);
        let mut changed = false;
        let mut action = None;
        self.script_row(ui, on_chart, &mut action, &mut changed);
        if self.code_focused {
            let (save, open) = ui.input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::COMMAND, egui::Key::S),
                    input.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
                )
            });
            if save {
                action = Some(MenuAction::Save);
            }
            if open {
                action = Some(MenuAction::OpenDialog);
            }
        }
        changed |= self.run_action(action, on_chart);
        ui.add_space(4.0);
        // Without a bounded height the editor asks for every row it holds, the window grows
        // past the screen and its close button goes with it.
        let console_height = if self.console_open { CONSOLE_HEIGHT } else { 0.0 };
        let code_height = (ui.available_height() - STATUS_HEIGHT - console_height - 6.0).max(120.0);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            self.code_area(ui, &chrome, code_height);
            self.console_panel(ui, &chrome, console_height);
            self.status_bar(ui, &chrome);
        });
        changed |= self.open_dialog(ui.ctx());
        changed
    }

    /// The top row: the script menu, the chart button and the more-menu at the right.
    fn script_row(
        &mut self,
        ui: &mut egui::Ui,
        on_chart: Option<SeriesId>,
        action: &mut Option<MenuAction>,
        changed: &mut bool,
    ) {
        ui.horizontal(|ui| {
            if self.renaming {
                let response =
                    ui.add(egui::TextEdit::singleline(&mut self.rename_draft).desired_width(180.0));
                if self.rename_focus {
                    response.request_focus();
                    self.rename_focus = false;
                }
                let escaped = ui.input(|input| input.key_pressed(egui::Key::Escape));
                let entered = ui.input(|input| input.key_pressed(egui::Key::Enter));
                if response.lost_focus() || (response.has_focus() && entered) {
                    if escaped {
                        self.renaming = false;
                    } else {
                        *changed |= self.commit_rename();
                    }
                }
            } else {
                self.name_menu(ui, action);
            }
            match self.chart_action(on_chart) {
                Some(ChartAction::Add) => {
                    if Button::new("\u{25B6} Add to chart")
                        .primary()
                        .show(ui)
                        .clicked()
                    {
                        *action = Some(MenuAction::Add);
                    }
                }
                Some(ChartAction::Update) => {
                    if Button::new("Update on chart").primary().show(ui).clicked() {
                        *action = Some(MenuAction::Update);
                    }
                }
                None => {}
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.more_menu(ui, action);
            });
        });
    }

    /// The selected script's name with the script menu under it, as in TradingView.
    fn name_menu(&self, ui: &mut egui::Ui, action: &mut Option<MenuAction>) {
        let name = self
            .current()
            .map_or("Untitled script", |script| script.name.as_str());
        // The trailing em spaces leave room for the chevron painted over the button.
        let label = format!("~  {name}\u{2003}\u{2003}");
        let button = egui::Button::new(egui::RichText::new(label).strong())
            .frame_when_inactive(false)
            .min_size(vec2(0.0, ButtonSize::MD.pixels()));
        let has_script = self.current().is_some();
        let recent: Vec<usize> = self
            .recent
            .iter()
            .filter_map(|name| self.scripts.iter().position(|script| &script.name == name))
            .take(5)
            .collect();
        let (response, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
            ui.set_min_width(250.0);
            if menu_item(ui, "Save script", "Ctrl+S", true) {
                *action = Some(MenuAction::Save);
            }
            if menu_item(ui, "Make a copy\u{2026}", "", has_script) {
                *action = Some(MenuAction::Copy);
            }
            if menu_item(ui, "Rename\u{2026}", "", has_script) {
                *action = Some(MenuAction::Rename);
            }
            ui.add_enabled(false, egui::Button::new("Version history\u{2026}"))
                .on_disabled_hover_text("Not implemented yet");
            ui.separator();
            ui.menu_button("Create new", |ui| {
                for (label, kind) in [
                    ("Indicator", NewKind::Indicator),
                    ("Strategy", NewKind::Strategy),
                    ("Library", NewKind::Library),
                ] {
                    if menu_item(ui, label, "", true) {
                        *action = Some(MenuAction::New(kind));
                    }
                }
            });
            if !recent.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("RECENTLY USED").small().weak());
                for at in &recent {
                    let name = &self.scripts[*at].name;
                    if ui.selectable_label(self.editing == Some(*at), name).clicked() {
                        *action = Some(MenuAction::Open(*at));
                        ui.close();
                    }
                }
            }
            ui.separator();
            if menu_item(ui, "Open script\u{2026}", "Ctrl+O", true) {
                *action = Some(MenuAction::OpenDialog);
            }
        });
        paint_chevron(ui, response.rect);
    }

    /// The menu at the right of the row: what does not fit the script menu.
    fn more_menu(&self, ui: &mut egui::Ui, action: &mut Option<MenuAction>) {
        let has_script = self.current().is_some();
        let (menu, _) = egui::containers::menu::MenuButton::from_button(
            icons::MENU
                .as_button_square(20.0)
                .frame_when_inactive(false)
                .min_size(egui::Vec2::splat(ButtonSize::MD.pixels())),
        )
        .ui(ui, |ui| {
            if menu_item(ui, "Import\u{2026}", "", true) {
                *action = Some(MenuAction::Import);
            }
            if menu_item(ui, "Export\u{2026}", "", true) {
                *action = Some(MenuAction::Export);
            }
            ui.separator();
            if menu_item(ui, "Delete script", "", has_script) {
                *action = Some(MenuAction::Delete);
            }
        });
        menu.on_hover_text("More");
    }

    /// Carries out the menu entry clicked in this frame.
    fn run_action(&mut self, action: Option<MenuAction>, on_chart: Option<SeriesId>) -> bool {
        let mut changed = false;
        match action {
            Some(MenuAction::New(kind)) => {
                self.editing = None;
                self.draft = kind.source().to_owned();
                self.marks.clear();
            }
            Some(MenuAction::Rename) => {
                self.renaming = true;
                self.rename_draft = self.current().map_or_else(String::new, |s| s.name.clone());
                self.rename_focus = true;
            }
            Some(MenuAction::Save) => changed |= self.apply(),
            Some(MenuAction::Copy) => changed |= self.make_copy(),
            Some(MenuAction::Import) => changed |= self.import(),
            Some(MenuAction::Export) => self.export(),
            Some(MenuAction::Delete) => {
                if let Some(at) = self.editing {
                    changed |= self.delete_script(at);
                }
            }
            Some(MenuAction::Add) => changed |= self.add_to_chart(None),
            Some(MenuAction::Update) => changed |= self.add_to_chart(on_chart),
            Some(MenuAction::Open(at)) => self.select(at),
            Some(MenuAction::OpenDialog) => self.dialog = Some(OpenDialog::default()),
            None => {}
        }
        changed
    }

    /// The code: gutter, current-line band, indent guides, highlighted text and the minimap.
    fn code_area(&mut self, ui: &mut egui::Ui, chrome: &Chrome, height: f32) {
        let full = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(full.min, vec2(full.width(), height));
        ui.allocate_rect(rect, Sense::hover());
        ui.painter().rect_filled(rect, 0.0, chrome.code_bg);

        let font = TextStyle::Monospace.resolve(ui.style());
        let digit = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0'));
        let row_height = ui.text_style_height(&TextStyle::Monospace);
        let total_lines = self.draft.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let gutter_width = digit * total_lines.to_string().len().max(2) as f32 + 30.0;
        let minimap_width = if rect.width() > 460.0 {
            MINIMAP_WIDTH
        } else {
            0.0
        };
        let text_rect = Rect::from_min_max(
            pos2(rect.left() + gutter_width, rect.top()),
            pos2(rect.right() - minimap_width, rect.bottom()),
        );
        // No wrapping, as in TradingView: the field is as wide as its longest line.
        let longest = self
            .draft
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let content_width = (longest as f32 + 8.0) * digit;
        let rows = (height / row_height).ceil() as usize + 1;
        let id = egui::Id::unique("pine-code-edit");
        if self.code_focused {
            self.edit_keys(ui.ctx(), id);
        }
        let palette = if ui.visuals().dark_mode {
            &syntax::DARK
        } else {
            &syntax::LIGHT
        };
        let marks = self.marks.clone();
        let leading: Vec<usize> = self
            .draft
            .lines()
            .map(|line| line.chars().take_while(|c| *c == ' ').count())
            .collect();
        let pending = self.pending_scroll.take();

        let mut shown: Option<Shown> = None;
        ui.scope_builder(UiBuilder::new().max_rect(text_rect), |ui| {
            let mut area = egui::ScrollArea::both()
                .id_salt("pine-code")
                .auto_shrink([false, false]);
            if let Some(y) = pending {
                area = area.vertical_scroll_offset(y);
            }
            let out = area.show(ui, |ui| {
                // Painted behind the text: the shapes are filled in once the rows are known.
                let background = ui.painter().add(egui::Shape::Noop);
                let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, _wrap: f32| {
                    let spans = cached_spans(ui.ctx(), buffer.as_str());
                    let job = pine_layout_job(ui, buffer.as_str(), &spans, palette);
                    ui.fonts_mut(|fonts| fonts.layout_job(job))
                };
                let output = egui::TextEdit::multiline(&mut self.draft)
                    .id(id)
                    .code_editor()
                    .lock_focus(true)
                    .desired_rows(rows)
                    .desired_width(content_width.max(text_rect.width() - 8.0))
                    .layouter(&mut layouter)
                    .show(ui);
                if output.response.changed() {
                    self.marks.clear();
                }
                let galley = output.galley.clone();
                let pos = output.galley_pos;
                let cursor_char = output.cursor_range.map(|range| range.primary.index.into());
                let cursor_line = cursor_char.map(|at| line_col(&self.draft, at).0 - 1);

                let clip = ui.clip_rect();
                let mut shapes = Vec::new();
                let mut line = 0usize;
                let mut first_row = true;
                for row in &galley.rows {
                    let y = pos.y + row.pos.y;
                    if y + row_height >= clip.top() && y <= clip.bottom() {
                        let band = Rect::from_min_max(
                            pos2(clip.left(), y),
                            pos2(clip.right(), y + row_height),
                        );
                        if marks.contains(&(line + 1)) {
                            shapes.push(egui::Shape::rect_filled(band, 0.0, chrome.error_hl));
                        } else if Some(line) == cursor_line {
                            shapes.push(egui::Shape::rect_filled(band, 0.0, chrome.line_hl));
                        }
                        if first_row {
                            let levels = leading.get(line).copied().unwrap_or(0) / 4;
                            for level in 0..levels {
                                let x = pos.x + (level * 4) as f32 * digit + 0.5;
                                shapes.push(egui::Shape::line_segment(
                                    [pos2(x, y), pos2(x, y + row_height)],
                                    Stroke::new(1.0, chrome.guide),
                                ));
                            }
                        }
                    }
                    if row.ends_with_newline {
                        line += 1;
                        first_row = true;
                    } else {
                        first_row = false;
                    }
                }
                ui.painter().set(background, egui::Shape::Vec(shapes));
                (galley, pos, cursor_char, output.response.has_focus())
            });
            let (galley, pos, cursor_char, focused) = out.inner;
            shown = Some(Shown {
                galley,
                pos,
                cursor_char,
                focused,
                scroll_y: out.state.clamped_offset().y,
                view_height: out.inner_rect.height(),
            });
        });

        let Some(shown) = shown else {
            return;
        };
        self.code_focused = shown.focused;
        if let Some(at) = shown.cursor_char {
            self.cursor_char = at;
        }

        // The gutter stays put while the code scrolls sideways, so it is drawn on top.
        let gutter = Rect::from_min_max(rect.min, pos2(rect.left() + gutter_width, rect.bottom()));
        let painter = ui.painter_at(gutter);
        painter.rect_filled(gutter, 0.0, chrome.code_bg);
        let caret_line = line_col(&self.draft, self.cursor_char).0 - 1;
        let mut line = 0usize;
        let mut first_row = true;
        for row in &shown.galley.rows {
            let y = shown.pos.y + row.pos.y;
            if first_row && y + row_height >= gutter.top() && y <= gutter.bottom() {
                let color = if line == caret_line {
                    chrome.gutter_active
                } else {
                    chrome.gutter
                };
                painter.text(
                    pos2(gutter.right() - 10.0, y),
                    egui::Align2::RIGHT_TOP,
                    (line + 1).to_string(),
                    font.clone(),
                    color,
                );
                if self.marks.contains(&(line + 1)) {
                    painter.circle_filled(
                        pos2(gutter.left() + 9.0, y + row_height / 2.0),
                        3.0,
                        ui.visuals().error_fg_color,
                    );
                }
            }
            if row.ends_with_newline {
                line += 1;
                first_row = true;
            } else {
                first_row = false;
            }
        }

        if minimap_width > 0.0 {
            let map = Rect::from_min_max(pos2(rect.right() - minimap_width, rect.top()), rect.max);
            self.minimap(
                ui,
                map,
                chrome,
                palette,
                shown.scroll_y,
                shown.view_height,
                row_height,
            );
        }
    }

    /// A scaled-down picture of the whole script; a click or a drag scrolls the code.
    #[allow(clippy::too_many_arguments)]
    fn minimap(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        chrome: &Chrome,
        palette: &Palette,
        scroll_y: f32,
        view_height: f32,
        row_height: f32,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, chrome.code_bg);
        painter.line_segment(
            [rect.left_top(), rect.left_bottom()],
            Stroke::new(1.0, chrome.border),
        );
        let text = self.draft.as_str();
        let mut starts = vec![0usize];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        let top = rect.top() + 4.0;
        let left = rect.left() + 6.0;
        // Two points a line like TradingView, squeezed when the script is longer than the column.
        let line_px = ((rect.height() - 8.0) / starts.len() as f32).clamp(0.2, 2.0);
        let step = ((0.9 / line_px).ceil() as usize).max(1);
        let right = rect.right() - 2.0;
        let plain = rgba(palette.plain, 70);
        for (line, start) in starts.iter().enumerate().step_by(step) {
            let end = starts.get(line + 1).map_or(text.len(), |next| next - 1);
            let content = &text[*start..end.max(*start)];
            let trimmed = content.trim();
            if trimmed.is_empty() {
                continue;
            }
            let indent = content.len() - content.trim_start().len();
            let x = left + indent as f32 * MINIMAP_CHAR;
            let width = (trimmed.chars().count() as f32 * MINIMAP_CHAR).min(right - x);
            if width > 0.0 {
                let y = top + line as f32 * line_px;
                painter.rect_filled(
                    Rect::from_min_size(pos2(x, y), vec2(width, (line_px * 0.7).max(0.8))),
                    0.0,
                    plain,
                );
            }
        }
        let spans = cached_spans(ui.ctx(), text);
        for (range, token) in spans.iter() {
            let line = starts.partition_point(|start| *start <= range.start) - 1;
            if line % step != 0 {
                continue;
            }
            let x = left + (range.start - starts[line]) as f32 * MINIMAP_CHAR;
            let width = (range.len() as f32 * MINIMAP_CHAR).min(right - x);
            if width > 0.0 {
                let y = top + line as f32 * line_px;
                painter.rect_filled(
                    Rect::from_min_size(pos2(x, y), vec2(width, (line_px * 0.7).max(0.8))),
                    0.0,
                    rgba(palette.of(*token), 210),
                );
            }
        }
        // The part of the script the code field shows.
        let view_top = top + scroll_y / row_height * line_px;
        let view = Rect::from_min_size(
            pos2(rect.left() + 1.0, view_top),
            vec2(rect.width() - 1.0, view_height / row_height * line_px),
        )
        .intersect(rect);
        painter.rect_filled(view, 0.0, Color32::from_white_alpha(14));

        let response = ui.interact(rect, egui::Id::unique("pine-minimap"), Sense::click_and_drag());
        if response.clicked() || response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let line = (pointer.y - top) / line_px;
                self.pending_scroll = Some((line * row_height - view_height / 2.0).max(0.0));
                ui.ctx().request_repaint();
            }
        }
    }

    /// Tab indents by spaces and Enter keeps the indent, which the field would not do itself.
    fn edit_keys(&mut self, ctx: &egui::Context, id: egui::Id) {
        let (tab, enter) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            )
        });
        if !tab && !enter {
            return;
        }
        let Some(range) = egui::TextEdit::load_state(ctx, id).and_then(|state| state.cursor.char_range())
        else {
            return;
        };
        let [start, _] = range.sorted_cursors();
        if tab {
            // A selection is left alone: replacing it with spaces would delete it.
            if range.is_empty() {
                let column = line_col(&self.draft, start.index.into()).1 - 1;
                let spaces = 4 - column % 4;
                insert_text(ctx, id, &mut self.draft, &" ".repeat(spaces));
            }
        } else {
            let indent = auto_indent(&self.draft, start.index.into());
            insert_text(ctx, id, &mut self.draft, &format!("\n{indent}"));
        }
    }

    /// The console of what the editor did, newest at the bottom.
    fn console_panel(&mut self, ui: &mut egui::Ui, chrome: &Chrome, height: f32) {
        if height <= 0.0 {
            return;
        }
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, chrome.code_bg);
        ui.painter().line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, chrome.border),
        );
        ui.scope_builder(UiBuilder::new().max_rect(rect.shrink2(vec2(12.0, 6.0))), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("pine-console")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for line in &self.console {
                        let color = match line.level {
                            Level::Info => chrome.console_text,
                            Level::Warn => ui.visuals().warn_fg_color,
                            Level::Error => ui.visuals().error_fg_color,
                        };
                        ui.label(
                            egui::RichText::new(format!("{}  {}", line.time, line.text))
                                .color(color),
                        );
                    }
                });
        });
    }

    /// The bottom bar: the console toggle, the saved version, the caret and the Pine version.
    fn status_bar(&mut self, ui: &mut egui::Ui, chrome: &Chrome) {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), STATUS_HEIGHT), Sense::hover());
        ui.painter().line_segment(
            [rect.left_top(), rect.right_top()],
            Stroke::new(1.0, chrome.border),
        );
        let (line, column) = line_col(&self.draft, self.cursor_char);
        let saved = match self.current() {
            Some(script) => format!(
                "{} \u{b7} {}",
                script.version.max(1),
                local_time(script.updated, "%b %-d, %H:%M")
            ),
            None => "Unsaved".to_owned(),
        };
        let pine = format!("Pine Script\u{ae} v{}", pine_version(&self.draft));
        let text = |value: String| egui::RichText::new(value).size(12.0).color(chrome.muted);
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect.shrink2(vec2(8.0, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                let toggle = ui
                    .add(
                        egui::Button::new(egui::RichText::new(">_").monospace())
                            .frame(false)
                            .selected(self.console_open),
                    )
                    .on_hover_text("Pine console");
                if toggle.clicked() {
                    self.console_open = !self.console_open;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(text(pine));
                    ui.add_space(8.0);
                    ui.label(text(format!("Line {line}, Col {column}")));
                    ui.add_space(8.0);
                    ui.label(text(saved));
                });
            },
        );
    }

    /// The "Open my script" dialog. Returns whether a script was deleted.
    fn open_dialog(&mut self, ctx: &egui::Context) -> bool {
        let Some(mut dialog) = self.dialog.take() else {
            return false;
        };
        let mut keep = !ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let mut pick = None;
        let mut delete = None;
        let mut star = None;
        let query = dialog.query.to_lowercase();
        let mut order: Vec<usize> = (0..self.scripts.len())
            .filter(|at| self.scripts[*at].name.to_lowercase().contains(&query))
            .collect();
        order.sort_by(|a, b| {
            let (a, b) = (&self.scripts[*a], &self.scripts[*b]);
            b.starred
                .cmp(&a.starred)
                .then(b.updated.cmp(&a.updated))
                .then(a.name.cmp(&b.name))
        });
        egui::Window::new("Open my script")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([460.0, 520.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Open my script");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if IconButton::new(&icons::CLOSE)
                            .size(ButtonSize::SM)
                            .tooltip("Close")
                            .show(ui)
                            .clicked()
                        {
                            keep = false;
                        }
                    });
                });
                let search = ui.add(
                    egui::TextEdit::singleline(&mut dialog.query)
                        .hint_text("Search")
                        .desired_width(f32::INFINITY),
                );
                if !dialog.focused {
                    search.request_focus();
                    dialog.focused = true;
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("SCRIPT NAME").small().weak());
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for at in &order {
                            let script = &self.scripts[*at];
                            ui.horizontal(|ui| {
                                let star_color = if script.starred {
                                    Color32::from_rgb(0xff, 0xb7, 0x4d)
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("\u{2605}").color(star_color),
                                        )
                                        .frame(false),
                                    )
                                    .on_hover_text("Favourite")
                                    .clicked()
                                {
                                    star = Some(*at);
                                }
                                ui.vertical(|ui| {
                                    let name = ui.add(
                                        egui::Label::new(egui::RichText::new(&script.name).strong())
                                            .sense(Sense::click())
                                            .truncate(),
                                    );
                                    if name.clicked() {
                                        pick = Some(*at);
                                    }
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Version: {}.0 ({})",
                                            script.version.max(1),
                                            local_time(script.updated, "%d.%m.%Y %H:%M")
                                        ))
                                        .small()
                                        .weak(),
                                    );
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if dialog.confirm_delete == Some(*at) {
                                        let sure = egui::Button::new(
                                            egui::RichText::new("Delete?")
                                                .color(ui.visuals().error_fg_color),
                                        );
                                        if ui.add(sure).clicked() {
                                            delete = Some(*at);
                                        }
                                    } else if ui
                                        .add(egui::Button::new("\u{1F5D1}").frame(false))
                                        .on_hover_text("Delete script")
                                        .clicked()
                                    {
                                        dialog.confirm_delete = Some(*at);
                                    }
                                });
                            });
                            ui.add_space(2.0);
                        }
                        if order.is_empty() {
                            ui.label(egui::RichText::new("No scripts found").weak());
                        }
                    });
            });
        let mut changed = false;
        if let Some(at) = star {
            self.scripts[at].starred = !self.scripts[at].starred;
            self.save_state();
        }
        if let Some(at) = delete {
            changed |= self.delete_script(at);
            dialog.confirm_delete = None;
        }
        if let Some(at) = pick {
            self.select(at);
            keep = false;
        }
        if keep {
            self.dialog = Some(dialog);
        }
        changed
    }

    /// What the chart button offers: Add while the script is elsewhere, Update while it differs.
    fn chart_action(&self, on_chart: Option<SeriesId>) -> Option<ChartAction> {
        match on_chart {
            None => Some(ChartAction::Add),
            Some(_) => self
                .current()
                .filter(|script| script.source != self.draft)
                .map(|_| ChartAction::Update),
        }
    }

    /// Opens the stored script `at` in the editor.
    fn select(&mut self, at: usize) {
        let Some(script) = self.scripts.get(at) else {
            return;
        };
        let name = script.name.clone();
        self.draft = script.source.clone();
        self.editing = Some(at);
        self.marks.clear();
        self.log(Level::Info, format!("\"{name}\" opened"));
        self.recent.retain(|recent| recent != &name);
        self.recent.insert(0, name);
        self.recent.truncate(RECENT);
        self.save_state();
    }

    /// Moves the stored script `at` to the trash; returns whether there was one.
    fn delete_script(&mut self, at: usize) -> bool {
        let Some(script) = self.scripts.get(at).cloned() else {
            return false;
        };
        if let Err(error) = self.store.delete(script.id) {
            self.log(
                Level::Error,
                format!("Could not delete `{}`: {error}", script.name),
            );
            return false;
        }
        self.scripts.remove(at);
        match self.editing {
            Some(editing) if editing == at => {
                self.editing = None;
                self.draft.clear();
            }
            Some(editing) if editing > at => self.editing = Some(editing - 1),
            _ => {}
        }
        self.recent.retain(|name| name != &script.name);
        self.marks.clear();
        self.log(Level::Info, format!("\"{}\" deleted", script.name));
        self.save_state();
        true
    }

    /// Stores the draft as a second script named after the first.
    fn make_copy(&mut self) -> bool {
        let Some(script) = self.current() else {
            return false;
        };
        let name = self.unique_name(&format!("{} (copy)", script.name), None);
        let source = script_with_title(&self.draft, &name);
        let id = match self
            .store
            .create(ScriptKind::from_source(&source), &name, &source)
        {
            Ok(id) => id,
            Err(error) => {
                self.log(Level::Error, format!("Could not store `{name}`: {error}"));
                return false;
            }
        };
        self.scripts.push(SavedScript {
            id,
            name: name.clone(),
            source: source.clone(),
            version: 1,
            updated: unix_now(),
            starred: false,
        });
        self.editing = Some(self.scripts.len() - 1);
        self.draft = source;
        self.marks.clear();
        self.log(Level::Info, format!("\"{name}\" created"));
        true
    }

    /// Renames the draft's script and stores the result when it is a saved one.
    fn commit_rename(&mut self) -> bool {
        self.renaming = false;
        let name = self.rename_draft.trim().to_owned();
        if name.is_empty() {
            return false;
        }
        self.draft = script_with_title(&self.draft, &name);
        self.current().is_some() && self.apply()
    }

    /// Compiles the draft, stores it and hands the result to the chart.
    fn add_to_chart(&mut self, replace: Option<SeriesId>) -> bool {
        if !self.apply() {
            return false;
        }
        let Some(script) = self.current() else {
            return false;
        };
        let Ok(study) = PineStudy::new(&script.source) else {
            return false;
        };
        let mut study = study;
        study.set_context(self.ctx.clone());
        self.pending = Some(ChartPush {
            replace,
            indicator: Box::new(study),
        });
        true
    }

    /// Compiles the draft, stores it and reports what it could not run.
    fn apply(&mut self) -> bool {
        self.marks.clear();
        let study = match PineStudy::new(&self.draft) {
            Ok(study) => study,
            Err(error) => {
                self.log(Level::Error, error);
                return false;
            }
        };
        let name = self.unique_name(study.name(), self.editing);
        let editing = self
            .editing
            .and_then(|at| self.scripts.get(at).map(|script| (at, script.id, script.name.clone())));
        match editing {
            Some((at, id, old_name)) => {
                // The store snapshots only a changed source, and reports the new version.
                let version = match self.store.save(id, &self.draft, None) {
                    Ok(Some(version)) => version,
                    Ok(None) => self.scripts[at].version,
                    Err(error) => {
                        self.log(Level::Error, format!("Could not save `{name}`: {error}"));
                        return false;
                    }
                };
                // The `.pine` file is named after the script, so a new title moves it.
                if old_name != name
                    && let Err(error) = self.store.rename(id, &name)
                {
                    self.log(
                        Level::Error,
                        format!("Could not rename `{old_name}`: {error}"),
                    );
                    return false;
                }
                let previous = &mut self.scripts[at];
                let updated = if previous.version == version {
                    previous.updated
                } else {
                    unix_now()
                };
                *previous = SavedScript {
                    id,
                    name: name.clone(),
                    source: self.draft.clone(),
                    version,
                    updated,
                    starred: previous.starred,
                };
            }
            None => match self
                .store
                .create(ScriptKind::from_source(&self.draft), &name, &self.draft)
            {
                Ok(id) => {
                    self.scripts.push(SavedScript {
                        id,
                        name: name.clone(),
                        source: self.draft.clone(),
                        version: 1,
                        updated: unix_now(),
                        starred: false,
                    });
                    self.editing = Some(self.scripts.len() - 1);
                }
                Err(error) => {
                    self.log(Level::Error, format!("Could not store `{name}`: {error}"));
                    return false;
                }
            },
        }
        let mut reported = false;
        for note in study.diagnostics() {
            self.marks.push(note.line);
            self.log(Level::Warn, format!("line {}: {}", note.line, note.message));
            reported = true;
        }
        if !reported {
            self.log(Level::Info, format!("`{name}` compiled"));
        }
        true
    }

    /// `name` with a copy number that no script other than `keep` already uses.
    fn unique_name(&self, name: &str, keep: Option<usize>) -> String {
        let taken = |candidate: &str| {
            self.scripts
                .iter()
                .enumerate()
                .any(|(at, script)| Some(at) != keep && script.name == candidate)
        };
        let mut candidate = name.to_owned();
        let mut copy = 2;
        while taken(&candidate) {
            candidate = format!("{name} ({copy})");
            copy += 1;
        }
        candidate
    }

    /// Adds the `.pine` files the user picks.
    fn import(&mut self) -> bool {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("Pine script", &["pine"])
            .pick_files()
        else {
            return false;
        };
        let mut added = 0;
        for path in paths {
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) => {
                    self.log(
                        Level::Error,
                        format!("Could not read {}: {error}", path.display()),
                    );
                    continue;
                }
            };
            // A file that does not compile yet still opens, named after the file.
            let name = PineStudy::new(&source)
                .map(|study| study.name().to_owned())
                .unwrap_or_else(|_| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("Imported script")
                        .to_owned()
                });
            let name = self.unique_name(&name, None);
            match self
                .store
                .create(ScriptKind::from_source(&source), &name, &source)
            {
                Ok(id) => {
                    self.scripts.push(SavedScript {
                        id,
                        name,
                        source,
                        version: 1,
                        updated: unix_now(),
                        starred: false,
                    });
                    added += 1;
                }
                Err(error) => self.log(
                    Level::Error,
                    format!("Could not store {}: {error}", path.display()),
                ),
            }
        }
        if added > 0 {
            self.log(Level::Info, format!("Scripts imported: {added}"));
        }
        added > 0
    }

    /// Writes the draft to the `.pine` file the user picks.
    fn export(&mut self) {
        let name = self
            .current()
            .map_or("script", |script| script.name.as_str());
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Pine script", &["pine"])
            .set_file_name(pine_file_name(name))
            .save_file()
        else {
            return;
        };
        let path = if path.extension().is_some() {
            path
        } else {
            path.with_extension("pine")
        };
        match std::fs::write(&path, &self.draft) {
            Ok(()) => self.log(Level::Info, format!("Exported to {}", path.display())),
            Err(error) => self.log(
                Level::Error,
                format!("Could not write {}: {error}", path.display()),
            ),
        }
    }

    fn log(&mut self, level: Level, text: impl Into<String>) {
        self.console.push(ConsoleLine {
            time: chrono::Local::now().format("%-I:%M:%S %p").to_string(),
            text: text.into(),
            level,
        });
        if self.console.len() > 300 {
            self.console.drain(..100);
        }
    }

    /// Writes the pin, the recent list and the stars, so the editor opens the way it was left.
    fn save_state(&self) {
        if self.state_path.as_os_str().is_empty() {
            return;
        }
        let state = EditorState {
            pinned: self.pinned,
            recent: self.recent.clone(),
            starred: self
                .scripts
                .iter()
                .filter(|script| script.starred)
                .map(|script| script.id)
                .collect(),
        };
        let json = match serde_json::to_vec_pretty(&state) {
            Ok(json) => json,
            Err(error) => {
                eprintln!("Pine editor state: {error}");
                return;
            }
        };
        if let Some(parent) = self
            .state_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            eprintln!("Pine editor state: {error}");
            return;
        }
        if let Err(error) = std::fs::write(&self.state_path, json) {
            eprintln!("Pine editor state: {error}");
        }
    }
}

/// A menu entry with an optional shortcut at its right; closes the menu when clicked.
fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let mut button = egui::Button::new(label);
    if !shortcut.is_empty() {
        button = button.shortcut_text(shortcut);
    }
    let clicked = ui.add_enabled(enabled, button).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// The chevron at the right edge of the script name button.
fn paint_chevron(ui: &egui::Ui, rect: Rect) {
    let center = pos2(rect.right() - 12.0, rect.center().y + 0.5);
    let stroke = Stroke::new(1.4, ui.visuals().text_color());
    let painter = ui.painter();
    painter.line_segment(
        [center + vec2(-3.5, -2.0), center + vec2(0.0, 2.0)],
        stroke,
    );
    painter.line_segment([center + vec2(0.0, 2.0), center + vec2(3.5, -2.0)], stroke);
}

/// The spans of `text`, kept for as long as the text does not change.
#[derive(Clone)]
struct SpanCache {
    hash: u64,
    spans: Arc<Vec<(Range<usize>, Token)>>,
}

fn cached_spans(ctx: &egui::Context, text: &str) -> Arc<Vec<(Range<usize>, Token)>> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    let id = egui::Id::unique("pine-span-cache");
    if let Some(cache) = ctx.data(|data| data.get_temp::<SpanCache>(id))
        && cache.hash == hash
    {
        return cache.spans;
    }
    let spans = Arc::new(syntax::tokenize(text));
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            SpanCache {
                hash,
                spans: spans.clone(),
            },
        )
    });
    spans
}

/// The highlighted layout of `text`, in TradingView's colours; it never wraps.
fn pine_layout_job(
    ui: &egui::Ui,
    text: &str,
    spans: &[(Range<usize>, Token)],
    palette: &Palette,
) -> egui::text::LayoutJob {
    let font = TextStyle::Monospace.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    let format = |color: [u8; 3]| egui::TextFormat {
        font_id: font.clone(),
        color: rgba(color, 255),
        ..Default::default()
    };
    let mut at = 0;
    for (range, token) in spans {
        if at < range.start {
            job.append(&text[at..range.start], 0.0, format(palette.plain));
        }
        job.append(&text[range.clone()], 0.0, format(palette.of(*token)));
        at = range.end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, format(palette.plain));
    }
    job
}

fn rgba(color: [u8; 3], alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color[0], color[1], color[2], alpha)
}

/// The byte offset of character `index` in `text`, or its length past the end.
fn byte_at(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The 1-based line and column of the character `index`.
fn line_col(text: &str, index: usize) -> (usize, usize) {
    let (mut line, mut column) = (1, 1);
    for (at, character) in text.chars().enumerate() {
        if at == index {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// The indent a new line after character `index` starts with: the current line's, and one level
/// deeper after `=>` or a line that opens a block.
fn auto_indent(text: &str, index: usize) -> String {
    let before = &text[..byte_at(text, index)];
    let line = before.rsplit('\n').next().unwrap_or_default();
    let mut indent: String = line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let trimmed = line.trim();
    let first = trimmed
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()
        .unwrap_or_default();
    let opens = !trimmed.starts_with("//")
        && (trimmed.ends_with("=>") || matches!(first, "if" | "for" | "while" | "switch" | "else"));
    if opens {
        indent.push_str("    ");
    }
    indent
}

/// Replaces the selection of the field `id` with `insert` and puts the caret after it.
fn insert_text(ctx: &egui::Context, id: egui::Id, text: &mut String, insert: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let range = state.cursor.char_range().unwrap_or_else(|| {
        egui::text::CCursorRange::one(egui::text::CCursor::new(text.chars().count()))
    });
    let [start, end] = range.sorted_cursors();
    text.replace_range(
        byte_at(text, start.index.into())..byte_at(text, end.index.into()),
        insert,
    );
    let caret = egui::text::CCursor::new(start.index + insert.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(caret)));
    state.store(ctx, id);
}

/// The Pine version a script declares, or 6.
fn pine_version(source: &str) -> u32 {
    source
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("//@version=")
                .and_then(|version| version.trim().parse().ok())
        })
        .unwrap_or(6)
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `seconds` since the epoch as local time in `format`, or a dash when unknown.
fn local_time(seconds: i64, format: &str) -> String {
    if seconds == 0 {
        return "\u{2014}".to_owned();
    }
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format(format)
                .to_string()
        })
        .unwrap_or_else(|| "\u{2014}".to_owned())
}

/// `source` with the title of its `indicator`/`study` call set to `name`.
fn script_with_title(source: &str, name: &str) -> String {
    let Some(call) = TITLE.captures(source) else {
        return source.to_owned();
    };
    let prefix = call.get(1).expect("group one is the call").end();
    let end = call.get(0).expect("the whole match").end();
    let mut text = String::with_capacity(source.len() + name.len());
    text.push_str(&source[..prefix]);
    text.push('"');
    text.push_str(&name.replace('"', ""));
    text.push('"');
    text.push_str(&source[end..]);
    text
}

/// The `indicator`/`study` call up to its title, which group one ends before.
static TITLE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r#"(?m)^([ \t]*(?:indicator|study)[ \t]*\([ \t]*)(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')"#,
    )
    .expect("the title pattern is valid")
});

/// `name` as a file name that ends in `.pine`.
fn pine_file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.pine", safe.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An editor over memory with the given `(name, source)` scripts, the first selected.
    fn editor(scripts: &[(&str, &str)]) -> PineScripts {
        let mut pine = PineScripts::default();
        for (name, source) in scripts {
            let id = pine
                .store
                .create(ScriptKind::from_source(source), name, source)
                .expect("stored");
            pine.scripts.push(SavedScript {
                id,
                name: (*name).to_owned(),
                source: (*source).to_owned(),
                version: 1,
                updated: 0,
                starred: false,
            });
        }
        pine.draft = pine
            .scripts
            .first()
            .map_or_else(String::new, |script| script.source.clone());
        pine.editing = (!pine.scripts.is_empty()).then_some(0);
        pine
    }

    /// A docked editor takes its width before the terminal draws, a floating one takes none.
    #[test]
    fn a_pinned_editor_claims_the_width_before_the_terminal() {
        for pinned in [false, true] {
            let ctx = egui::Context::default();
            let screen =
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1200.0, 800.0));
            let mut pine = PineScripts {
                open: true,
                pinned,
                ..PineScripts::default()
            };
            let mut width = screen.width();
            for _ in 0..2 {
                // The test paints no glyphs to a screen, so the frame's texture deltas are dropped
                // on purpose; epaint panics at a bare drop.
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ui| {
                        pine.show(ui, None);
                        width = ui.available_rect_before_wrap().width();
                    },
                )
                .drop_without_applying_deltas();
            }
            if pinned {
                assert!(width < screen.width() - 300.0, "{width}");
            } else {
                assert_eq!(width, screen.width());
            }
        }
    }

    /// The chart offers Add until the script is there, and Update only while it differs.
    #[test]
    fn the_chart_button_follows_the_edited_script() {
        let mut pine = editor(&[("T", "indicator(\"T\")\nplot(close)")]);
        assert_eq!(pine.chart_action(None), Some(ChartAction::Add));
        assert_eq!(pine.chart_action(Some(SeriesId(7))), None);
        pine.draft.push_str("\nplot(open)");
        assert_eq!(
            pine.chart_action(Some(SeriesId(7))),
            Some(ChartAction::Update)
        );
    }

    /// A script added from the study list runs on a thread of its own, not in the frame that
    /// adds it: the factory is built before the editor is ever opened.
    #[test]
    fn a_script_from_the_study_list_runs_off_the_frame() {
        let mut pine = editor(&[("T", "//@version=6\nindicator(\"T\")\nplot(close)\n")]);
        pine.attach(&egui::Context::default());
        let mut study = pine.factory().create("T").unwrap();
        let bars: Vec<_> = (0..1200)
            .map(|at| {
                egui_charts::model::Bar::new(
                    chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(at),
                    1.0,
                    2.0,
                    0.5,
                    1.5,
                    10.0,
                )
            })
            .collect();
        study.update_data(&bars, 0, &[]);
        assert!(
            study.values().is_empty(),
            "the run still happens in the frame that adds the study"
        );
    }

    /// Renaming rewrites the title the indicator call gives the script.
    #[test]
    fn renaming_rewrites_the_indicator_title() {
        assert_eq!(
            script_with_title(
                "//@version=6\nindicator(\"Old\", overlay=true)\nplot(close)",
                "New"
            ),
            "//@version=6\nindicator(\"New\", overlay=true)\nplot(close)"
        );
        // A script without an indicator call keeps its source.
        assert_eq!(script_with_title("plot(close)", "New"), "plot(close)");
    }

    /// Saving, renaming and deleting go through the store, so a restart keeps what the editor
    /// shows.
    #[test]
    fn the_editor_keeps_its_scripts_in_the_store() {
        let mut pine = editor(&[]);
        pine.draft = "indicator(\"Fresh\")\nplot(close)".to_owned();
        assert!(pine.apply());
        assert_eq!(pine.scripts.len(), 1);
        let id = pine.scripts[0].id;
        assert_eq!(pine.scripts[0].version, 1);
        assert_eq!(pine.store.load(id).unwrap().source, pine.draft);
        // An unchanged save is not a new version; an edited one is.
        assert!(pine.apply());
        assert_eq!(pine.scripts[0].version, 1);
        assert_eq!(pine.store.history(id).unwrap().len(), 1);
        pine.draft.push_str("\nplot(open)");
        assert!(pine.apply());
        assert_eq!(pine.scripts[0].version, 2);
        assert_eq!(pine.store.history(id).unwrap().len(), 2);
        assert_eq!(pine.store.load(id).unwrap().source, pine.draft);

        pine.rename_draft = "Renamed".to_owned();
        assert!(pine.commit_rename());
        assert_eq!(pine.scripts[0].name, "Renamed");
        assert_eq!(pine.store.load(id).unwrap().meta.title, "Renamed");

        assert!(pine.delete_script(0));
        assert!(pine.scripts.is_empty());
        assert!(pine.store.list(None).unwrap().is_empty());
    }

    /// The caret position the status bar shows counts lines and columns from one.
    #[test]
    fn the_caret_is_reported_from_one() {
        assert_eq!(line_col("ab\ncd", 0), (1, 1));
        assert_eq!(line_col("ab\ncd", 2), (1, 3));
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
        // Characters, not bytes.
        assert_eq!(line_col("é\nx", 3), (2, 2));
    }

    /// Enter keeps the indent of its line and opens a level after `=>` and block keywords.
    #[test]
    fn enter_keeps_and_opens_the_indent() {
        assert_eq!(auto_indent("    x = 1", 9), "    ");
        assert_eq!(auto_indent("f(a) =>", 7), "    ");
        assert_eq!(auto_indent("if close > open", 15), "    ");
        assert_eq!(auto_indent("    // if x", 10), "    ");
        // The caret in the middle of a line looks at the text before it only.
        assert_eq!(auto_indent("if a\nplot(close)", 4), "    ");
    }

    /// The Pine version comes from the annotation, and defaults to 6.
    #[test]
    fn the_pine_version_comes_from_the_annotation() {
        assert_eq!(pine_version("//@version=5\nplot(close)"), 5);
        assert_eq!(pine_version("plot(close)"), 6);
    }
}
