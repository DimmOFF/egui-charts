//! The application settings window: the General and Tabs panes of TradingView's settings.

use super::Terminal;
use super::layout::Region;
use egui::{Align2, FontId, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};
use egui_charts::localization as l10n;
use egui_charts::services::{TabTitlePart, default_tab_title};
use egui_charts::theme::{Theme, ThemeExt};
use egui_charts::ui_kit::checkbox::checkbox;
use egui_charts::ui_kit::dialog::DialogFooter;
use egui_charts::ui_kit::modal::Modal;
use egui_charts::ui_kit::sidebar_layout::{SidebarLayout, SidebarTab};

/// The pane of the window on show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pane {
    #[default]
    General,
    Tabs,
}

/// The parts of a tab title in the order a document that lacks them gets them.
const PARTS: [TabTitlePart; 5] = [
    TabTitlePart::Ticker,
    TabTitlePart::ChangeIcon,
    TabTitlePart::LastPrice,
    TabTitlePart::ChangePercent,
    TabTitlePart::LayoutName,
];

pub(super) struct State {
    pub(super) open: bool,
    pane: Pane,
    /// Every part of the tab title, in title order, with whether the title shows it.
    title: Vec<(TabTitlePart, bool)>,
    /// Whether the tabs open at exit come back on start.
    auto_restore: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            pane: Pane::default(),
            title: default_tab_title()
                .into_iter()
                .map(|part| (part, true))
                .collect(),
            auto_restore: true,
        }
    }
}

impl Terminal {
    /// Opens the application settings at their first pane, staging the current choices.
    pub(super) fn preferences_open(&mut self) {
        self.preferences.open = true;
        self.preferences.pane = Pane::General;
        self.preferences.auto_restore = self.layouts.auto_restore;
        let title = &mut self.preferences.title;
        title.clear();
        for part in &self.layouts.title_parts {
            if !title.iter().any(|(seen, _)| seen == part) {
                title.push((*part, true));
            }
        }
        for part in &self.layouts.title_hidden {
            if !title.iter().any(|(seen, _)| seen == part) {
                title.push((*part, false));
            }
        }
        // A part the title does not mention at all is offered hidden.
        for part in PARTS {
            if !title.iter().any(|(seen, _)| *seen == part) {
                title.push((part, false));
            }
        }
    }

    pub(super) fn preferences_show(&mut self, ui: &mut Ui, region: Region) {
        if region != Region::Windows || !self.preferences.open {
            return;
        }
        let ctx = ui.ctx().clone();
        let mut open = self.preferences.open;
        let mut pane = self.preferences.pane;
        let mut reset = false;
        let mut apply = false;
        Modal::new("app_settings", "App Settings")
            .width(720.0)
            .height(480.0)
            .dim_background(false)
            .show(&ctx, &mut open, |ui| {
                let height = (ui.available_height() - 68.0).max(0.0);
                ui.allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
                    let sidebar = 160.0_f32.min(ui.available_width() * 0.4);
                    let shown = pane;
                    SidebarLayout::new(sidebar).show(
                        ui,
                        |ui| {
                            for (candidate, label) in
                                [(Pane::General, "General"), (Pane::Tabs, "Tabs")]
                            {
                                if SidebarTab::new(label, shown == candidate).show(ui) {
                                    pane = candidate;
                                }
                            }
                        },
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("app_settings_content")
                                .auto_shrink([false, false])
                                .show(ui, |ui| match shown {
                                    Pane::General => self.preferences_general(ui),
                                    Pane::Tabs => self.preferences_tabs(ui),
                                });
                        },
                    );
                });
                let (ok, _) = DialogFooter::new("Ok").show_with_left(ui, |ui| {
                    if ui
                        .button(l10n::text(ui.ctx(), "Reset to default"))
                        .clicked()
                    {
                        reset = true;
                    }
                });
                apply = ok;
            });
        self.preferences.open = open && !apply;
        self.preferences.pane = pane;
        if reset {
            self.theme_choice = None;
            self.update_theme(&ctx);
            self.preferences.auto_restore = true;
            let defaults = default_tab_title();
            let title = &mut self.preferences.title;
            title.clear();
            for part in PARTS {
                title.push((part, defaults.contains(&part)));
            }
        }
        if apply {
            self.apply_preferences();
        }
    }

    /// Writes the staged choices into the terminal, which the settings keep.
    fn apply_preferences(&mut self) {
        let title = &self.preferences.title;
        self.layouts.title_parts = title
            .iter()
            .filter(|(_, shown)| *shown)
            .map(|(part, _)| *part)
            .collect();
        self.layouts.title_hidden = title
            .iter()
            .filter(|(_, shown)| !*shown)
            .map(|(part, _)| *part)
            .collect();
        self.layouts.auto_restore = self.preferences.auto_restore;
    }

    fn preferences_general(&mut self, ui: &mut Ui) {
        let theme = ui.active_theme().clone();
        ui.label(
            RichText::new(l10n::text(ui.ctx(), "Theme"))
                .size(theme.text.heading)
                .color(theme.ui.text),
        );
        ui.add_space(theme.spacing.md);
        let (dark, light) = (Theme::dark(), Theme::light());
        let mut choice = self.theme_choice;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme.spacing.lg;
            if theme_card(ui, "System", choice.is_none(), &[&dark, &light]).clicked() {
                choice = None;
            }
            if theme_card(ui, "Dark", choice == Some(true), &[&dark]).clicked() {
                choice = Some(true);
            }
            if theme_card(ui, "Light", choice == Some(false), &[&light]).clicked() {
                choice = Some(false);
            }
        });
        if choice != self.theme_choice {
            self.theme_choice = choice;
            self.update_theme(ui.ctx());
        }
    }

    fn preferences_tabs(&mut self, ui: &mut Ui) {
        let theme = ui.active_theme().clone();
        ui.label(
            RichText::new(l10n::text(ui.ctx(), "Tabs"))
                .size(theme.text.heading)
                .color(theme.ui.text),
        );
        ui.add_space(theme.spacing.md);
        checkbox(
            ui,
            &mut self.preferences.auto_restore,
            l10n::text(ui.ctx(), "Restore tabs on start"),
        );
        ui.add_space(theme.spacing.xl);
        ui.label(
            RichText::new(l10n::text(ui.ctx(), "Tab title"))
                .size(theme.text.title)
                .color(theme.ui.text),
        );
        ui.label(
            RichText::new(l10n::text(ui.ctx(), "Drag a part to reorder it"))
                .size(theme.text.small)
                .color(theme.ui.text_muted),
        );
        ui.add_space(theme.spacing.sm);
        let mut from = None;
        let mut to = None;
        let title = &mut self.preferences.title;
        for index in 0..title.len() {
            let handle_id = ui.scope_id().with(("tab_title_handle", index));
            let (part, shown) = &mut title[index];
            let row = ui.horizontal(|ui| {
                ui.set_width(ui.available_width());
                checkbox(ui, shown, l10n::text(ui.ctx(), part_label(*part)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Only the dots drag the part, so the checkbox keeps its clicks.
                    ui.dnd_drag_source(handle_id, index, |ui| {
                        ui.label(RichText::new("⠿").color(theme.ui.text_muted))
                    })
                    .response
                    .on_hover_text(l10n::text(ui.ctx(), "Drag to reorder"));
                })
            });
            let rect = row.response.rect;
            if row.response.dnd_hover_payload::<usize>().is_some() {
                ui.painter().hline(
                    rect.x_range(),
                    rect.top(),
                    Stroke::new(2.0, theme.ui.accent),
                );
            }
            if let Some(payload) = row.response.dnd_release_payload::<usize>() {
                from = Some(*payload);
                to = Some(index);
            }
        }
        if let (Some(from), Some(to)) = (from, to)
            && from != to
        {
            let part = title.remove(from);
            title.insert(to, part);
        }
    }
}

/// The label of a part of a tab title.
fn part_label(part: TabTitlePart) -> &'static str {
    match part {
        TabTitlePart::Ticker => "Ticker",
        TabTitlePart::ChangeIcon => "Change icon",
        TabTitlePart::LastPrice => "Last price",
        TabTitlePart::ChangePercent => "Change %",
        TabTitlePart::LayoutName => "Layout name",
        _ => "Unknown",
    }
}

/// A theme card: a preview of the chart surface with its name under it, framed in accent when
/// chosen. Each preview fills a column, so System shows both appearances.
fn theme_card(ui: &mut Ui, label: &str, selected: bool, previews: &[&Theme]) -> Response {
    let theme = ui.active_theme().clone();
    let size = Vec2::new(112.0, 76.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let preview = egui::Rect::from_min_size(rect.min, Vec2::new(size.x, size.y - 20.0));
    let painter = ui.painter();
    let radius = theme.radius.sm as u8;
    let column_width = preview.width() / previews.len().max(1) as f32;
    for (index, shelf) in previews.iter().enumerate() {
        let column = egui::Rect::from_min_size(
            egui::pos2(preview.left() + column_width * index as f32, preview.top()),
            Vec2::new(column_width, preview.height()),
        );
        let mut corners = egui::CornerRadius::ZERO;
        if index == 0 {
            corners.nw = radius;
            corners.sw = radius;
        }
        if index + 1 == previews.len() {
            corners.ne = radius;
            corners.se = radius;
        }
        painter.rect_filled(column, corners, shelf.chart.bg);
        for step in 0..3 {
            let x = column.left() + column.width() * (0.28 + 0.22 * step as f32);
            let color = if step == 1 {
                shelf.chart.candle_down
            } else {
                shelf.chart.candle_up
            };
            painter.line_segment(
                [
                    egui::pos2(x, column.top() + column.height() * 0.2),
                    egui::pos2(x, column.top() + column.height() * 0.8),
                ],
                Stroke::new(1.0, color),
            );
            painter.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(x, column.center().y),
                    Vec2::new(column.width() * 0.16, column.height() * 0.26),
                ),
                1.0,
                color,
            );
        }
    }
    let border = if selected || response.has_focus() {
        Stroke::new(2.0, theme.ui.accent)
    } else if response.hovered() {
        Stroke::new(1.0, theme.ui.text_muted)
    } else {
        Stroke::new(1.0, theme.ui.border)
    };
    painter.rect_stroke(preview, theme.radius.sm, border, StrokeKind::Inside);
    painter.text(
        rect.center_bottom(),
        Align2::CENTER_BOTTOM,
        l10n::text(ui.ctx(), label),
        FontId::proportional(theme.text.body),
        if selected {
            theme.ui.text
        } else {
            theme.ui.text_secondary
        },
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::RadioButton, ui.is_enabled(), selected, label)
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestFeed;
    use std::sync::Arc;

    #[test]
    fn the_tabs_pane_keeps_the_order_and_visibility_of_title_parts() {
        let mut terminal = Terminal::new(Arc::new(TestFeed));
        terminal.layouts.title_parts = vec![TabTitlePart::LastPrice, TabTitlePart::Ticker];
        terminal.layouts.title_hidden = vec![TabTitlePart::LayoutName];
        terminal.layouts.auto_restore = false;
        terminal.preferences_open();
        assert_eq!(
            terminal.preferences.title,
            [
                (TabTitlePart::LastPrice, true),
                (TabTitlePart::Ticker, true),
                (TabTitlePart::LayoutName, false),
                (TabTitlePart::ChangeIcon, false),
                (TabTitlePart::ChangePercent, false),
            ],
            "the pane stages the order and hides the parts the title leaves out"
        );
        terminal.preferences.title = vec![
            (TabTitlePart::Ticker, true),
            (TabTitlePart::LayoutName, true),
            (TabTitlePart::LastPrice, false),
        ];
        terminal.preferences.auto_restore = true;
        terminal.apply_preferences();
        assert_eq!(
            terminal.layouts.title_parts,
            [TabTitlePart::Ticker, TabTitlePart::LayoutName]
        );
        assert_eq!(terminal.layouts.title_hidden, [TabTitlePart::LastPrice]);
        assert!(terminal.layouts.auto_restore);
    }
}
