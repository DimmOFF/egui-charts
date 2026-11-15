//! The user's Pine scripts: the study factory they feed, the editor window and the file that
//! keeps them between runs.

use std::path::PathBuf;

use egui_charts::icons::icons;
use egui_charts::studies::{CustomIndicator, Indicator, IndicatorFactory};
use egui_charts::ui_kit::buttons::{ButtonSize, IconButton};

use crate::pine_engine::PineStudy;

/// One stored script, named by the title its `indicator`/`study` call gives it.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct SavedScript {
    name: String,
    source: String,
}

pub struct PineScripts {
    path: PathBuf,
    scripts: Vec<SavedScript>,
    /// Whether the editor window is open.
    pub open: bool,
    /// Whether the editor is docked beside the terminal's right rail instead of floating.
    pinned: bool,
    draft: String,
    /// The index of the selected script, or `None` while a new one is edited.
    editing: Option<usize>,
    status: Vec<String>,
}

impl PineScripts {
    /// Reads the saved scripts, importing the development copies on the first run.
    pub fn load() -> Self {
        let path = crate::deriv_services::default_path().with_file_name("pine_scripts.json");
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
            scripts,
            open: false,
            pinned: false,
            draft,
            editing,
            status: Vec::new(),
        }
    }

    /// The built-in studies plus one entry per saved script, under My scripts.
    pub fn factory(&self) -> IndicatorFactory {
        let mut factory = IndicatorFactory::new();
        for script in &self.scripts {
            let source = script.source.clone();
            let fallback = script.name.clone();
            factory.register_in(IndicatorFactory::PERSONAL, script.name.clone(), move || {
                match PineStudy::new(&source) {
                    Ok(study) => Box::new(study),
                    // A saved script that no longer parses stays listed instead of taking the
                    // terminal down; the editor reports the syntax error when it is opened.
                    Err(_) => Box::new(CustomIndicator::new(fallback.clone(), |_| Vec::new())),
                }
            });
        }
        factory
    }

    /// Draws the editor. Returns whether the scripts changed, so the caller rebuilds its factory.
    ///
    /// A pinned editor claims its width from `ui` before the terminal draws its frame, which
    /// docks it to the right of the terminal's right rail; otherwise it floats in a window.
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.open {
            return false;
        }
        if self.pinned {
            self.show_docked(ui)
        } else {
            self.show_window(ui.ctx())
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) -> bool {
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
                let (body_changed, body_close) = self.body(ui);
                changed = body_changed;
                close |= body_close;
            });
        if close {
            self.open = false;
        }
        changed
    }

    fn show_docked(&mut self, ui: &mut egui::Ui) -> bool {
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
                let (body_changed, body_close) = self.body(ui);
                changed = body_changed;
                close |= body_close;
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

    /// The script picker, the code and the actions. Returns `(scripts changed, close clicked)`.
    fn body(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
        let mut changed = false;
        let mut close = false;
        ui.horizontal(|ui| {
            let current = self
                .editing
                .and_then(|at| self.scripts.get(at))
                .map(|script| script.name.as_str())
                .unwrap_or("—");
            let mut pick = None;
            egui::ComboBox::from_label("Script")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (at, script) in self.scripts.iter().enumerate() {
                        if ui
                            .selectable_label(self.editing == Some(at), &script.name)
                            .clicked()
                        {
                            pick = Some(at);
                        }
                    }
                });
            if let Some(at) = pick {
                self.editing = Some(at);
                self.draft = self.scripts[at].source.clone();
                self.status.clear();
            }
            if ui.button("New").clicked() {
                self.editing = None;
                self.draft.clear();
                self.status.clear();
            }
            if ui.button("Delete").clicked()
                && let Some(at) = self.editing.take()
                && at < self.scripts.len()
            {
                self.scripts.remove(at);
                self.save();
                self.draft.clear();
                self.status.clear();
                changed = true;
            }
        });
        ui.separator();
        // Without a bounded height the editor asks for every row it holds, the window grows
        // past the screen and its close button goes with it. The width is passed in too:
        // left to itself the field shrinks to a few characters inside the scroll area.
        let width = ui.available_width();
        let height = (ui.available_height() - 60.0).clamp(160.0, 900.0);
        egui::ScrollArea::vertical()
            .id_salt("pine-code")
            .max_height(height)
            .show(ui, |ui| {
                ui.set_min_width(width);
                ui.add(
                    egui::TextEdit::multiline(&mut self.draft)
                        .code_editor()
                        .desired_rows(24)
                        .desired_width(width),
                );
            });
        ui.horizontal(|ui| {
            if ui.button("Apply").clicked() {
                changed = true;
                self.apply();
            }
            if ui.button("Import temp/pine").clicked() {
                changed = true;
                self.import();
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        for line in &self.status {
            ui.label(line);
        }
        (changed, close)
    }

    /// Compiles the draft, stores it and reports what it could not run.
    fn apply(&mut self) {
        self.status.clear();
        let study = match PineStudy::new(&self.draft) {
            Ok(study) => study,
            Err(error) => {
                self.status.push(error);
                return;
            }
        };
        let mut name = study.name().to_owned();
        let taken = |scripts: &[SavedScript], editing: Option<usize>, name: &str| {
            scripts
                .iter()
                .enumerate()
                .any(|(at, script)| Some(at) != editing && script.name == name)
        };
        let mut copy = 2;
        while taken(&self.scripts, self.editing, &name) {
            name = format!("{} ({copy})", study.name());
            copy += 1;
        }
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
    }

    /// Adds the scripts of the development folder that are not stored yet.
    fn import(&mut self) {
        let mut added = 0;
        for script in import_folder() {
            if self.scripts.iter().any(|stored| stored.name == script.name) {
                continue;
            }
            self.scripts.push(script);
            added += 1;
        }
        self.status = vec![format!("Scripts imported: {added}")];
        self.save();
    }

    fn save(&self) {
        if let Err(error) = write_scripts(&self.path, &self.scripts) {
            eprintln!("Pine scripts: {error}");
        }
    }
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
                path: PathBuf::new(),
                scripts: Vec::new(),
                open: true,
                pinned,
                draft: String::new(),
                editing: None,
                status: Vec::new(),
            };
            let mut width = screen.width();
            for _ in 0..2 {
                let _ = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ui| {
                        pine.show(ui);
                        width = ui.available_rect_before_wrap().width();
                    },
                );
            }
            if pinned {
                assert!(width < screen.width() - 300.0, "{width}");
            } else {
                assert_eq!(width, screen.width());
            }
        }
    }
}
