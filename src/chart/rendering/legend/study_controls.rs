use crate::localization as l10n;
use crate::theme::ThemeExt;
use egui::FontId;
use egui::{Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

/// Side of a study legend button.
const BUTTON_SIZE: f32 = 24.0;

/// Width of the study's buttons: a read-only study keeps hide, a script's edit button and, with
/// an article, help.
pub(crate) fn controls_width(study: &dyn crate::studies::Indicator) -> f32 {
    let buttons = 1
        + 3 * usize::from(study.user_edit_enabled())
        + usize::from(study.script_editable())
        + usize::from(study.help_url().is_some());
    BUTTON_SIZE * buttons as f32
}

/// The row outline and its hide, settings, edit-script, remove, more-actions and help buttons.
/// Returns the first four buttons and, when the more-actions button is clicked or the row
/// secondary-clicked, the corner of the study menu. `backdrop`, the chart background behind the
/// controls, fills them when they are revealed, for a caller that paints nothing under them.
pub(crate) fn study_controls(
    ui: &mut Ui,
    row: Rect,
    controls: Rect,
    backdrop: Option<egui::Color32>,
    foreground: egui::Color32,
    id: egui::Id,
    study: &dyn crate::studies::Indicator,
) -> ([Response; 4], Option<Pos2>) {
    let theme = ui.active_theme();
    let visible = study.is_visible();
    let editable = study.user_edit_enabled();
    let help = study.help_url();
    // The script button follows the settings, and the help button the more-actions one.
    let shown = |index: usize| match index {
        0 => true,
        5 => help.is_some(),
        2 => study.script_editable(),
        _ => editable,
    };
    let touch_id = egui::Id::unique("legend_touch_input");
    let input_touch = ui.input(|input| {
        if input.any_touches()
            || input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Touch { .. }))
        {
            Some(true)
        } else if input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::PointerMoved(_)))
        {
            Some(false)
        } else {
            None
        }
    });
    let touch = ui.ctx().data_mut(|data| {
        let touch = input_touch.unwrap_or_else(|| data.get_temp::<bool>(touch_id).unwrap_or(false));
        data.insert_temp(touch_id, touch);
        touch
    });
    let row_response = ui.interact(row, id.with("row"), Sense::click());
    let controls_label = l10n::text(ui.ctx(), "Study controls");
    row_response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &controls_label));
    let mut menu = row_response
        .secondary_clicked()
        .then(|| row_response.interact_pointer_pos())
        .flatten();
    let clicked_elsewhere = row_response.clicked_elsewhere();
    let clicked = row_response.clicked();
    let selected = ui.ctx().data_mut(|data| {
        let mut selected = data.get_temp::<bool>(id).unwrap_or(false);
        if clicked_elsewhere || !touch {
            selected = false;
        } else if clicked {
            selected = true;
        }
        data.insert_temp(id, selected);
        selected
    });
    // Keep controls focusable while Tab transfers focus from their row.
    let focused = row_response.has_focus()
        || row_response.lost_focus()
        || (0..6_usize)
            .any(|index| shown(index) && ui.memory(|memory| memory.has_focus(id.with(index))));
    let hovered = !touch
        && ui.input(|input| input.pointer.hover_pos().is_some())
        && ui.rect_contains_pointer(row);
    let revealed = selected || focused || hovered;
    // A legend line is not a chart hit: keep the arrow instead of the chart's crosshair.
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
    }
    if revealed {
        if let Some(backdrop) = backdrop {
            ui.painter()
                .rect_filled(controls, theme.radius.sm, backdrop);
        }
        ui.painter().rect_stroke(
            row.union(controls.expand2(Vec2::new(0.0, 2.0))),
            theme.radius.sm,
            Stroke::new(1.0, theme.ui.accent),
            egui::StrokeKind::Inside,
        );
    }
    let buttons: [Response; 6] = std::array::from_fn(|index| {
        let slot = (0..index).filter(|other| shown(*other)).count();
        let rect = Rect::from_min_size(
            controls.min + Vec2::new(slot as f32 * BUTTON_SIZE, 0.0),
            Vec2::splat(BUTTON_SIZE),
        )
        .intersect(controls);
        let label = l10n::text(
            ui.ctx(),
            match index {
                0 if visible => "Hide study",
                0 => "Show study",
                1 => "Study settings",
                2 => "Edit script",
                3 => "Remove study",
                5 => "Study help",
                _ => "More actions",
            },
        );
        let shown = shown(index);
        let response = ui.interact(
            rect,
            id.with(index),
            if revealed && shown {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        // A hidden study keeps its crossed-out eye on show, as TradingView does.
        let hidden_eye = index == 0 && !visible;
        // Without a row to hover, nothing follows to paint the icon above the hover fill.
        if hidden_eye && !revealed {
            #[cfg(feature = "icons")]
            crate::icons::icons::EYE_HIDE
                .as_image_tinted(Vec2::splat(18.0), foreground)
                .paint_at(ui, rect.shrink(3.0));
            #[cfg(not(feature = "icons"))]
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "+",
                FontId::proportional(16.0),
                foreground,
            );
        }
        if !revealed || !shown {
            return response;
        }
        let response = response.on_hover_text(&label);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        if response.hovered() || ui.rect_contains_pointer(rect) {
            // Mixed from the chart colours, so it stays subtle in light and dark themes.
            ui.painter().rect_filled(
                rect,
                theme.radius.sm,
                theme.chart.bg.lerp_to_gamma(foreground, 0.12),
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if index == 5 {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "?",
                FontId::proportional(16.0),
                foreground,
            );
            if response.clicked()
                && let Some(url) = help
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        } else {
            #[cfg(feature = "icons")]
            {
                let icon = match index {
                    0 if visible => &crate::icons::icons::EYE,
                    0 => &crate::icons::icons::EYE_HIDE,
                    1 => &crate::icons::icons::SETTINGS,
                    2 => &crate::icons::icons::BRACES,
                    3 => &crate::icons::icons::TRASH,
                    _ => &crate::icons::icons::UI_MORE_HORIZONTAL,
                };
                icon.as_image_tinted(Vec2::splat(18.0), foreground)
                    .paint_at(ui, rect.shrink(3.0));
            }
            #[cfg(not(feature = "icons"))]
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                [if visible { "−" } else { "+" }, "...", "{}", "×", "•••"][index],
                FontId::proportional(16.0),
                foreground,
            );
        }
        response
    });
    let [hide, settings, script, remove, more, _] = buttons;
    if more.clicked() {
        menu = Some(more.rect.left_bottom());
    }
    ([hide, settings, script, remove], menu)
}

#[cfg(all(test, feature = "studies"))]
mod tests {
    use super::super::{Bar, Indicator, Legend};
    use super::*;
    use chrono::{TimeZone, Utc};
    use egui::{Context, Event, Key, PointerButton, Shape};

    #[test]
    fn study_controls_reveal_on_touch_and_dismiss_outside() {
        for theme in [crate::theme::Theme::light(), crate::theme::Theme::dark()] {
            let ctx = Context::default();
            crate::theme::apply_to_egui(&ctx, &theme);
            // A translator that changes every key must reach the control labels.
            l10n::set_translator(&ctx, |english| Some(format!("{english}!")));
            let row = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(220.0, 20.0));
            let controls = Rect::from_min_size(Pos2::new(134.0, 10.0), Vec2::new(96.0, 20.0));
            let settings = controls.min + Vec2::new(36.0, 10.0);
            let frame = |events| {
                let mut clicked = [false; 4];
                let mut menu = None;
                let output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let (buttons, opened) = study_controls(
                            ui,
                            row,
                            controls,
                            Some(theme.chart.bg),
                            theme.chart.axis_text,
                            egui::Id::unique("study"),
                            &crate::studies::CustomIndicator::new("Plain", |_| Vec::new()),
                        );
                        clicked = buttons.map(|response| response.clicked());
                        menu = opened;
                    },
                );
                // An accent outline 24 px tall, 2 px past the 20 px row on each side.
                let outline = Rect::from_min_size(Pos2::new(10.0, 8.0), Vec2::new(220.0, 24.0));
                let painted = output.shapes.iter().any(|s| match &s.shape {
                    Shape::Rect(r) => r.rect == outline && r.stroke.color == theme.ui.accent,
                    _ => false,
                });
                let buttons_filled = output.shapes.iter().any(|s| {
                    matches!(&s.shape, Shape::Rect(r) if r.rect == controls && r.fill == theme.chart.bg)
                });
                assert_eq!(
                    buttons_filled, painted,
                    "only revealed controls mask the chart"
                );
                let focused_label = output.platform_output.events.iter().find_map(|event| {
                    if let egui::output::OutputEvent::FocusGained(info) = event {
                        info.label.clone()
                    } else {
                        None
                    }
                });
                output.drop_without_applying_deltas();
                (painted, clicked, focused_label, menu)
            };
            let touch = |pos, phase| Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(0),
                phase,
                pos,
                force: None,
            };
            let tap = |pos| {
                frame(vec![
                    touch(pos, egui::TouchPhase::Start),
                    Event::PointerMoved(pos),
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ]);
                frame(vec![
                    touch(pos, egui::TouchPhase::End),
                    Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                    Event::PointerGone,
                ])
            };
            assert!(!frame(vec![Event::PointerGone]).0);
            let first = tap(settings);
            assert_eq!(first.1, [false; 4], "the first tap only reveals controls");
            let second = tap(settings);
            assert_eq!(
                second.1,
                [false, true, false, false],
                "the second tap opens settings"
            );
            tap(row.left_center() + Vec2::new(10.0, 0.0));
            assert!(frame(vec![]).0, "touch selection must survive pointer exit");
            tap(Pos2::new(250.0, 100.0));
            assert!(!frame(vec![]).0, "outside tap must dismiss controls");
            assert!(
                frame(vec![Event::PointerMoved(row.center())]).0,
                "mouse hover reveals controls"
            );
            // A secondary click on the row opens the menu at the pointer.
            let pos = row.left_center() + Vec2::new(20.0, 0.0);
            frame(vec![Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                let clicked = frame(vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Secondary,
                    pressed,
                    modifiers: Default::default(),
                }]);
                assert_eq!(clicked.3, (!pressed).then_some(pos));
            }
            assert!(!frame(vec![Event::PointerGone]).0);
            for (index, label) in [
                "Study controls",
                "Hide study",
                "Study settings",
                "Remove study",
                "More actions",
            ]
            .into_iter()
            .enumerate()
            {
                let focused = frame(vec![Event::Key {
                    key: Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }]);
                assert!(focused.0, "keyboard focus must reveal {label}");
                assert_eq!(focused.2, Some(format!("{label}!")));
                assert!(frame(vec![]).0, "focused controls must stay visible");
                let activated = frame(vec![Event::Key {
                    key: Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }]);
                let mut expected = [false; 4];
                match index {
                    1 => expected[0] = true,
                    2 => expected[1] = true,
                    3 => expected[3] = true,
                    _ => {}
                }
                assert_eq!(activated.1, expected);
                // The menu opens below the more-actions button.
                assert_eq!(
                    activated.3,
                    (index == 4).then(|| controls.min + Vec2::new(72.0, 20.0))
                );
            }

            // Read-only studies keep visibility and help; only editable ones keep the edit buttons.
            let mut study =
                crate::studies::ConfiguredStudy::new(Box::new(crate::studies::SMA::new(1)));
            let mut saved = serde_json::to_value(study.settings()).unwrap();
            saved["user_edit_enabled"] = false.into();
            saved["help_url"] = "https://example.org/average".into();
            study
                .apply_settings(&[], serde_json::from_value(saved).unwrap())
                .unwrap();
            let ctx = Context::default();
            crate::theme::apply_to_egui(&ctx, &theme);
            let controls = Rect::from_min_size(Pos2::new(150.0, 10.0), Vec2::new(120.0, 20.0));
            let row = row.union(controls);
            let frame = |events| {
                let mut clicks = [false; 4];
                let output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let (buttons, _) = study_controls(
                            ui,
                            row,
                            controls,
                            Some(theme.chart.bg),
                            theme.chart.axis_text,
                            egui::Id::unique("locked"),
                            &study,
                        );
                        clicks = buttons.map(|button| button.clicked());
                    },
                );
                let urls: Vec<_> = output
                    .platform_output
                    .commands
                    .iter()
                    .filter_map(|command| {
                        if let egui::OutputCommand::OpenUrl(url) = command {
                            Some(url.url.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                output.drop_without_applying_deltas();
                (clicks, urls)
            };
            for index in [1, 2, 3, 4, 0] {
                let help_slot = index == 1;
                let pos = controls.min + Vec2::new(index as f32 * 24.0 + 12.0, 10.0);
                frame(vec![Event::PointerMoved(pos)]);
                let mut opened = Vec::new();
                for pressed in [true, false] {
                    let (clicks, urls) = frame(vec![Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    }]);
                    assert_eq!(clicks, [index == 0 && !pressed, false, false, false]);
                    opened.extend(urls);
                }
                assert_eq!(
                    opened,
                    if help_slot {
                        vec!["https://example.org/average".to_owned()]
                    } else {
                        Vec::new()
                    }
                );
            }

            let mut settings = serde_json::to_value(study.settings()).unwrap();
            settings["user_edit_enabled"] = true.into();
            study
                .apply_settings(&[], serde_json::from_value(settings).unwrap())
                .unwrap();
            let pos = controls.min + Vec2::new(108.0, 10.0);
            let mut urls = Vec::new();
            for events in [
                vec![Event::PointerMoved(pos)],
                vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                }],
                vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            ] {
                let output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        study_controls(
                            ui,
                            row,
                            controls,
                            Some(theme.chart.bg),
                            theme.chart.axis_text,
                            egui::Id::unique("locked"),
                            &study,
                        );
                    },
                );
                for command in &output.platform_output.commands {
                    if let egui::OutputCommand::OpenUrl(url) = command {
                        urls.push(url.url.clone());
                    }
                }
                output.drop_without_applying_deltas();
            }
            assert_eq!(urls, ["https://example.org/average"]);

            // A wrapped row is selectable over its text, even when the controls wrap below it.
            let ctx = Context::default();
            crate::theme::apply_to_egui(&ctx, &theme);
            let bar = Bar::new(
                Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
                100.0,
                110.0,
                90.0,
                105.0,
                1000.0,
            );
            let studies: Vec<Box<dyn Indicator>> = vec![Box::new(
                crate::studies::CustomIndicator::new("A long overlay study title", |_| Vec::new())
                    .with_overlay(true),
            )];
            let pos = Pos2::new(130.0, 16.0);
            let frame = |events| {
                let output = ctx.run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let mut legend = Legend::new("SAMPLE", "1h", &bar);
                        legend.show_title = false;
                        legend.show_ohlc = false;
                        legend.show_change = false;
                        legend.show_change_percent = false;
                        legend.show_volume = false;
                        legend.studies = &studies;
                        legend.show(ui, Rect::from_min_size(Pos2::ZERO, Vec2::new(160.0, 400.0)));
                    },
                );
                let selected = output.shapes.iter().any(|shape| {
                    matches!(&shape.shape,
                    Shape::Rect(rect) if rect.stroke.width > 0.0 && rect.rect.contains(pos))
                });
                output.drop_without_applying_deltas();
                selected
            };
            frame(vec![]);
            frame(vec![
                touch(pos, egui::TouchPhase::Start),
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ]);
            frame(vec![
                touch(pos, egui::TouchPhase::End),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
                Event::PointerGone,
            ]);
            assert!(
                frame(vec![]),
                "the full wrapped study row must respond to a tap"
            );
        }
    }

    /// The legend line is a label, not a chart hit: hovering it shows the arrow, not the
    /// crosshair the chart sets.
    #[test]
    fn hovering_the_legend_row_shows_the_arrow() {
        let theme = crate::theme::Theme::dark();
        let ctx = Context::default();
        crate::theme::apply_to_egui(&ctx, &theme);
        let study = crate::studies::CustomIndicator::new("Plain", |_| Vec::new());
        let row = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(220.0, 20.0));
        let controls = Rect::from_min_size(Pos2::new(134.0, 10.0), Vec2::new(96.0, 20.0));
        let output = ctx.run_ui(
            egui::RawInput {
                events: vec![Event::PointerMoved(row.center())],
                ..Default::default()
            },
            |ui| {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                study_controls(
                    ui,
                    row,
                    controls,
                    None,
                    theme.chart.axis_text,
                    egui::Id::unique("arrow"),
                    &study,
                );
            },
        );
        assert_eq!(
            output.platform_output.cursor_icon,
            egui::CursorIcon::Default
        );
        output.drop_without_applying_deltas();
    }

    /// The button hover is mixed from the chart colours, so it stays subtle in both themes
    /// instead of using the brighter UI hover fill.
    #[test]
    fn study_button_hover_fill_follows_the_chart_colours() {
        for theme in [crate::theme::Theme::light(), crate::theme::Theme::dark()] {
            let ctx = Context::default();
            crate::theme::apply_to_egui(&ctx, &theme);
            let study = crate::studies::CustomIndicator::new("Plain", |_| Vec::new());
            let row = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(220.0, 20.0));
            let controls = Rect::from_min_size(Pos2::new(134.0, 10.0), Vec2::new(96.0, 20.0));
            let settings = Rect::from_min_size(
                controls.min + Vec2::new(BUTTON_SIZE, 0.0),
                Vec2::splat(BUTTON_SIZE),
            )
            .intersect(controls);
            let output = ctx.run_ui(
                egui::RawInput {
                    events: vec![Event::PointerMoved(settings.center())],
                    ..Default::default()
                },
                |ui| {
                    study_controls(
                        ui,
                        row,
                        controls,
                        None,
                        theme.chart.axis_text,
                        egui::Id::unique("hover fill"),
                        &study,
                    );
                },
            );
            let fill = output.shapes.iter().find_map(|shape| match &shape.shape {
                Shape::Rect(rect) if rect.rect == settings => Some(rect.fill),
                _ => None,
            });
            assert_eq!(
                fill,
                Some(theme.chart.bg.lerp_to_gamma(theme.chart.axis_text, 0.12))
            );
            output.drop_without_applying_deltas();
        }
    }

    /// A script study offers its edit button between the settings and the remove one.
    #[test]
    fn a_script_study_offers_an_edit_button() {
        let theme = crate::theme::Theme::dark();
        let ctx = Context::default();
        crate::theme::apply_to_egui(&ctx, &theme);
        let study = crate::studies::CustomIndicator::new("Script", |_| Vec::new())
            .with_script_editable(true);
        assert_eq!(controls_width(&study), 120.0);
        let row = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(260.0, 20.0));
        let controls = Rect::from_min_size(Pos2::new(150.0, 10.0), Vec2::new(120.0, 20.0));
        // The second button after hide: hidden settings come first, then this one.
        let pos = controls.min + Vec2::new(60.0, 10.0);
        let frame = |events| {
            let mut clicked = [false; 4];
            ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    let (buttons, _) = study_controls(
                        ui,
                        row,
                        controls,
                        None,
                        theme.chart.axis_text,
                        egui::Id::unique("script"),
                        &study,
                    );
                    clicked = buttons.map(|response| response.clicked());
                },
            )
            .drop_without_applying_deltas();
            clicked
        };
        frame(vec![Event::PointerMoved(pos)]);
        let button = |pressed| {
            frame(vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            }])
        };
        assert_eq!(button(true), [false; 4], "a press clicks nothing yet");
        assert_eq!(
            button(false),
            [false, false, true, false],
            "the edit button sits between settings and remove"
        );
    }
}
