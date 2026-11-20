//! Layouts: chart slots, layouts and the layout tabs.
//!
//! A layout is the arrangement of the chart slots with their charts. The terminal shows one
//! layout at a time; the others wait in tabs and are swapped in when selected. Saving, opening,
//! renaming and deleting go through the layout menu, which keeps layouts in the host's
//! [`Storage`](egui_charts::services::Storage).

use super::core::Direction;
use super::layout::Region;
use super::{Action, Terminal};
use egui_charts::config::{ChartConfig, KeyboardAction};
use egui_charts::localization as l10n;
use egui_charts::services::{
    StoreKind, TabSettings, TabTitlePart, UserSettings, default_tab_title,
};
use egui_charts::theme::Theme;
use egui_charts::ui::Control;
use egui_charts::ui::export::{ExportConfig, ExportFormat, ExportResponse};
use egui_charts::ui::floating_replay_control::ReplayControlAction;
use egui_charts::ui::top_toolbar::{LayoutAction, LayoutMenu, TopToolbarAction};
use egui_charts::ui_kit::dialog::DialogFooter;
use egui_charts::ui_kit::modal::Modal;
use egui_charts::widget::{
    Chart, ChartBuilder, ChartLayout, ChartLayoutMode, SavedLayout, SeriesId, ShortcutRequest,
};
use std::collections::VecDeque;

/// The height of the strip of layout tabs, as TradingView's window tabs.
const TAB_STRIP_HEIGHT: f32 = 32.0;

/// An open tab. The one on show lives in the terminal; the others keep their slots here.
struct Tab {
    /// The tab's number, which names its document in [`StoreKind::Tabs`].
    id: u64,
    /// The name a layout is saved under while the tab shows none.
    name: String,
    /// The title the user gave the tab; the parts of the tab title when `None`.
    title: Option<String>,
    /// The saved layout the tab shows, if any.
    layout: Option<String>,
    /// Whether the tab still asks what to open: a new tab shows the choice of a saved layout or
    /// a new one until the user picks.
    start: bool,
    /// The edits the tab's own document was last written at, so closing does not write it again.
    saved: Option<u64>,
    workspace: Option<Workspace>,
    /// The ticker, last price and price change of the tab's first chart, for the strip.
    ticker: String,
    last: f64,
    change: f64,
}

/// The chart slots of a tab that is not on show.
struct Workspace {
    charts: Vec<Chart>,
    layout: ChartLayout,
    active: usize,
}

/// The tabs read from the settings, which reopen their own documents one at a time.
struct Restore {
    /// The tabs the settings had, kept while they are reopened.
    saved: Vec<TabSettings>,
    /// The tabs still to read, after the one being read.
    pending: VecDeque<TabSettings>,
    /// The tab being read, to notice one whose document the store could not read and skip it.
    current: Option<TabSettings>,
    /// Whether the tab on show has taken the first stored one.
    filled: bool,
    /// The tab to show once the list is back.
    active: Option<u64>,
}

impl Tab {
    fn unnamed(id: u64) -> Self {
        Self {
            id,
            name: "Unnamed".into(),
            title: None,
            layout: None,
            start: false,
            saved: None,
            workspace: None,
            ticker: String::new(),
            last: 0.0,
            change: 0.0,
        }
    }
}

pub(super) struct State {
    tabs: Vec<Tab>,
    /// The tab on show.
    tab: usize,
    /// Whether the open-layout chooser is open.
    chooser: bool,
    /// The name staged in the Create new layout dialog; `None` while the dialog is closed.
    creating: Option<String>,
    /// Grid changes, counted with the chart edits so autosave sees them.
    grid_edits: u64,
    /// The slot that was active at the last poll, to notice a change of slot.
    seen_active: usize,
    /// Reads and writes the tabs' own documents, apart from the saved layouts.
    store: LayoutMenu,
    /// The tabs being reopened after a restart, until the list is back.
    restoring: Option<Restore>,
    /// Whether the tabs were read from the settings; a change by another window leaves them.
    restored: bool,
    /// Whether the tabs open at exit come back on start.
    pub(super) auto_restore: bool,
    /// The parts of a tab title, in order.
    pub(super) title_parts: Vec<TabTitlePart>,
    /// The parts of a tab title the user hid.
    pub(super) title_hidden: Vec<TabTitlePart>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            tab: 0,
            chooser: false,
            creating: None,
            grid_edits: 0,
            seen_active: 0,
            store: LayoutMenu::new_in(StoreKind::Tabs),
            restoring: None,
            restored: false,
            auto_restore: true,
            title_parts: default_tab_title(),
            title_hidden: Vec::new(),
        }
    }
}

/// The name of the document of the tab numbered `id`.
fn tab_document(id: u64) -> String {
    format!("tab {id}")
}

/// Fills the ticker, last price and price change of `tab` from its first chart.
fn refresh_tab_market(charts: &[Chart], tab: &mut Tab) {
    let Some(chart) = charts.first() else {
        return;
    };
    let bars = chart.bars(SeriesId::MAIN);
    let last = bars.last().map_or(0.0, |bar| bar.close);
    let previous = bars
        .get(bars.len().saturating_sub(2))
        .map_or(last, |bar| bar.close);
    tab.ticker = chart.symbol().to_owned();
    tab.last = last;
    tab.change = if previous == 0.0 {
        0.0
    } else {
        (last / previous - 1.0) * 100.0
    };
}

impl Terminal {
    pub(super) fn layouts_init(&mut self) {
        self.layouts.tabs.push(Tab::unnamed(1));
        if let Some(storage) = self.services.storage.clone() {
            self.top_toolbar.layout_menu.set_storage(storage.clone());
            self.layouts.store.set_storage(storage);
            self.set_control_visible(Control::Layouts, true);
        }
    }

    pub(super) fn layouts_poll(&mut self, ctx: &egui::Context) {
        // A replay belongs to the chart it was started on.
        if self.layouts.seen_active != self.active {
            self.layouts.seen_active = self.active;
            self.layout_stop_replay();
        }
        self.restore_poll(ctx);
        if let Some((name, layout)) = self.top_toolbar.layout_menu.poll(ctx) {
            self.restore_layout(name, layout);
        }
        self.refresh_layout_menu();
        if let Some(error) = self.top_toolbar.layout_menu.take_error() {
            self.toasts.error(error);
        }
        let edits = self.layout_edits();
        if self.top_toolbar.layout_menu.autosave_due(ctx, edits) {
            self.save_layout();
        }
    }

    /// The chart and grid edits of the layouts on show, which autosave compares with the count it
    /// saved.
    pub(super) fn layout_edits(&self) -> u64 {
        self.charts
            .iter()
            .map(Chart::edit_count)
            .fold(self.layouts.grid_edits, u64::wrapping_add)
    }

    pub(super) fn layouts_show(&mut self, ui: &mut egui::Ui, region: Region) {
        match region {
            Region::Top => self.show_layout_tabs(ui),
            // The toolbar has been drawn: its grid selector holds this frame's sync options.
            Region::Left => self.layout.sync = self.top_toolbar.chart_grid_selector.sync.clone(),
            Region::Windows => {
                self.show_open_layout(ui.ctx());
                self.show_create_layout(ui.ctx());
            }
            _ => {}
        }
    }

    pub(super) fn layouts_perform(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::TopToolbar(TopToolbarAction::Save)
            | Action::Shortcut(ShortcutRequest::Action(KeyboardAction::SaveLayout)) => {
                if self.services.storage.is_none() {
                    return Some(action);
                }
                self.save_layout();
            }
            Action::TopToolbar(TopToolbarAction::Layout(action)) | Action::Layout(action) => {
                return self.perform_layout_action(action);
            }
            Action::Shortcut(ShortcutRequest::Action(
                request @ (KeyboardAction::CycleChartFocus | KeyboardAction::CycleChartFocusBack),
            )) => {
                let count = self.layout.mode.chart_count().min(self.charts.len());
                let delta = if request == KeyboardAction::CycleChartFocus {
                    1
                } else {
                    count - 1
                };
                self.activate_slot((self.active + delta) % count);
            }
            action => return Some(action),
        }
        None
    }

    pub(super) fn layouts_settings(&mut self, settings: &mut UserSettings, direction: Direction) {
        match direction {
            Direction::Save => {
                let menu = &self.top_toolbar.layout_menu;
                let tab = &self.layouts.tabs[self.layouts.tab];
                settings.layout = tab
                    .layout
                    .clone()
                    .filter(|name| menu.layouts.contains(name));
                settings.layout_autosave = menu.autosave;
                settings.layout_favorites = menu.favorites.clone();
                settings.tabs_auto_restore = self.layouts.auto_restore;
                settings.tab_title = self.layouts.title_parts.clone();
                settings.tab_title_hidden = self.layouts.title_hidden.clone();
                // While the tabs are reopened, the settings keep the list they had.
                match &self.layouts.restoring {
                    Some(restoring) => {
                        settings.tabs = restoring.saved.clone();
                        settings.active_tab = restoring.active;
                    }
                    None => {
                        settings.active_tab = Some(tab.id);
                        settings.tabs = self
                            .layouts
                            .tabs
                            .iter()
                            .map(|tab| TabSettings {
                                id: tab.id,
                                name: tab.name.clone(),
                                title: tab.title.clone(),
                                layout: tab.layout.clone(),
                            })
                            .collect();
                    }
                }
            }
            Direction::Load => {
                self.top_toolbar.layout_menu.autosave = settings.layout_autosave;
                self.top_toolbar.layout_menu.favorites = settings.layout_favorites.clone();
                self.layouts.auto_restore = settings.tabs_auto_restore;
                self.layouts.title_parts = settings.tab_title.clone();
                self.layouts.title_hidden = settings.tab_title_hidden.clone();
                // A change by another window leaves the tabs, and their unsaved edits, as they are.
                if self.layouts.restored {
                    return;
                }
                self.layouts.restored = true;
                if self.layouts.auto_restore && !settings.tabs.is_empty() {
                    // The first tab fills the one on show; the others are parked beside it as
                    // the store answers, one per poll.
                    self.layouts.restoring = Some(Restore {
                        saved: settings.tabs.clone(),
                        pending: settings.tabs.iter().cloned().collect(),
                        current: None,
                        filled: false,
                        active: settings.active_tab,
                    });
                    self.restore_next();
                } else if let Some(layout) = &settings.layout {
                    self.top_toolbar.layout_menu.open(layout);
                }
            }
        }
    }

    pub(super) fn layouts_modal(&self) -> bool {
        self.layouts.chooser || self.layouts.creating.is_some()
    }

    /// Whether the tab on show still asks what to open, before it shows a chart.
    pub(super) fn start_page(&self) -> bool {
        self.layouts
            .tabs
            .get(self.layouts.tab)
            .is_some_and(|tab| tab.start)
    }

    /// Draws the page a new tab opens with: a new layout, or one of the saved ones. It covers the
    /// charts, which keep drawing underneath so that their drawings are ready for the next tab.
    pub(super) fn show_start_page(&mut self, ui: &mut egui::Ui) {
        if !self.start_page() {
            return;
        }
        let rect = ui.max_rect();
        let ctx = ui.ctx().clone();
        let page_bg = self.theme.ui.panel_bg;
        let (text, border) = (self.theme.ui.text, self.theme.ui.border);
        let outline = self.theme.ui.border_control;
        let (card, hover_bg) = (self.theme.ui.panel_bg_floating, self.theme.ui.btn_bg_hover);
        let muted = self.theme.ui.text_muted;
        // TradingView's gold star; darker on the light theme so that it stays legible.
        let gold = if self.theme.dark {
            egui::Color32::from_rgb(245, 166, 35)
        } else {
            egui::Color32::from_rgb(196, 124, 0)
        };
        let (radius, gap) = (self.theme.radius.md, self.theme.spacing.md);
        let (heading, title) = (self.theme.text.dialog_title, self.theme.text.title);
        let small = self.theme.text.small;
        let top = self.theme.spacing.xxl;
        // Starred layouts come first, as the layout menu lists them.
        let favorites = self.top_toolbar.layout_menu.favorites.clone();
        let info = self.top_toolbar.layout_menu.info.clone();
        let mut names = self.top_toolbar.layout_menu.layouts.clone();
        names.sort_by_key(|name| !favorites.contains(name));
        let create_hint = l10n::text(&ctx, "Create new layout");
        let mut create = false;
        let mut open = None;
        let mut open_tab = None;
        let mut copy = None;
        let mut remove = None;
        let mut star = None;
        egui::Area::new(egui::Id::unique("tv_start_page"))
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                ui.set_width(rect.width());
                ui.set_height(rect.height());
                // The page takes the clicks the chart underneath would get.
                ui.interact(rect, ui.scope_id().with("start_page"), egui::Sense::click());
                ui.painter().rect_filled(rect, 0.0, page_bg);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let columns = (((ui.available_width() + gap) / (260.0 + gap)).floor()
                            as usize)
                            .max(1);
                        let tile = ((ui.available_width() - (columns - 1) as f32 * gap)
                            / columns as f32)
                            .min(260.0);
                        let size = egui::vec2(tile, 84.0);
                        // TradingView centres the heading and the cards as one block.
                        let content = columns as f32 * tile + (columns - 1) as f32 * gap;
                        let left = ((ui.available_width() - content) / 2.0).max(0.0);
                        ui.add_space(top);
                        ui.horizontal(|ui| {
                            ui.add_space(left);
                            ui.label(
                                egui::RichText::new(l10n::text(&ctx, "Layouts"))
                                    .size(heading)
                                    .color(text),
                            );
                        });
                        ui.add_space(top);
                        let items = std::iter::once(None)
                            .chain(names.iter().map(Some))
                            .collect::<Vec<_>>();
                        for row in items.chunks(columns) {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                ui.add_space(left);
                                for name in row {
                                    let Some(name) = name else {
                                        let (rect, response) =
                                            ui.allocate_exact_size(size, egui::Sense::click());
                                        let painter = ui.painter().clone();
                                        painter.rect(
                                            rect,
                                            radius,
                                            card,
                                            egui::Stroke::new(1.0, outline),
                                            egui::StrokeKind::Inside,
                                        );
                                        painter.text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            format!("+  {create_hint}"),
                                            egui::FontId::proportional(title),
                                            text,
                                        );
                                        if response.clicked() {
                                            create = true;
                                        }
                                        continue;
                                    };
                                    let name = *name;
                                    let (rect, response) =
                                        ui.allocate_exact_size(size, egui::Sense::click());
                                    let starred = favorites.contains(name);
                                    let painter = ui.painter().clone();
                                    painter.rect(
                                        rect,
                                        radius,
                                        card,
                                        egui::Stroke::new(1.0, outline),
                                        egui::StrokeKind::Inside,
                                    );
                                    // The star sits above the name, as in TradingView.
                                    let star_rect = egui::Rect::from_min_size(
                                        rect.left_top() + egui::vec2(12.0, 10.0),
                                        egui::vec2(22.0, 22.0),
                                    );
                                    let star_response = ui.interact(
                                        star_rect,
                                        ui.scope_id().with(("start_star", name)),
                                        egui::Sense::click(),
                                    );
                                    let (star_icon, star_color) = if starred {
                                        (egui_charts::icons::icons::STAR_FILLED, gold)
                                    } else {
                                        (egui_charts::icons::icons::STAR_EMPTY, muted)
                                    };
                                    star_icon
                                        .as_image_tinted(egui::Vec2::splat(16.0), star_color)
                                        .paint_at(ui, star_rect.shrink(3.0));
                                    // No bold face is installed, so the name is painted twice a
                                    // fraction of a point apart to read as bold.
                                    let name_pos = rect.left_top() + egui::vec2(16.0, 36.0);
                                    let name_font = egui::FontId::proportional(title);
                                    painter.text(
                                        name_pos,
                                        egui::Align2::LEFT_TOP,
                                        name,
                                        name_font.clone(),
                                        text,
                                    );
                                    painter.text(
                                        name_pos + egui::vec2(0.4, 0.0),
                                        egui::Align2::LEFT_TOP,
                                        name,
                                        name_font,
                                        text,
                                    );
                                    // What the layout shows and when it was last saved.
                                    let shows = info.get(name).map(|info| {
                                        let interval = info
                                            .interval
                                            .map(|interval| interval.as_str().into_owned())
                                            .unwrap_or_default();
                                        let shows = [info.symbol.as_str(), interval.as_str()]
                                            .into_iter()
                                            .filter(|part| !part.is_empty())
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        let saved = info
                                            .saved_at
                                            .and_then(|seconds| {
                                                chrono::DateTime::from_timestamp(seconds, 0)
                                            })
                                            .map(|saved| {
                                                saved
                                                    .with_timezone(&chrono::Local)
                                                    .format("%d %b '%y")
                                                    .to_string()
                                            });
                                        [shows, saved.unwrap_or_default()]
                                            .into_iter()
                                            .filter(|part| !part.is_empty())
                                            .collect::<Vec<_>>()
                                            .join(" • ")
                                    });
                                    if let Some(shows) = shows.filter(|shows| !shows.is_empty()) {
                                        painter.text(
                                            rect.left_top() + egui::vec2(16.0, 56.0),
                                            egui::Align2::LEFT_TOP,
                                            shows,
                                            egui::FontId::proportional(small),
                                            muted,
                                        );
                                    }
                                    // The menu button: three dots down the right edge.
                                    let menu_rect = egui::Rect::from_center_size(
                                        rect.right_center() - egui::vec2(18.0, 0.0),
                                        egui::vec2(28.0, 28.0),
                                    );
                                    let menu_response = ui.interact(
                                        menu_rect,
                                        ui.scope_id().with(("start_menu", name)),
                                        egui::Sense::click(),
                                    );
                                    if menu_response.hovered() {
                                        painter.rect_filled(menu_rect, radius, hover_bg);
                                    }
                                    for offset in [-5.0, 0.0, 5.0] {
                                        painter.circle_filled(
                                            menu_rect.center() + egui::vec2(0.0, offset),
                                            1.4,
                                            muted,
                                        );
                                    }
                                    egui::Popup::menu(&menu_response).show(|ui| {
                                        if ui.button(l10n::text(&ctx, "Open in new tab")).clicked()
                                        {
                                            open_tab = Some(name.clone());
                                            ui.close();
                                        }
                                        if ui.button(l10n::text(&ctx, "Make a copy…")).clicked() {
                                            copy = Some(name.clone());
                                            ui.close();
                                        }
                                        if ui.button(l10n::text(&ctx, "Remove layout")).clicked() {
                                            remove = Some(name.clone());
                                            ui.close();
                                        }
                                    });
                                    if star_response.clicked() {
                                        star = Some(name.clone());
                                    } else if !menu_response.hovered() && response.clicked() {
                                        open = Some(name.clone());
                                    }
                                }
                            });
                            ui.add_space(gap);
                        }
                        if names.is_empty() {
                            ui.label(
                                egui::RichText::new(l10n::text(&ctx, "No saved layouts"))
                                    .size(title)
                                    .color(border),
                            );
                        }
                    });
            });
        if create {
            // The dialog names a new, clean layout before the tab leaves the start page.
            self.layouts.creating = Some(l10n::text(&ctx, "New layout"));
        }
        if let Some(name) = open {
            self.top_toolbar.layout_menu.open(&name);
        }
        if let Some(name) = open_tab {
            // A new tab shows the saved layout instead of the chart it copies to start with.
            if self.new_layout(None) {
                self.top_toolbar.layout_menu.open(&name);
            }
        }
        if let Some(name) = copy {
            self.top_toolbar.layout_menu.duplicate(&name);
        }
        if let Some(name) = remove {
            self.perform_layout_action(LayoutAction::Delete(name));
        }
        if let Some(name) = star {
            let favorites = &mut self.top_toolbar.layout_menu.favorites;
            match favorites.iter().position(|favorite| *favorite == name) {
                Some(at) => {
                    favorites.remove(at);
                }
                None => favorites.push(name),
            }
        }
    }

    fn perform_layout_action(&mut self, action: LayoutAction) -> Option<Action> {
        match action {
            LayoutAction::SelectGrid(mode) => {
                self.layouts.grid_edits = self.layouts.grid_edits.wrapping_add(1);
                self.set_grid(mode);
            }
            LayoutAction::DownloadData => {
                return self.perform(Action::Export(ExportResponse::Export {
                    config: ExportConfig {
                        format: ExportFormat::Csv,
                        ..ExportConfig::default()
                    },
                    save_path: None,
                }));
            }
            LayoutAction::DownloadImage => {
                return self.perform(Action::Export(ExportResponse::Export {
                    config: ExportConfig::default(),
                    save_path: None,
                }));
            }
            LayoutAction::OpenLayout => self.layouts.chooser = true,
            LayoutAction::CreateNew => {
                self.new_layout(None);
            }
            LayoutAction::SaveAs(name) => match self.capture_layout() {
                Ok(layout) => {
                    if self.new_layout(Some(name)) {
                        let name = self.layouts.tabs[self.layouts.tab].name.clone();
                        self.top_toolbar.layout_menu.save(&name, &layout);
                    }
                }
                Err(error) => self.toasts.error(error),
            },
            LayoutAction::Open(name) => {
                match self
                    .layouts
                    .tabs
                    .iter()
                    .position(|tab| tab.layout.as_deref() == Some(name.as_str()))
                {
                    Some(index) if index != self.layouts.tab => self.select_layout(index),
                    // The current tab opens the layout again as it was saved.
                    _ => self.top_toolbar.layout_menu.open(&name),
                }
            }
            LayoutAction::Delete(name) => {
                self.top_toolbar.layout_menu.delete(&name);
                for tab in &mut self.layouts.tabs {
                    if tab.layout.as_deref() == Some(name.as_str()) {
                        tab.layout = None;
                    }
                }
            }
            LayoutAction::Rename(_, new)
                if !new.trim().is_empty()
                    && !self
                        .layouts
                        .tabs
                        .iter()
                        .any(|tab| tab.layout.as_deref() == Some(new.as_str())) =>
            {
                match self.capture_layout() {
                    Ok(layout) => {
                        self.top_toolbar.layout_menu.rename(&new, &layout);
                        self.layouts.tabs[self.layouts.tab].layout = Some(new.clone());
                    }
                    Err(error) => self.toasts.error(error),
                }
            }
            LayoutAction::Save => self.save_layout(),
            _ => {}
        }
        None
    }

    /// Gives the layout the slots `mode` shows, copying the active chart into new ones. Slots
    /// beyond a smaller mode are kept, hidden, until a larger one shows them again.
    fn set_grid(&mut self, mode: ChartLayoutMode) {
        let count = if mode == ChartLayoutMode::Tabs {
            self.charts.len().max(2)
        } else {
            mode.chart_count()
        };
        while self.charts.len() < count {
            let Some(mut copy) = self.copy_slot(self.active) else {
                return;
            };
            // Drawings reach new slots through synchronization, which records their links.
            copy.drawings_mut().drawings.clear();
            self.charts.push(copy);
            self.wire_slot(self.charts.len() - 1);
        }
        self.layout.mode = mode;
        if self.active >= count {
            self.activate_slot(0);
        }
        self.top_toolbar.chart_grid_selector.set_layout(mode);
    }

    /// A chart that shows what the chart in slot `from` shows: its snapshot, and the properties
    /// the snapshot leaves out. The caller wires it into its slot.
    fn copy_slot(&mut self, from: usize) -> Option<Chart> {
        let source = &self.charts[from];
        let mut chart = ChartBuilder::new()
            .with_feed(self.feed.clone())
            .with_symbol(source.symbol())
            .with_timeframe(source.timeframe().unwrap_or_default())
            .with_theme(
                source
                    .theme_override()
                    .cloned()
                    .unwrap_or_else(|| self.theme.clone()),
            )
            .with_drawing_registry(source.drawings().registry.clone())
            .with_visible_candles(0)
            .build();
        chart.set_price_targets_visible(source.price_targets_visible());
        chart.set_event_visibility(source.event_visibility().clone());
        let copied = source
            .snapshot_with(self.studies.indicators.factory())
            .map_err(|error| error.to_string())
            .and_then(|snapshot| {
                chart
                    .restore_with(&snapshot, self.studies.indicators.factory())
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = copied {
            self.toasts
                .error(format!("Could not copy the chart: {error}"));
            return None;
        }
        chart.set_data(SeriesId::MAIN, source.bars(SeriesId::MAIN).to_vec());
        chart.set_conversion(source.conversion().map(str::to_owned));
        chart.set_study_limit(source.study_limit());
        Some(chart)
    }

    fn layout_stop_replay(&mut self) {
        if self.replay_active {
            self.perform(Action::Replay(ReplayControlAction::Stop));
        }
    }

    fn show_layout_tabs(&mut self, ui: &mut egui::Ui) {
        let chart = self.chart();
        let (up, down) = (chart.config().bullish_color, chart.config().bearish_color);
        let current = self.layouts.tab;
        let tab = &mut self.layouts.tabs[current];
        refresh_tab_market(&self.charts, tab);
        let ctx = ui.ctx().clone();
        let (close_hint, new_hint, rename_hint) = (
            l10n::text(&ctx, "Close layout"),
            l10n::text(&ctx, "New layout"),
            l10n::text(&ctx, "Customize tab title…"),
        );
        let mut select = None;
        let mut close = None;
        let mut customize = false;
        let mut new = false;
        let strip = self.theme.ui.border;
        let active = self.theme.ui.panel_bg;
        let muted = self.theme.ui.text_muted;
        let body = self.theme.text.body;
        let parts = self.layouts.title_parts.clone();
        // TradingView's window tabs: always on screen, the layout on show filled as the panel
        // below it, the others on the strip colour, each with its close cross.
        egui::Panel::top("layout_tabs")
            .exact_size(TAB_STRIP_HEIGHT)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(strip))
            .show(ui, |ui| {
                // A tab is a click target, not selectable text: hovering it keeps the arrow.
                ui.style_mut().interaction.selectable_labels = false;
                let count = self.layouts.tabs.len();
                let plus = TAB_STRIP_HEIGHT + 6.0;
                let close_width = TAB_STRIP_HEIGHT * 0.75;
                // TradingView's tabs: 200 wide until the strip runs out of room, then narrower.
                let width = ((ui.available_width() - plus - count.saturating_sub(1) as f32)
                    / count.max(1) as f32)
                    .clamp(close_width + 16.0, 200.0);
                let separator = self.theme.ui.border_control;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (index, tab) in self.layouts.tabs.iter().enumerate() {
                        if index > 0 {
                            let (line, _) = ui.allocate_exact_size(
                                egui::vec2(1.0, TAB_STRIP_HEIGHT),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_center_size(
                                    line.center(),
                                    egui::vec2(1.0, TAB_STRIP_HEIGHT - 8.0),
                                ),
                                0.0,
                                separator,
                            );
                        }
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(width, TAB_STRIP_HEIGHT),
                            egui::Sense::hover(),
                        );
                        let shown = index == current;
                        // The close cross keeps its own clicks; the rest of the tab selects it
                        // and opens its context menu.
                        let hit = egui::Rect::from_min_max(
                            rect.min,
                            egui::pos2(rect.max.x - close_width, rect.max.y),
                        );
                        let click = ui.interact(
                            hit,
                            ui.scope_id().with(("layout_tab", tab.id)),
                            egui::Sense::click(),
                        );
                        let fill = if shown {
                            active
                        } else if click.hovered() {
                            self.theme.ui.btn_bg_hover
                        } else {
                            strip
                        };
                        ui.painter().rect_filled(rect, 0.0, fill);
                        let title = egui::Rect::from_min_max(
                            egui::pos2(rect.min.x + 8.0, rect.min.y),
                            egui::pos2(hit.max.x - 4.0, rect.max.y),
                        );
                        let label = if tab.start {
                            let text = l10n::text(ui.ctx(), "New Tab");
                            let color = if shown {
                                self.theme.ui.text
                            } else {
                                self.theme.ui.text_secondary
                            };
                            egui::WidgetText::from(
                                egui::RichText::new(text).size(body).color(color),
                            )
                        } else if let Some(title) = &tab.title {
                            let color = if shown {
                                self.theme.ui.text
                            } else {
                                self.theme.ui.text_secondary
                            };
                            egui::WidgetText::from(
                                egui::RichText::new(title).size(body).color(color),
                            )
                        } else {
                            // One job for the parts, so the title truncates as a whole.
                            let mut job = egui::text::LayoutJob::default();
                            let format = |color| egui::TextFormat {
                                font_id: egui::FontId::proportional(body),
                                color,
                                ..Default::default()
                            };
                            let color = if tab.change >= 0.0 { up } else { down };
                            for part in &parts {
                                match part {
                                    TabTitlePart::Ticker => {
                                        job.append(&tab.ticker, 0.0, format(self.theme.ui.text));
                                    }
                                    TabTitlePart::ChangeIcon => {
                                        let icon = if tab.change >= 0.0 { "↑" } else { "↓" };
                                        job.append(icon, 0.0, format(color));
                                    }
                                    TabTitlePart::LastPrice => {
                                        let price = format!("{:.2}", tab.last);
                                        job.append(&price, 0.0, format(self.theme.ui.text));
                                    }
                                    TabTitlePart::ChangePercent => {
                                        let percent = format!("{:+.2}%", tab.change);
                                        job.append(&percent, 0.0, format(color));
                                    }
                                    TabTitlePart::LayoutName => {
                                        if let Some(layout) = &tab.layout {
                                            job.append(
                                                layout,
                                                0.0,
                                                format(self.theme.ui.text_muted),
                                            );
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            egui::WidgetText::from(job)
                        };
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(title)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                            |ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                ui.set_clip_rect(title.intersect(ui.clip_rect()));
                                ui.add(egui::Label::new(label).truncate());
                            },
                        );
                        let cross = egui::Rect::from_min_max(
                            egui::pos2(rect.max.x - close_width, rect.min.y),
                            rect.max,
                        );
                        let close_btn = ui
                            .put(
                                cross,
                                egui::Button::new(egui::RichText::new("×").color(muted))
                                    .frame(false),
                            )
                            .on_hover_text(&close_hint);
                        // The title and the cross are placed, not laid out: the row keeps the
                        // tab's width whatever their size.
                        ui.advance_cursor_after_rect(rect);
                        click.clone().context_menu(|ui| {
                            if ui.button(rename_hint.as_str()).clicked() {
                                customize = true;
                                ui.close();
                            }
                            if ui.button(close_hint.as_str()).clicked() {
                                close = Some(index);
                                ui.close();
                            }
                        });
                        if close_btn.clicked() {
                            close = Some(index);
                        } else if click.clicked() {
                            select = Some(index);
                        }
                    }
                    ui.add_space(6.0);
                    new = ui
                        .add(
                            egui::Button::new("+")
                                .frame(false)
                                .min_size(egui::vec2(TAB_STRIP_HEIGHT, TAB_STRIP_HEIGHT)),
                        )
                        .on_hover_text(&new_hint)
                        .clicked();
                });
            });
        if customize {
            self.preferences_open_tabs();
        }
        if let Some(index) = select {
            self.select_layout(index);
        }
        if let Some(index) = close {
            self.close_layout(index);
        }
        if new {
            self.new_layout(None);
        }
    }

    /// Shows the layout in tab `index`, parking the one on show in its tab.
    fn select_layout(&mut self, index: usize) {
        if index == self.layouts.tab {
            return;
        }
        // A tab is kept as it is left, so that it comes back after a restart.
        refresh_tab_market(&self.charts, &mut self.layouts.tabs[self.layouts.tab]);
        self.save_tab();
        let Some(next) = self
            .layouts
            .tabs
            .get_mut(index)
            .and_then(|tab| tab.workspace.take())
        else {
            return;
        };
        self.layout_stop_replay();
        self.layouts.tabs[self.layouts.tab].workspace = Some(Workspace {
            charts: std::mem::replace(&mut self.charts, next.charts),
            layout: std::mem::replace(&mut self.layout, next.layout),
            active: std::mem::replace(&mut self.active, next.active),
        });
        self.layouts.tab = index;
        self.layout.set_active(self.active);
        refresh_tab_market(&self.charts, &mut self.layouts.tabs[index]);
        self.refresh_layout_menu();
        // The theme may have changed and other tabs may have edited the same symbol since this
        // layout was last on show.
        for slot in 0..self.charts.len() {
            self.wire_slot(slot);
        }
        let grid = &mut self.top_toolbar.chart_grid_selector;
        grid.set_layout(self.layout.mode);
        grid.sync = self.layout.sync.clone();
    }

    /// Opens a tab with a copy of the active chart, or with a copy of the whole layout under
    /// `name`.
    fn new_layout(&mut self, name: Option<String>) -> bool {
        let duplicate = name.is_some();
        let (charts, layout, active) = if duplicate {
            let Some(charts) = (0..self.charts.len())
                .map(|slot| self.copy_slot(slot))
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            (charts, self.layout.clone(), self.active)
        } else {
            let Some(chart) = self.copy_slot(self.active) else {
                return false;
            };
            (vec![chart], ChartLayout::default(), 0)
        };
        let mut name = name.unwrap_or_else(|| "New Tab".into());
        let base = name.clone();
        let mut suffix = 2;
        while self.layouts.tabs.iter().any(|tab| tab.name == name) {
            name = format!("{base} {suffix}");
            suffix += 1;
        }
        let id = self
            .layouts
            .tabs
            .iter()
            .map(|tab| tab.id)
            .max()
            .unwrap_or(0)
            + 1;
        let mut tab = Tab {
            id,
            name: name.clone(),
            title: None,
            layout: duplicate.then_some(name),
            // A new tab asks what to show; a copy already shows the layout it duplicates.
            start: !duplicate,
            saved: None,
            workspace: None,
            ticker: String::new(),
            last: 0.0,
            change: 0.0,
        };
        refresh_tab_market(&charts, &mut tab);
        tab.workspace = Some(Workspace {
            charts,
            layout,
            active,
        });
        self.layouts.tabs.push(tab);
        self.select_layout(self.layouts.tabs.len() - 1);
        // The tab keeps its own document, so that it comes back after a restart.
        self.save_tab();
        true
    }

    fn close_layout(&mut self, index: usize) {
        if self.layouts.tabs.len() == 1 {
            return;
        }
        let id = self.layouts.tabs[index].id;
        if index == self.layouts.tab {
            self.select_layout(if index == 0 { 1 } else { index - 1 });
        }
        self.layouts.tabs.remove(index);
        if self.layouts.tab > index {
            self.layouts.tab -= 1;
        }
        // A closed tab leaves no document behind.
        if self.services.storage.is_some() {
            self.layouts.store.delete(&tab_document(id));
        }
    }

    /// The saved layouts, for the Open layout shortcut.
    fn show_open_layout(&mut self, ctx: &egui::Context) {
        if !self.layouts.chooser {
            return;
        }
        let mut open = true;
        let mut chosen = None;
        let none = l10n::text(ctx, "No saved layouts");
        Modal::new("open_layout", l10n::text(ctx, "Open layout"))
            .width(320.0)
            .height(400.0)
            .show(ctx, &mut open, |ui| {
                if self.top_toolbar.layout_menu.layouts.is_empty() {
                    ui.label(none);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for name in &self.top_toolbar.layout_menu.layouts {
                        if ui
                            .add_sized(
                                egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
                                egui::Button::new(name).frame(false).truncate(),
                            )
                            .on_hover_text(name)
                            .clicked()
                        {
                            chosen = Some(name.clone());
                        }
                    }
                });
            });
        self.layouts.chooser = open && chosen.is_none();
        if let Some(name) = chosen {
            self.perform_layout_action(LayoutAction::Open(name));
        }
    }

    /// The Create new layout dialog of the start page.
    fn show_create_layout(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.layouts.creating.take() else {
            return;
        };
        let theme = self.theme.clone();
        let taken = self
            .top_toolbar
            .layout_menu
            .layouts
            .iter()
            .any(|layout| layout.trim() == name.trim());
        let mut open = true;
        let mut create = false;
        let mut cancelled = false;
        Modal::new("create_layout", l10n::text(ctx, "Create new layout"))
            .width(360.0)
            .show(ctx, &mut open, |ui| {
                ui.label(l10n::text(ctx, "Layout name"));
                ui.add(
                    egui::TextEdit::singleline(&mut name)
                        .id(egui::Id::unique("create_layout_name"))
                        .desired_width(ui.available_width()),
                );
                if taken {
                    ui.label(
                        egui::RichText::new(l10n::text(
                            ctx,
                            "A layout with this name already exists",
                        ))
                        .size(theme.text.small)
                        .color(theme.ui.error),
                    );
                }
                let (ok, cancel) = DialogFooter::new("Create")
                    .secondary("Cancel")
                    .accent_primary()
                    .primary_enabled(!name.trim().is_empty() && !taken)
                    .show(ui);
                create = ok;
                cancelled = cancel;
            });
        if create {
            self.create_layout(name.trim().to_owned());
            return;
        }
        if open && !cancelled {
            self.layouts.creating = Some(name);
        }
    }

    /// Gives the tab on show a clean layout named `name`, with no studies and default settings.
    fn create_layout(&mut self, name: String) {
        let source = self.chart();
        let symbol = source.symbol().to_owned();
        let timeframe = source.timeframe().unwrap_or_default();
        let theme = source
            .theme_override()
            .cloned()
            .unwrap_or_else(|| self.theme.clone());
        let registry = source.drawings().registry.clone();
        let chart = ChartBuilder::new()
            .with_feed(self.feed.clone())
            .with_symbol(&symbol)
            .with_timeframe(timeframe)
            .with_theme(theme)
            .with_drawing_registry(registry)
            .build();
        self.layout_stop_replay();
        self.charts = vec![chart];
        self.layout = ChartLayout::default();
        self.active = 0;
        self.layout.set_active(0);
        self.top_toolbar
            .chart_grid_selector
            .set_layout(ChartLayoutMode::Single);
        let tab = &mut self.layouts.tabs[self.layouts.tab];
        tab.name = name;
        tab.layout = None;
        tab.start = false;
        tab.title = None;
        refresh_tab_market(&self.charts, tab);
        // Saved before the chart takes a drawing store, whose load the snapshot would wait for.
        self.save_layout();
        self.wire_slot(0);
    }

    fn refresh_layout_menu(&mut self) {
        let tab = &self.layouts.tabs[self.layouts.tab];
        self.top_toolbar.layout_menu.curr_layout =
            Some(tab.layout.clone().unwrap_or_else(|| tab.name.clone()));
    }

    /// The layout on show: its arrangement, the charts it shows, in slot order, and the chart
    /// sizes of the arrangement when its borders were moved.
    fn capture_layout(&self) -> Result<SavedLayout, String> {
        let mode = self.layout.mode;
        let count = if mode == ChartLayoutMode::Tabs {
            self.charts.len()
        } else {
            mode.chart_count().min(self.charts.len())
        };
        let charts = self.charts[..count]
            .iter()
            .map(|chart| chart.snapshot_with(self.studies.indicators.factory()))
            .collect::<Result<_, _>>()
            .map_err(|error| format!("Could not save the layout: {error}"))?;
        let sizing = self
            .layout
            .saved_sizes()
            .into_iter()
            .find(|sizes| sizes.mode == mode);
        let mut layout = SavedLayout::new(mode, charts);
        layout.sizing = sizing;
        layout.saved_at = Some(chrono::Utc::now().timestamp());
        Ok(layout)
    }

    /// Saves the layout on show under the layout of its tab, or under the tab's name while it
    /// shows none, which the tab then shows.
    pub(super) fn save_layout(&mut self) {
        let tab = &self.layouts.tabs[self.layouts.tab];
        let name = tab.layout.clone().unwrap_or_else(|| tab.name.clone());
        match self.capture_layout() {
            Ok(layout) => {
                self.top_toolbar.layout_menu.save(&name, &layout);
                self.layouts.tabs[self.layouts.tab].layout = Some(name);
            }
            Err(error) => self.toasts.error(error),
        }
    }

    /// Stores the tab on show as its own document, which brings it back after a restart with the
    /// edits made after its layout was saved.
    fn save_tab(&mut self) {
        if self.services.storage.is_none() {
            return;
        }
        let id = self.layouts.tabs[self.layouts.tab].id;
        let edits = self.layout_edits();
        match self.capture_layout() {
            Ok(layout) => {
                self.layouts.store.save(&tab_document(id), &layout);
                self.layouts.tabs[self.layouts.tab].saved = Some(edits);
            }
            Err(error) => self.toasts.error(error),
        }
    }

    /// Stores the tab on show at exit, ignoring a chart whose drawings are still loading so that
    /// closing does not report it, and leaving a write already on its way or one that has the
    /// tab's state already.
    pub(super) fn flush_tab(&mut self) {
        if self.services.storage.is_none() || !self.layouts.store.is_idle() {
            return;
        }
        let edits = self.layout_edits();
        let id = self.layouts.tabs[self.layouts.tab].id;
        if self.layouts.tabs[self.layouts.tab].saved == Some(edits) {
            return;
        }
        if let Ok(layout) = self.capture_layout() {
            self.layouts.store.save(&tab_document(id), &layout);
            self.layouts.tabs[self.layouts.tab].saved = Some(edits);
        }
    }

    /// Whether storage has every tab document answered.
    pub(super) fn tabs_idle(&self) -> bool {
        self.layouts.store.is_idle()
    }

    /// Shows a stored layout in the current tab, which mentions it without taking its name.
    fn restore_layout(&mut self, name: String, layout: SavedLayout) {
        // A new tab that is still asking what to open fills with the layout the user picks,
        // even when another tab already shows it.
        if !self.start_page()
            && let Some(index) = self
                .layouts
                .tabs
                .iter()
                .position(|tab| tab.layout.as_deref() == Some(name.as_str()))
            && index != self.layouts.tab
        {
            self.select_layout(index);
            return;
        }
        let Some(charts) = self.charts_of(&layout) else {
            return;
        };
        self.install_charts(charts, &layout);
        let tab = &mut self.layouts.tabs[self.layouts.tab];
        tab.layout = Some(name);
        tab.start = false;
        refresh_tab_market(&self.charts, tab);
    }

    /// Shows `charts` in the slots their stored layout arranges, at its sizes.
    fn install_charts(&mut self, charts: Vec<Chart>, layout: &SavedLayout) {
        self.layout_stop_replay();
        // The drawing tool defaults come from the settings, not from a layout.
        let defaults = self
            .charts
            .first()
            .map(|chart| chart.drawings().tool_defaults.clone())
            .unwrap_or_default();
        self.charts = charts;
        for chart in &mut self.charts {
            chart.drawings_mut().tool_defaults = defaults.clone();
        }
        // A copy drops a divider drag in progress, which would overwrite the restored sizes.
        self.layout = self.layout.clone();
        self.layout.reset_sizes(true);
        let sized = match &layout.sizing {
            Some(sizing) if sizing.mode != layout.mode => Err("sizes of another arrangement"),
            Some(sizing) => self.layout.set_sizes(std::slice::from_ref(sizing), true),
            None => Ok(()),
        };
        if let Err(error) = sized {
            self.toasts
                .error(format!("Could not open chart sizes: {error}"));
        }
        self.layout.mode = layout.mode;
        if self.active >= self.charts.len() {
            self.active = 0;
        }
        self.layout.set_active(self.active);
        self.top_toolbar.chart_grid_selector.set_layout(layout.mode);
        for slot in 0..self.charts.len() {
            self.wire_slot(slot);
        }
    }

    /// The built-in theme the saved palette came from, or the app theme for a document that
    /// saved none: a layout keeps the colours it was saved with.
    fn layout_theme(&self, palette: Option<&ChartConfig>) -> Theme {
        [Theme::light(), Theme::dark()]
            .into_iter()
            .find(|theme| palette == Some(&ChartConfig::default().with_theme(theme)))
            .unwrap_or_else(|| self.theme.clone())
    }

    /// The charts a stored layout shows, in this terminal's feed and drawings.
    fn charts_of(&mut self, layout: &SavedLayout) -> Option<Vec<Chart>> {
        if layout.charts.is_empty()
            || layout.charts.len() > ChartLayoutMode::Tabs.chart_count()
            || (layout.mode != ChartLayoutMode::Tabs
                && layout.charts.len() != layout.mode.chart_count())
        {
            self.toasts
                .error("Could not open layout: invalid chart count");
            return None;
        }
        let source = self.chart();
        let registry = source.drawings().registry.clone();
        let study_limit = source.study_limit();
        let targets = source.price_targets_visible();
        let events = source.event_visibility().clone();
        let conversion = source.conversion().map(str::to_owned);
        let mut charts = Vec::with_capacity(layout.charts.len());
        let mut skipped_drawings = 0;
        for snapshot in &layout.charts {
            let mut chart = ChartBuilder::new()
                .with_feed(self.feed.clone())
                .with_symbol(&snapshot.symbol)
                .with_timeframe(snapshot.interval.unwrap_or_default())
                .with_theme(self.layout_theme(snapshot.palette.as_ref()))
                .with_drawing_registry(registry.clone())
                .with_visible_candles(0)
                .build();
            match chart.restore_with(snapshot, self.studies.indicators.factory()) {
                Err(error) => {
                    self.toasts.error(format!("Could not open layout: {error}"));
                    return None;
                }
                Ok(skipped) => skipped_drawings += skipped.len(),
            }
            chart.set_study_limit(study_limit);
            chart.set_price_targets_visible(targets);
            chart.set_event_visibility(events.clone());
            chart.set_conversion(conversion.clone());
            charts.push(chart);
        }
        if skipped_drawings > 0 {
            self.toasts
                .warning(format!("Skipped {skipped_drawings} unreadable drawings"));
        }
        Some(charts)
    }

    /// Reopens one tab of the settings' list: the first fills the tab on show, the others wait
    /// beside it, and the last one read brings the saved tab back to the front.
    fn restore_tab(&mut self, layout: SavedLayout) {
        let Some(tab) = self
            .layouts
            .restoring
            .as_mut()
            .and_then(|restoring| restoring.current.take())
        else {
            return;
        };
        let Some(charts) = self.charts_of(&layout) else {
            self.restore_next();
            return;
        };
        if self
            .layouts
            .restoring
            .as_ref()
            .is_some_and(|restoring| restoring.filled)
        {
            self.park_charts(charts, tab, &layout);
        } else {
            self.install_charts(charts, &layout);
            let current = &mut self.layouts.tabs[self.layouts.tab];
            current.id = tab.id;
            current.name = tab.name;
            current.title = tab.title;
            current.layout = tab.layout;
            refresh_tab_market(&self.charts, current);
            if let Some(restoring) = &mut self.layouts.restoring {
                restoring.filled = true;
            }
        }
        self.restore_next();
    }

    /// Reads the tabs the settings had, one at a time, and shows the one saved last once they
    /// are all back. A tab whose document the store cannot read leaves the rest to come back.
    fn restore_poll(&mut self, ctx: &egui::Context) {
        let opened = self.layouts.store.poll(ctx);
        if let Some(error) = self.layouts.store.take_error() {
            self.toasts.error(error);
        }
        if let Some((_, layout)) = opened {
            self.restore_tab(layout);
        } else if self
            .layouts
            .restoring
            .as_ref()
            .is_some_and(|restoring| restoring.current.is_some())
            && self.layouts.store.is_idle()
        {
            if let Some(restoring) = &mut self.layouts.restoring {
                restoring.current = None;
            }
            self.restore_next();
        }
    }

    /// Asks for the next tab document of the restart.
    fn restore_next(&mut self) {
        let next = self.layouts.restoring.as_mut().and_then(|restoring| {
            let next = restoring.pending.pop_front();
            restoring.current = next.clone();
            next
        });
        if let Some(tab) = next {
            self.layouts.store.open(&tab_document(tab.id));
            return;
        }
        let Some(restoring) = self.layouts.restoring.take() else {
            return;
        };
        if let Some(active) = restoring.active
            && let Some(index) = self.layouts.tabs.iter().position(|tab| tab.id == active)
        {
            self.select_layout(index);
        }
    }

    /// Keeps the charts of a stored tab beside the one on show, without showing it.
    fn park_charts(&mut self, charts: Vec<Chart>, tab: TabSettings, layout: &SavedLayout) {
        let mut parked = ChartLayout::default();
        parked.mode = layout.mode;
        let sized = match &layout.sizing {
            Some(sizing) if sizing.mode != layout.mode => Err("sizes of another arrangement"),
            Some(sizing) => parked.set_sizes(std::slice::from_ref(sizing), true),
            None => Ok(()),
        };
        if let Err(error) = sized {
            self.toasts
                .error(format!("Could not open chart sizes: {error}"));
        }
        let mut tab = Tab {
            id: tab.id,
            name: tab.name,
            title: tab.title,
            layout: tab.layout,
            start: false,
            saved: None,
            workspace: None,
            ticker: String::new(),
            last: 0.0,
            change: 0.0,
        };
        refresh_tab_market(&charts, &mut tab);
        tab.workspace = Some(Workspace {
            charts,
            layout: parked,
            active: 0,
        });
        self.layouts.tabs.push(tab);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MemoryStorage, SIZE, TestFeed, click, frame, key};
    use egui::{Key, Modifiers};
    use egui_charts::config::CrosshairStyle;
    use egui_charts::drawings::{ChartPoint, Drawing, DrawingToolType, ToolAvailability};
    use egui_charts::model::ChartType;
    use egui_charts::services::StoreKind;
    use egui_charts::studies::{CustomIndicator, IndicatorFactory, InputValue, RSI, SMA};
    use egui_charts::widget::{ChartLayoutSizing, PaneId};
    use std::sync::Arc;

    fn new_terminal(storage: &Arc<MemoryStorage>) -> Terminal {
        Terminal::new(Arc::new(TestFeed))
            .with_symbol("AAPL")
            .with_storage(storage.clone())
    }

    /// Runs frames until the answers of storage to the layout menu and the settings arrive.
    fn settle(terminal: &mut Terminal) {
        for _ in 0..4 {
            frame(terminal, SIZE, vec![]);
        }
    }

    fn layout(terminal: &mut Terminal, action: LayoutAction) {
        assert!(terminal.run(Action::Layout(action)).is_none());
        settle(terminal);
    }

    #[test]
    fn grid_selector_grows_slots_and_tab_cycles_the_visible_ones() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        // The slots a grid shows, and the slots that exist after it: shrinking keeps them.
        for (mode, shown, existing) in [
            (ChartLayoutMode::Three, 3, 3),
            (ChartLayoutMode::Single, 1, 3),
            (ChartLayoutMode::Sixteen, 16, 16),
            (ChartLayoutMode::Tabs, 16, 16),
        ] {
            layout(&mut terminal, LayoutAction::SelectGrid(mode));
            assert_eq!(terminal.layout.mode.chart_count(), shown);
            assert_eq!(terminal.charts.len(), existing);
            assert!(terminal.charts.iter().all(|chart| chart.symbol() == "AAPL"));
        }
        layout(
            &mut terminal,
            LayoutAction::SelectGrid(ChartLayoutMode::Three),
        );
        terminal.layout.sync.sync_time_axis = false;
        terminal.top_toolbar.chart_grid_selector.sync = terminal.layout.sync.clone();
        terminal.activate_slot(0);
        // The canvas takes the keyboard focus in the frame after it is activated.
        frame(&mut terminal, SIZE, vec![]);
        let tab = |terminal: &mut Terminal, modifiers| {
            let events = vec![
                egui::Event::ModifiersChanged(modifiers),
                key(Key::Tab, modifiers),
            ];
            frame(terminal, SIZE, events);
            frame(terminal, SIZE, vec![]);
            terminal.active
        };
        let forward: Vec<_> = (0..4)
            .map(|_| tab(&mut terminal, Modifiers::NONE))
            .collect();
        assert_eq!(forward, [1, 2, 0, 1]);
        let back: Vec<_> = (0..2)
            .map(|_| tab(&mut terminal, Modifiers::SHIFT))
            .collect();
        assert_eq!(back, [0, 2]);
        let before = terminal.chart().get_time_scale_state();
        let other = terminal.charts[0].get_time_scale_state();
        frame(
            &mut terminal,
            SIZE,
            vec![key(Key::ArrowRight, Modifiers::NONE)],
        );
        assert_ne!(terminal.chart().get_time_scale_state(), before);
        assert_eq!(terminal.charts[0].get_time_scale_state(), other);
        // Only the slots the grid shows take focus.
        layout(
            &mut terminal,
            LayoutAction::SelectGrid(ChartLayoutMode::Single),
        );
        assert_eq!(terminal.active, 0);
        assert_eq!(tab(&mut terminal, Modifiers::NONE), 0);
    }

    #[test]
    fn layout_tabs_keep_their_own_slots_history_and_theme() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        layout(
            &mut terminal,
            LayoutAction::SelectGrid(ChartLayoutMode::Four),
        );
        // The strip is always there, as TradingView's window tabs, even for one layout.
        let check_strip = |terminal: &mut Terminal, height| {
            let ctx = terminal.ctx();
            for width in [393.0, 599.0, 600.0, 1440.0] {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 900.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let top = ui.available_rect_before_wrap().top();
                        terminal.show_layout_tabs(ui);
                        assert_eq!(ui.available_rect_before_wrap().top() - top, height);
                    },
                )
                .drop_without_applying_deltas();
            }
        };
        check_strip(&mut terminal, TAB_STRIP_HEIGHT);
        for (slot, width) in [(1, 0.6), (3, 0.9)] {
            terminal.activate_slot(slot);
            terminal.chart_mut().config_mut().candle_width = width;
        }
        terminal.activate_slot(0);
        terminal
            .chart_mut()
            .user_action(|chart| chart.config_mut().show_grid = false);
        terminal.top_toolbar.layout_menu.autosave = true;
        layout(&mut terminal, LayoutAction::Save);
        layout(&mut terminal, LayoutAction::CreateNew);
        // The new layout is one slot with a copy of the active chart; the strip lists both.
        let names: Vec<_> = terminal.layouts.tabs.iter().map(|tab| &*tab.name).collect();
        assert_eq!(names, ["Unnamed", "New Tab"]);
        assert!(
            storage.get(StoreKind::Layouts, "Unnamed").is_some(),
            "the layout saved before the new tab stays stored"
        );
        assert!(
            storage.get(StoreKind::Tabs, "tab 1").is_some(),
            "the tab keeps the state it was left in as its own document"
        );
        assert!(
            terminal
                .layouts
                .tabs
                .iter()
                .all(|tab| !tab.ticker.is_empty()),
            "the first layout has a label when the strip appears"
        );
        assert_eq!(terminal.layouts.tab, 1);
        assert_eq!(terminal.layout.mode, ChartLayoutMode::Single);
        assert_eq!(terminal.charts.len(), 1);
        assert!(!terminal.chart().config().show_grid);
        check_strip(&mut terminal, TAB_STRIP_HEIGHT);
        terminal
            .chart_mut()
            .user_action(|chart| chart.config_mut().show_grid = true);
        assert!(
            terminal.chart_mut().undo(),
            "the copied chart has no unfinished action"
        );
        assert!(!terminal.chart().config().show_grid);
        terminal.chart_mut().set_symbol("MSFT");
        // Back in the first layout the slots, their settings and their undo history are as left.
        layout(&mut terminal, LayoutAction::Open("Unnamed".into()));
        assert_eq!(terminal.layout.mode, ChartLayoutMode::Four);
        let widths: Vec<_> = terminal
            .charts
            .iter()
            .map(|chart| chart.config().candle_width)
            .collect();
        assert_eq!(widths[1], 0.6);
        assert_eq!(widths[3], 0.9);
        assert_eq!(terminal.chart().symbol(), "AAPL");
        assert!(terminal.chart_mut().undo(), "the edit stays undoable");
        assert!(terminal.chart().config().show_grid);
        // The app theme changes the application alone; the charts keep their own colours.
        terminal.select_layout(1);
        assert_eq!(terminal.chart().symbol(), "MSFT");
        let background = terminal.chart().config().background_color;
        assert!(
            terminal
                .run(Action::TopToolbar(TopToolbarAction::ThemeToggle))
                .is_none()
        );
        assert_eq!(
            terminal.chart().config().background_color,
            background,
            "the app theme leaves the chart settings' colours"
        );
        // The first close button belongs to the other layout: the one on show stays.
        let close = frame(&mut terminal, SIZE, vec![])
            .text_at("×")
            .expect("close button");
        click(&mut terminal, SIZE, close);
        assert_eq!(terminal.layouts.tabs.len(), 1);
        assert_eq!(terminal.layouts.tabs[0].name, "New Tab");
        assert_eq!(terminal.layouts.tab, 0);
        assert_eq!(terminal.chart().symbol(), "MSFT");
    }

    #[test]
    fn chart_slots_and_layout_tabs_keep_independent_state() {
        let mut factory = IndicatorFactory::new();
        factory.register("Host pane", || {
            Box::new(CustomIndicator::new("Host pane", |_| Vec::new()).with_overlay(false))
        });
        let mut terminal = Terminal::new(Arc::new(TestFeed))
            .with_symbol("AAPL")
            .with_indicator_factory(factory);
        settle(&mut terminal);
        terminal.chart_mut().set_study_limit(Some(3));
        terminal.chart_mut().set_main_series_visible(false);
        let mut visibility = egui_charts::model::EventVisibility::default();
        visibility.show_earnings = true;
        visibility.show_economic_events = true;
        terminal.chart_mut().set_event_visibility(visibility);
        let mut hidden = ToolAvailability::default();
        hidden.visible = false;
        hidden.enabled = false;
        terminal
            .chart_mut()
            .drawings_mut()
            .registry
            .set_availability(DrawingToolType::TrendLine, hidden);
        let mut drawing = Drawing::new(7, DrawingToolType::HorizontalLine);
        drawing.completed = true;
        drawing.chart_points.push(ChartPoint::new(10.0, 42.0));
        terminal
            .chart_mut()
            .drawings_mut()
            .add_synced_drawing(drawing)
            .unwrap();
        terminal.layout.sync.sync_drawings = true;
        terminal.top_toolbar.chart_grid_selector.sync = terminal.layout.sync.clone();
        layout(
            &mut terminal,
            LayoutAction::SelectGrid(ChartLayoutMode::Four),
        );
        for chart in &terminal.charts {
            assert_eq!(chart.study_limit(), Some(3));
            assert!(!chart.is_main_series_visible());
            assert!(chart.event_visibility().show_earnings);
            assert!(
                !chart
                    .drawings()
                    .registry
                    .availability(DrawingToolType::TrendLine)
                    .enabled
            );
            assert_eq!(chart.drawings().drawings.len(), 1);
        }
        for slot in [1, 3, 0, 2] {
            terminal.activate_slot(slot);
            terminal.chart_mut().config_mut().candle_width = 0.5 + slot as f32 * 0.1;
        }
        for slot in [3, 1, 2, 0] {
            terminal.activate_slot(slot);
            assert_eq!(
                terminal.chart().config().candle_width,
                0.5 + slot as f32 * 0.1
            );
        }
        let removed = terminal.chart_mut().add_indicator(Box::new(SMA::new(7)));
        terminal.chart_mut().remove_series(removed);
        let study = terminal
            .studies
            .indicators
            .factory()
            .create("Host pane")
            .unwrap();
        let id = terminal.chart_mut().add_indicator(study);
        let mut drawing = terminal.chart().drawings().drawings[0].clone();
        drawing.id = 8;
        drawing.pane = Some(id.0);
        terminal
            .chart_mut()
            .drawings_mut()
            .add_synced_drawing(drawing)
            .unwrap();
        terminal.chart_mut().set_conversion(Some("adjusted".into()));
        let before = terminal.chart().get_time_scale_state();
        assert!(terminal.new_layout(Some("Copy".into())));
        assert_eq!(terminal.chart().conversion(), Some("adjusted"));
        assert_eq!(terminal.layout.mode, ChartLayoutMode::Four);
        assert_eq!(terminal.chart().get_time_scale_state(), before);
        assert_eq!(terminal.chart().study_limit(), Some(3));
        let copied = terminal
            .chart()
            .indicators()
            .find(|(_, study)| study.name() == "Host pane")
            .unwrap()
            .0;
        assert_eq!(
            terminal
                .chart()
                .drawings()
                .drawings
                .iter()
                .find(|drawing| drawing.pane.is_some())
                .unwrap()
                .pane,
            Some(copied.0)
        );
        terminal.chart_mut().set_symbol("copy-only");
        terminal.select_layout(0);
        assert_eq!(terminal.chart().symbol(), "AAPL");
        terminal.select_layout(1);
        assert_eq!(terminal.chart().symbol(), "copy-only");
        terminal.close_layout(1);
        assert_eq!(terminal.chart().symbol(), "AAPL");
        terminal.set_grid(ChartLayoutMode::Single);
        terminal.set_grid(ChartLayoutMode::Four);
        terminal.activate_slot(3);
        assert_eq!(terminal.chart().config().candle_width, 0.8);
    }

    #[test]
    fn a_host_study_comes_back_with_its_layout() {
        let storage = Arc::new(MemoryStorage::default());
        let factory = || {
            let mut factory = IndicatorFactory::new();
            factory.register_in(IndicatorFactory::PERSONAL, "Host pane", || {
                Box::new(CustomIndicator::new("Host pane", |_| Vec::new()).with_overlay(false))
            });
            factory
        };
        let mut terminal = Terminal::new(Arc::new(TestFeed))
            .with_symbol("AAPL")
            .with_storage(storage.clone())
            .with_indicator_factory(factory());
        settle(&mut terminal);
        terminal.user_action(|terminal| {
            let study = terminal
                .studies
                .indicators
                .factory()
                .create("Host pane")
                .unwrap();
            terminal.chart_mut().add_indicator(study);
        });
        layout(&mut terminal, LayoutAction::Save);
        drop(terminal);
        let mut terminal = Terminal::new(Arc::new(TestFeed))
            .with_symbol("AAPL")
            .with_storage(storage.clone())
            .with_indicator_factory(factory());
        settle(&mut terminal);
        assert!(
            terminal
                .chart()
                .indicators()
                .any(|(_, study)| study.name() == "Host pane"),
            "the layout keeps host studies"
        );
    }

    /// Closing saves the layout on show, unnamed or not, even when the autosave delay has not
    /// passed, and the flush waits for the write.
    #[test]
    fn closing_writes_the_open_layout() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        terminal.top_toolbar.layout_menu.autosave = true;
        terminal
            .chart_mut()
            .user_action(|chart| chart.config_mut().show_grid = false);
        assert!(!terminal.flush_storage(), "the write is still pending");
        // The host keeps calling this every frame until it is answered.
        for _ in 0..2 {
            frame(&mut terminal, SIZE, vec![]);
            if terminal.flush_storage() {
                break;
            }
        }
        assert!(terminal.flush_storage(), "the close waits for the write");
        let document = storage
            .get(StoreKind::Layouts, "Unnamed")
            .expect("the open layout is stored on close");
        let layout = SavedLayout::from_json(&document).unwrap();
        assert!(!layout.charts[0].config.as_ref().unwrap().show_grid);
        // The card of the saved layout has what it shows and when it was saved.
        assert!(layout.saved_at.is_some(), "the save time is kept");
        let info = &terminal.top_toolbar.layout_menu.info["Unnamed"];
        assert_eq!(info.symbol, "AAPL");
        assert!(info.interval.is_some());
        assert!(info.saved_at.is_some());
    }

    #[test]
    fn saved_layouts_open_again_and_after_a_restart() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        layout(
            &mut terminal,
            LayoutAction::SelectGrid(ChartLayoutMode::TwoHorizontal),
        );
        let select = |terminal: &mut Terminal, chart_type| {
            let action = Action::TopToolbar(TopToolbarAction::ChartStyleSelected(chart_type));
            assert!(terminal.run(action).is_none());
        };
        select(&mut terminal, ChartType::Line);
        terminal
            .chart_mut()
            .set_crosshair_style(CrosshairStyle::Dot);
        terminal.user_action(|terminal| {
            terminal.chart_mut().add_indicator(Box::new(SMA::new(7)));
        });
        let rsi = terminal.chart_mut().add_indicator(Box::new(RSI::new(14)));
        let pane = terminal.chart().series_pane(rsi).unwrap();
        terminal.chart_mut().move_pane(pane, 0);
        terminal.chart_mut().set_pane_stretch_factor(pane, 1.5);
        settle(&mut terminal);
        let saved = |terminal: &Terminal| {
            assert_eq!(terminal.charts.len(), 2);
            let chart = terminal.chart();
            assert_eq!(chart.chart_type(), ChartType::Line);
            assert_eq!(chart.chart_options().crosshair.style, CrosshairStyle::Dot);
            assert!(chart.indicators().any(|(_, study)| {
                study.inputs().first().map(|input| &input.value) == Some(&InputValue::Length(7))
            }));
            let rsi = chart
                .indicators()
                .find_map(|(id, study)| study.name().starts_with("RSI").then_some(id))
                .expect("RSI study");
            let pane = chart.series_pane(rsi).unwrap();
            assert_ne!(pane, PaneId::MAIN);
            assert_eq!(
                chart.panes()[0],
                pane,
                "the study pane stays above the price pane"
            );
            assert_eq!(chart.pane_stretch_factor(pane), Some(1.5));
        };
        frame(&mut terminal, SIZE, vec![key(Key::S, Modifiers::COMMAND)]);
        settle(&mut terminal);
        assert_eq!(terminal.top_toolbar.layout_menu.layouts, ["Unnamed"]);
        assert_eq!(terminal.layouts.tabs.len(), 1);
        // Opening the current layout again returns to it as saved.
        select(&mut terminal, ChartType::Area);
        layout(&mut terminal, LayoutAction::Open("Unnamed".into()));
        saved(&terminal);
        terminal.top_toolbar.layout_menu.favorites = vec!["Unnamed".into()];
        terminal.top_toolbar.layout_menu.autosave = true;
        terminal
            .chart_mut()
            .user_action(|chart| chart.config_mut().show_grid = false);
        for _ in 0..25 {
            frame(&mut terminal, SIZE, vec![]);
        }
        let document = storage.get(StoreKind::Layouts, "Unnamed").unwrap();
        let autosaved = SavedLayout::from_json(&document).unwrap();
        assert!(!autosaved.charts[0].config.as_ref().unwrap().show_grid);
        // A new terminal on the same storage opens the layout that was open last.
        drop(terminal);
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        saved(&terminal);
        assert!(terminal.top_toolbar.layout_menu.autosave);
        assert_eq!(terminal.top_toolbar.layout_menu.favorites, ["Unnamed"]);
        layout(
            &mut terminal,
            LayoutAction::Rename("Unnamed".into(), "Renamed".into()),
        );
        assert_eq!(terminal.top_toolbar.layout_menu.layouts, ["Renamed"]);
        assert_eq!(
            terminal.layouts.tabs[0].name, "Unnamed",
            "a tab keeps its name"
        );
        assert_eq!(
            terminal.layouts.tabs[0].layout.as_deref(),
            Some("Renamed"),
            "the tab shows the renamed layout"
        );
        assert_eq!(terminal.top_toolbar.layout_menu.favorites, ["Renamed"]);
        saved(&terminal);
        layout(&mut terminal, LayoutAction::SaveAs("Copy".into()));
        assert_eq!(
            terminal.top_toolbar.layout_menu.layouts,
            ["Copy", "Renamed"]
        );
        assert_eq!(terminal.layouts.tabs.len(), 2);
        saved(&terminal);
        // Opening restored the chart as one undoable edit after it.
        select(&mut terminal, ChartType::Area);
        assert!(terminal.chart().can_undo());
        terminal.run(Action::TopToolbar(TopToolbarAction::Undo));
        assert_eq!(terminal.chart().chart_type(), ChartType::Line);
        layout(&mut terminal, LayoutAction::Delete("Renamed".into()));
        assert_eq!(terminal.top_toolbar.layout_menu.layouts, ["Copy"]);
        assert!(terminal.top_toolbar.layout_menu.favorites.is_empty());

        // The chooser stays within small screens with a long catalogue and names.
        terminal.top_toolbar.layout_menu.layouts = (0..40)
            .map(|i| format!("Saved layout {i}: {}", "a long name ".repeat(10)))
            .collect();
        for theme in [
            egui_charts::theme::Theme::light(),
            egui_charts::theme::Theme::dark(),
        ] {
            let ctx = terminal.ctx();
            egui_charts::theme::apply_to_egui(&ctx, &theme);
            for size in [egui::vec2(320.0, 360.0), SIZE] {
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
                terminal.layouts.chooser = true;
                for _ in 0..3 {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        },
                        |ui| terminal.show_open_layout(ui.ctx()),
                    )
                    .drop_without_applying_deltas();
                }
                let bounds = ctx
                    .memory(|memory| memory.area_rect(egui::Id::unique("open_layout")))
                    .unwrap();
                assert!(
                    screen.contains_rect(bounds),
                    "Open layout exceeds {size:?}: {bounds:?}"
                );
            }
        }
    }

    /// A saved layout opens at the chart sizes it was saved with, whatever borders were moved
    /// in the tab since, in the same session and after a restart.
    #[test]
    fn opening_a_saved_layout_again_restores_its_chart_sizes() {
        let mode = ChartLayoutMode::TwoHorizontal;
        let resize = |terminal: &mut Terminal, columns: [f32; 2]| {
            let sizes = ChartLayoutSizing::new(mode, vec![vec![1.0], columns.to_vec()]);
            terminal.layout.set_sizes(&[sizes], false).unwrap();
            settle(terminal);
        };
        // The column weights at the save, `None` for the preset's two equal charts, and the
        // fractions of the width the layout opens at.
        for (saved, opened) in [(None, [0.5, 0.5]), (Some([1.0, 3.0]), [0.25, 0.75])] {
            let storage = Arc::new(MemoryStorage::default());
            let mut terminal = new_terminal(&storage);
            settle(&mut terminal);
            layout(&mut terminal, LayoutAction::SelectGrid(mode));
            if let Some(columns) = saved {
                resize(&mut terminal, columns);
            }
            layout(&mut terminal, LayoutAction::Save);
            let document: serde_json::Value =
                serde_json::from_str(&storage.get(StoreKind::Layouts, "Unnamed").unwrap()).unwrap();
            assert_eq!(
                document.get("sizing").is_some(),
                saved.is_some(),
                "preset proportions are stored as they were before layouts kept sizes"
            );
            let opened = [vec![1.0], opened.to_vec()];
            resize(&mut terminal, [3.0, 1.0]);
            assert_eq!(terminal.layout.sizes(mode).weights[1], [0.75, 0.25]);
            layout(&mut terminal, LayoutAction::Open("Unnamed".into()));
            assert_eq!(terminal.layout.sizes(mode).weights, opened, "{saved:?}");
            drop(terminal);
            let mut terminal = new_terminal(&storage);
            settle(&mut terminal);
            resize(&mut terminal, [3.0, 1.0]);
            layout(&mut terminal, LayoutAction::Open("Unnamed".into()));
            assert_eq!(terminal.layout.mode, mode);
            assert_eq!(terminal.charts.len(), 2);
            assert_eq!(
                terminal.layout.sizes(mode).weights,
                opened,
                "{saved:?}, after a restart"
            );
        }
    }

    #[test]
    fn the_open_tabs_come_back_after_a_restart() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        layout(&mut terminal, LayoutAction::CreateNew);
        terminal.chart_mut().set_symbol("MSFT");
        // A layout is copied or saved only once the active symbol's drawings have loaded, which
        // takes the next frames.
        settle(&mut terminal);
        settle(&mut terminal);
        layout(
            &mut terminal,
            LayoutAction::SelectGrid(ChartLayoutMode::TwoHorizontal),
        );
        layout(&mut terminal, LayoutAction::CreateNew);
        terminal.chart_mut().set_symbol("NVDA");
        settle(&mut terminal);
        settle(&mut terminal);
        let names: Vec<_> = terminal
            .layouts
            .tabs
            .iter()
            .map(|tab| tab.name.clone())
            .collect();
        assert_eq!(names, ["Unnamed", "New Tab", "New Tab 2"]);
        // The middle tab was on show last.
        terminal.select_layout(1);
        terminal.run(Action::TopToolbar(TopToolbarAction::Save));
        for _ in 0..6 {
            frame(&mut terminal, SIZE, vec![]);
        }
        assert_eq!(terminal.layouts.tab, 1);
        drop(terminal);

        let mut terminal = new_terminal(&storage);
        for _ in 0..8 {
            frame(&mut terminal, SIZE, vec![]);
        }
        let reopened: Vec<_> = terminal
            .layouts
            .tabs
            .iter()
            .map(|tab| tab.name.clone())
            .collect();
        assert_eq!(reopened, names, "the tabs, in tab order");
        assert_eq!(terminal.layouts.tab, 1, "the tab on show last");
        assert_eq!(terminal.chart().symbol(), "MSFT");
        assert_eq!(terminal.layout.mode, ChartLayoutMode::TwoHorizontal);
        // The parked tabs kept their charts too.
        let parked = |terminal: &Terminal, index: usize| {
            let workspace = terminal.layouts.tabs[index].workspace.as_ref().unwrap();
            (
                workspace.charts[0].symbol().to_owned(),
                workspace.charts.len(),
            )
        };
        assert_eq!(parked(&terminal, 0), ("AAPL".into(), 1));
        assert_eq!(parked(&terminal, 2), ("NVDA".into(), 1));
    }

    #[test]
    fn a_tab_whose_layout_is_gone_leaves_the_rest_to_come_back() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        layout(&mut terminal, LayoutAction::CreateNew);
        layout(&mut terminal, LayoutAction::CreateNew);
        let names: Vec<_> = terminal
            .layouts
            .tabs
            .iter()
            .map(|tab| tab.name.clone())
            .collect();
        assert_eq!(names, ["Unnamed", "New Tab", "New Tab 2"]);
        for _ in 0..6 {
            frame(&mut terminal, SIZE, vec![]);
        }
        // Another window deleted the middle tab's document while this one was closed.
        let gone = tab_document(terminal.layouts.tabs[1].id);
        storage.remove(StoreKind::Tabs, &gone);
        drop(terminal);

        let mut terminal = new_terminal(&storage);
        for _ in 0..16 {
            frame(&mut terminal, SIZE, vec![]);
        }
        let reopened: Vec<_> = terminal
            .layouts
            .tabs
            .iter()
            .map(|tab| tab.name.clone())
            .collect();
        assert_eq!(reopened, [names[0].clone(), names[2].clone()]);
        assert!(terminal.layouts.restoring.is_none(), "the restart finished");
    }

    /// A new tab asks what to show; picking one of the saved layouts fills the new tab with it,
    /// even when another tab could show the same one.
    #[test]
    fn a_new_tab_offers_the_saved_layouts_before_showing_a_chart() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        layout(&mut terminal, LayoutAction::SaveAs("Second".into()));
        assert!(
            !terminal.layouts.tabs[1].start,
            "a copy shows the layout it duplicates"
        );
        // Closing the tab leaves the layout saved but open in no tab.
        terminal.close_layout(1);
        settle(&mut terminal);
        layout(&mut terminal, LayoutAction::CreateNew);
        assert!(terminal.start_page(), "the new tab asks what to open");
        layout(&mut terminal, LayoutAction::Open("Second".into()));
        assert!(!terminal.start_page(), "the chosen layout is on show");
        assert_eq!(
            terminal.layouts.tabs[terminal.layouts.tab]
                .layout
                .as_deref(),
            Some("Second")
        );
        assert_eq!(
            terminal.layouts.tabs.len(),
            2,
            "the new tab keeps its place"
        );
    }

    #[test]
    fn a_layout_created_from_the_start_page_starts_clean() {
        let storage = Arc::new(MemoryStorage::default());
        let mut terminal = new_terminal(&storage);
        settle(&mut terminal);
        terminal.chart_mut().add_indicator(Box::new(SMA::new(7)));
        layout(&mut terminal, LayoutAction::CreateNew);
        assert!(terminal.start_page(), "the new tab asks what to open");
        terminal.create_layout("Fresh".into());
        for _ in 0..8 {
            frame(&mut terminal, SIZE, vec![]);
        }
        assert!(!terminal.start_page(), "the created layout is on show");
        let studies: Vec<_> = terminal
            .chart()
            .indicators()
            .map(|(_, study)| study.name().to_owned())
            .collect();
        assert_eq!(
            studies,
            ["Volume"],
            "the created layout carries no studies of its own"
        );
        assert_eq!(terminal.layouts.tabs[terminal.layouts.tab].name, "Fresh");
        assert!(
            storage.get(StoreKind::Layouts, "Fresh").is_some(),
            "the created layout is stored under the name it was given"
        );
    }
}
