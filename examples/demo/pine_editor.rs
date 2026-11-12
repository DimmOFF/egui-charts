//! The user's Pine scripts: the study factory they feed, the editor window and the file that
//! keeps them between runs.

use std::path::PathBuf;

use egui_charts::studies::{CustomIndicator, Indicator, IndicatorFactory};

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
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        if !self.open {
            return false;
        }
        let mut open = self.open;
        let mut close = false;
        let mut changed = false;
        egui::Window::new("Pine-скрипты")
            .open(&mut open)
            .default_size([860.0, 640.0])
            .min_size([420.0, 320.0])
            .max_size([1100.0, 900.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let current = self
                        .editing
                        .and_then(|at| self.scripts.get(at))
                        .map(|script| script.name.as_str())
                        .unwrap_or("—");
                    let mut pick = None;
                    egui::ComboBox::from_label("Скрипт")
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
                    if ui.button("Новый").clicked() {
                        self.editing = None;
                        self.draft.clear();
                        self.status.clear();
                    }
                    if ui.button("Удалить").clicked()
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
                    if ui.button("Применить").clicked() {
                        changed = true;
                        self.apply();
                    }
                    if ui.button("Импорт temp/pine").clicked() {
                        changed = true;
                        self.import();
                    }
                    if ui.button("Закрыть").clicked() {
                        close = true;
                    }
                });
                for line in &self.status {
                    ui.label(line);
                }
            });
        self.open = open && !close;
        changed
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
                .push(format!("строка {}: {}", note.line, note.message));
        }
        if self.status.is_empty() {
            self.status.push(format!("«{name}» скомпилирован"));
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
        self.status = vec![format!("Импортировано скриптов: {added}")];
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
