//! The user's Pine scripts: the study factory they feed, the editor window and the file that
//! keeps them between runs.

use std::path::PathBuf;
use std::sync::LazyLock;

use egui_charts::icons::icons;
use egui_charts::studies::{CustomIndicator, Indicator, IndicatorFactory};
use egui_charts::theme::ThemeExt;
use egui_charts::ui_kit::buttons::{Button, ButtonSize, IconButton};
use egui_charts::widget::{Chart, SeriesId};

use crate::PineStudy;

/// What a new script starts as.
const NEW_SCRIPT: &str = "//@version=6\nindicator(\"My script\")\nplot(close)\n";

/// One stored script, named by the title its `indicator`/`study` call gives it.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct SavedScript {
    name: String,
    source: String,
}

#[derive(Default)]
pub struct PineScripts {
    path: PathBuf,
    /// Where the editor's own state, the pin, lives.
    state_path: PathBuf,
    scripts: Vec<SavedScript>,
    /// Whether the editor window is open.
    pub open: bool,
    /// Whether the editor is docked beside the terminal's right rail instead of floating.
    pinned: bool,
    draft: String,
    /// The index of the selected script, or `None` while a new one is edited.
    editing: Option<usize>,
    status: Vec<String>,
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

/// A compiled script for the host to add to the chart.
pub struct ChartPush {
    /// The series this script replaces, for an update.
    pub replace: Option<SeriesId>,
    pub indicator: Box<dyn Indicator>,
}

/// What the top row offers besides the script menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChartAction {
    Add,
    Update,
}

/// A script menu entry clicked in the current frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAction {
    New,
    Rename,
    Save,
    Import,
    Export,
    Delete,
    Add,
    Update,
}

/// The editor state kept beside the scripts.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct EditorState {
    pinned: bool,
}

impl PineScripts {
    /// Reads the saved scripts from `path`, importing the development copies on the first run.
    pub fn load(path: PathBuf) -> Self {
        let state_path = path.with_file_name("pine_editor.json");
        let pinned = std::fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<EditorState>(&bytes).ok())
            .is_some_and(|state| state.pinned);
        let scripts = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| {
                let imported = import_folder();
                if !imported.is_empty() {
                    let _ = write_scripts(&path, &imported);
                }
                imported
            });
        let draft = scripts
            .first()
            .map(|script| script.source.clone())
            .unwrap_or_default();
        let editing = (!scripts.is_empty()).then_some(0);
        Self {
            path,
            state_path,
            scripts,
            draft,
            editing,
            pinned,
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
        // The window's own title bar has no room for the pin, so the editor draws the row.
        egui::Window::new("Pine Editor")
            .title_bar(false)
            .default_size([860.0, 640.0])
            .min_size([420.0, 320.0])
            .max_size([1100.0, 900.0])
            .resizable(true)
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
        egui::Panel::right("egui_charts_pine_editor")
            .resizable(true)
            .default_size(480.0)
            .min_size(320.0)
            .max_size(max_width)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(8, 8)),
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
        ui.separator();
    }

    /// The script menu, the name, the chart button, the numbered code and the status.
    fn body(&mut self, ui: &mut egui::Ui, on_chart: Option<SeriesId>) -> bool {
        let mut changed = false;
        let mut action = None;
        ui.horizontal(|ui| {
            let (menu, _) = egui::containers::menu::MenuButton::from_button(
                icons::MENU
                    .as_button_square(20.0)
                    .frame_when_inactive(false)
                    .min_size(egui::Vec2::splat(ButtonSize::MD.pixels())),
            )
            .ui(ui, |ui| {
                ui.menu_button("Create new", |ui| {
                    if ui.button("Indicator").clicked() {
                        action = Some(MenuAction::New);
                        ui.close();
                    }
                });
                if ui.button("Rename").clicked() {
                    action = Some(MenuAction::Rename);
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    action = Some(MenuAction::Save);
                    ui.close();
                }
                if ui.button("Import").clicked() {
                    action = Some(MenuAction::Import);
                    ui.close();
                }
                if ui.button("Export").clicked() {
                    action = Some(MenuAction::Export);
                    ui.close();
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    action = Some(MenuAction::Delete);
                    ui.close();
                }
            });
            menu.on_hover_text("Script menu");
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
                        changed |= self.commit_rename();
                    }
                }
            } else {
                self.name_menu(ui);
            }
            match self.chart_action(on_chart) {
                Some(ChartAction::Add) => {
                    if Button::new("Add to chart").primary().show(ui).clicked() {
                        action = Some(MenuAction::Add);
                    }
                }
                Some(ChartAction::Update) => {
                    if Button::new("Update").primary().show(ui).clicked() {
                        action = Some(MenuAction::Update);
                    }
                }
                None => {}
            }
        });
        match action {
            Some(MenuAction::New) => {
                self.editing = None;
                self.draft = NEW_SCRIPT.to_owned();
                self.status.clear();
            }
            Some(MenuAction::Rename) => {
                self.renaming = true;
                self.rename_draft = self.current().map_or_else(String::new, |s| s.name.clone());
                self.rename_focus = true;
            }
            Some(MenuAction::Save) => changed |= self.apply(),
            Some(MenuAction::Import) => changed |= self.import(),
            Some(MenuAction::Export) => self.export(),
            Some(MenuAction::Delete) => {
                if let Some(at) = self.editing.take()
                    && at < self.scripts.len()
                {
                    self.scripts.remove(at);
                    self.draft.clear();
                    self.status.clear();
                    self.save();
                    changed = true;
                }
            }
            Some(MenuAction::Add) => changed |= self.add_to_chart(None),
            Some(MenuAction::Update) => changed |= self.add_to_chart(on_chart),
            None => {}
        }
        ui.separator();
        // Without a bounded height the editor asks for every row it holds, the window grows
        // past the screen and its close button goes with it. The width is passed in too:
        // left to itself the field shrinks to a few characters inside the scroll area. Only the
        // diagnostics at the bottom take their space out of the code area in pinned mode.
        let reserved = if self.status.is_empty() { 0.0 } else { 80.0 };
        let height = (ui.available_height() - reserved).max(120.0);
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let digit = ui.fonts_mut(|fonts| fonts.glyph_width(&font, '0'));
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let rows = ((height - 8.0) / row_height).max(4.0) as usize;
        let lines = self.draft.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let gutter = digit * lines.to_string().len() as f32 + 8.0;
        egui::ScrollArea::vertical()
            .id_salt("pine-code")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    let (gutter_rect, _) =
                        ui.allocate_exact_size(egui::vec2(gutter, 0.0), egui::Sense::hover());
                    let width = (ui.available_width() - 8.0).max(80.0);
                    let mut layouter =
                        |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
                            let mut job = pine_layout_job(ui, buffer.as_str());
                            job.wrap.max_width = wrap_width;
                            ui.fonts_mut(|fonts| fonts.layout_job(job))
                        };
                    let output = egui::TextEdit::multiline(&mut self.draft)
                        .code_editor()
                        .desired_rows(rows)
                        .desired_width(width)
                        .layouter(&mut layouter)
                        .show(ui);
                    paint_line_numbers(ui, gutter_rect, &output);
                });
            });
        if !self.status.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("pine-status")
                .max_height(80.0)
                .show(ui, |ui| {
                    for line in &self.status {
                        ui.label(line);
                    }
                });
        }
        changed
    }

    /// The selected script's name, as a menu that switches to another stored script.
    fn name_menu(&mut self, ui: &mut egui::Ui) {
        let name = self
            .current()
            .map_or("Untitled script", |script| script.name.as_str());
        let mut pick = None;
        ui.menu_button(name, |ui| {
            for (at, script) in self.scripts.iter().enumerate() {
                if ui
                    .selectable_label(self.editing == Some(at), &script.name)
                    .clicked()
                {
                    pick = Some(at);
                    ui.close();
                }
            }
        });
        if let Some(at) = pick {
            self.editing = Some(at);
            self.draft = self.scripts[at].source.clone();
            self.status.clear();
        }
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
        self.status.clear();
        let study = match PineStudy::new(&self.draft) {
            Ok(study) => study,
            Err(error) => {
                self.status.push(error);
                return false;
            }
        };
        let name = self.unique_name(study.name(), self.editing);
        let script = SavedScript {
            name: name.clone(),
            source: self.draft.clone(),
        };
        match self.editing {
            Some(at) if at < self.scripts.len() => self.scripts[at] = script,
            _ => {
                self.scripts.push(script);
                self.editing = Some(self.scripts.len() - 1);
            }
        }
        for note in study.diagnostics() {
            self.status
                .push(format!("line {}: {}", note.line, note.message));
        }
        if self.status.is_empty() {
            self.status.push(format!("`{name}` compiled"));
        }
        self.save();
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
        let mut status = Vec::new();
        let mut added = 0;
        for path in paths {
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) => {
                    status.push(format!("Could not read {}: {error}", path.display()));
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
            self.scripts.push(SavedScript { name, source });
            added += 1;
        }
        if added > 0 {
            status.push(format!("Scripts imported: {added}"));
            self.save();
        }
        self.status = status;
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
        self.status = vec![match std::fs::write(&path, &self.draft) {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(error) => format!("Could not write {}: {error}", path.display()),
        }];
    }

    fn save(&self) {
        if let Err(error) = write_scripts(&self.path, &self.scripts) {
            eprintln!("Pine scripts: {error}");
        }
    }

    /// Writes the pin state, so the editor docks the way it was left.
    fn save_state(&self) {
        let state = EditorState {
            pinned: self.pinned,
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

/// The line numbers of `output` in the `gutter` left of it.
fn paint_line_numbers(ui: &egui::Ui, gutter: egui::Rect, output: &egui::text_edit::TextEditOutput) {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.active_theme().ui.text_muted;
    let painter = ui.painter();
    let mut line = 1;
    // A wrapped line spans several rows; only its first row carries the number.
    let mut starts_line = true;
    for row in &output.galley.rows {
        if starts_line {
            painter.text(
                egui::pos2(gutter.right() - 4.0, output.galley_pos.y + row.pos.y),
                egui::Align2::RIGHT_TOP,
                line.to_string(),
                font.clone(),
                color,
            );
            line += 1;
        }
        starts_line = row.ends_with_newline;
    }
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

fn write_scripts(path: &PathBuf, scripts: &[SavedScript]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(scripts)
        .map_err(|error| format!("Could not encode the scripts: {error}"))?;
    std::fs::write(path, json)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
}

/// The `.pine` and `.txt` scripts of `temp/pine`, when this runs beside the sources.
fn import_folder() -> Vec<SavedScript> {
    // The binary runs either from `examples/demo` or from the crate root.
    let entries = ["temp/pine", "../../temp/pine"]
        .into_iter()
        .find_map(|directory| std::fs::read_dir(directory).ok());
    let Some(entries) = entries else {
        return Vec::new();
    };
    let mut scripts: Vec<SavedScript> = entries
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "pine" | "txt"))
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|source| {
            let study = PineStudy::new(&source).ok()?;
            Some(SavedScript {
                name: study.name().to_owned(),
                source,
            })
        })
        .collect();
    scripts.sort_by(|first, second| first.name.cmp(&second.name));
    scripts
}

/// The Pine words the editor colours as keywords.
const KEYWORDS: &[&str] = &[
    "and", "break", "by", "continue", "else", "enum", "export", "false", "for", "if", "import",
    "in", "method", "na", "not", "or", "return", "switch", "to", "true", "type", "var", "varip",
    "while",
];

/// Built-in namespaces; their members (`ta.sma`) are built-ins too.
const NAMESPACES: &[&str] = &[
    "array",
    "barstate",
    "box",
    "chart",
    "color",
    "currency",
    "dayofweek",
    "input",
    "label",
    "line",
    "map",
    "math",
    "matrix",
    "plot",
    "polyline",
    "request",
    "runtime",
    "session",
    "str",
    "strategy",
    "syminfo",
    "ta",
    "table",
    "timeframe",
];

/// Built-in functions and variables, besides the namespaces.
const BUILTINS: &[&str] = &[
    "alert",
    "alertcondition",
    "bar_index",
    "barcolor",
    "bgcolor",
    "close",
    "dayofmonth",
    "fill",
    "fixnan",
    "high",
    "hline",
    "hl2",
    "hlc3",
    "hour",
    "indicator",
    "last_bar_index",
    "library",
    "low",
    "max_bars_back",
    "minute",
    "month",
    "nz",
    "ohlc4",
    "open",
    "plotbar",
    "plotcandle",
    "plotchar",
    "plotshape",
    "second",
    "study",
    "time",
    "time_close",
    "timestamp",
    "volume",
    "year",
];

/// What the editor colours a piece of the source as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PineKind {
    Keyword,
    Builtin,
    String,
    Number,
    Comment,
}

/// The coloured spans of `text`, in source order; everything else stays plain.
///
/// This tolerates half-typed code: an unterminated string runs to the end of its line and an
/// unknown character is skipped, where the compiler's lexer would report an error.
fn pine_spans(text: &str) -> Vec<(std::ops::Range<usize>, PineKind)> {
    let mut spans = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next().expect("at is a character boundary");
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(text.len(), |newline| at + newline);
            spans.push((at..end, PineKind::Comment));
            at = end;
        } else if c == '"' || c == '\'' {
            let end = string_end(text, at, c);
            spans.push((at..end, PineKind::String));
            at = end;
        } else if c == '#' {
            let digits = rest[1..]
                .chars()
                .take_while(|digit| digit.is_ascii_hexdigit())
                .count();
            let start = at;
            // A colour literal is six or eight hex digits; a half-typed one stays plain.
            if digits >= 6 {
                at += 1 + digits;
                spans.push((start..at, PineKind::Number));
            } else {
                at += 1;
            }
        } else if c.is_ascii_digit()
            || (c == '.'
                && rest
                    .chars()
                    .nth(1)
                    .is_some_and(|digit| digit.is_ascii_digit()))
        {
            let end = number_end(text, at);
            spans.push((at..end, PineKind::Number));
            at = end;
        } else if c.is_alphabetic() || c == '_' {
            let start = at;
            let mut end = at;
            for (offset, character) in rest.char_indices() {
                if character.is_alphanumeric() || character == '_' {
                    end = at + offset + character.len_utf8();
                } else {
                    break;
                }
            }
            let name = &text[start..end];
            let kind = if KEYWORDS.contains(&name) {
                Some(PineKind::Keyword)
            } else if NAMESPACES.contains(&name)
                || BUILTINS.contains(&name)
                || follows_namespace(&text[..start])
            {
                Some(PineKind::Builtin)
            } else {
                None
            };
            if let Some(kind) = kind {
                spans.push((start..end, kind));
            }
            at = end;
        } else {
            at += c.len_utf8();
        }
    }
    spans
}

/// Whether `before` ends with `namespace.`, so the identifier after it is a member.
fn follows_namespace(before: &str) -> bool {
    let Some(before) = before.strip_suffix('.') else {
        return false;
    };
    let name = before
        .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
        .next()
        .unwrap_or_default();
    NAMESPACES.contains(&name)
}

/// The end of the string literal opening at `at`, or the end of its line if it never closes.
fn string_end(text: &str, at: usize, quote: char) -> usize {
    let mut escaped = false;
    for (offset, character) in text[at + quote.len_utf8()..].char_indices() {
        if character == '\n' {
            return at + quote.len_utf8() + offset;
        }
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return at + quote.len_utf8() + offset + character.len_utf8();
        }
    }
    text.len()
}

/// The end of the number literal opening at `at`.
fn number_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = at;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        let digits = end + 1;
        let mut exponent = digits;
        if exponent < bytes.len() && matches!(bytes[exponent], b'+' | b'-') {
            exponent += 1;
        }
        while exponent < bytes.len() && bytes[exponent].is_ascii_digit() {
            exponent += 1;
        }
        if exponent > digits {
            end = exponent;
        }
    }
    end
}

/// The highlighted layout of `text`, in the colours of the active theme.
fn pine_layout_job(ui: &egui::Ui, text: &str) -> egui::text::LayoutJob {
    let theme = ui.active_theme();
    let colors = theme.ui;
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let plain = ui.visuals().text_color();
    let mut job = egui::text::LayoutJob::default();
    let format = |color, italics| egui::TextFormat {
        font_id: font.clone(),
        color,
        italics,
        ..Default::default()
    };
    let mut at = 0;
    for (range, kind) in pine_spans(text) {
        if at < range.start {
            job.append(&text[at..range.start], 0.0, format(plain, false));
        }
        let (color, italics) = match kind {
            PineKind::Keyword => (colors.accent, false),
            PineKind::Builtin => (colors.info, false),
            PineKind::String => (colors.warning, false),
            PineKind::Number => (colors.success, false),
            PineKind::Comment => (colors.text_muted, true),
        };
        job.append(&text[range.clone()], 0.0, format(color, italics));
        at = range.end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, format(plain, false));
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let source = "indicator(\"T\")\nplot(close)".to_owned();
        let mut pine = PineScripts {
            scripts: vec![SavedScript {
                name: "T".to_owned(),
                source: source.clone(),
            }],
            draft: source,
            editing: Some(0),
            ..PineScripts::default()
        };
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
        let source = "//@version=6\nindicator(\"T\")\nplot(close)\n".to_owned();
        let mut pine = PineScripts {
            scripts: vec![SavedScript {
                name: "T".to_owned(),
                source,
            }],
            ..PineScripts::default()
        };
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

    /// Comments, strings, keywords, built-ins, numbers and colours get their own runs.
    #[test]
    fn the_editor_colours_pine_syntax() {
        let source =
            "//@version=6\nindicator(\"T\", overlay=true)\nx = ta.sma(close, 20) + 1.5\n#ff8800";
        let spans: Vec<(PineKind, &str)> = pine_spans(source)
            .into_iter()
            .map(|(range, kind)| (kind, &source[range]))
            .collect();
        assert_eq!(
            spans,
            vec![
                (PineKind::Comment, "//@version=6"),
                (PineKind::Builtin, "indicator"),
                (PineKind::String, "\"T\""),
                (PineKind::Keyword, "true"),
                (PineKind::Builtin, "ta"),
                (PineKind::Builtin, "sma"),
                (PineKind::Builtin, "close"),
                (PineKind::Number, "20"),
                (PineKind::Number, "1.5"),
                (PineKind::Number, "#ff8800"),
            ]
        );
    }
}
