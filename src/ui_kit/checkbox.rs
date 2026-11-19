//! A checkbox as TradingView draws it.

use crate::theme::ThemeExt;
use egui::Response;

/// A checkbox as TradingView draws it: a filled rounded box that turns accent when checked, with
/// a white tick and a label in the normal text colour.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: impl Into<egui::WidgetText>,
) -> Response {
    let theme = ui.active_theme();
    let field = theme.ui.panel_bg_secondary;
    let border = field.lerp_to_gamma(theme.ui.text, 0.16);
    let size = ui.spacing().icon_width;
    // egui paints its own box from the widget visuals; keep its input and accessibility and
    // paint the TradingView box over it, as the chart settings checkboxes do.
    let previous = ui.visuals().widgets.clone();
    {
        let widgets = &mut ui.visuals_mut().widgets;
        for state in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
        ] {
            state.bg_fill = egui::Color32::TRANSPARENT;
            state.bg_stroke = egui::Stroke::NONE;
            state.fg_stroke.color = egui::Color32::TRANSPARENT;
        }
    }
    let label = egui::RichText::new(label.into().text()).color(theme.ui.text);
    let response = ui.checkbox(checked, label);
    ui.visuals_mut().widgets = previous;
    let rect = egui::Rect::from_min_size(
        egui::pos2(response.rect.left(), response.rect.center().y - size / 2.0),
        egui::Vec2::splat(size),
    );
    let fill = match (*checked, response.hovered()) {
        (true, true) => theme.ui.accent_hover,
        (true, false) => theme.ui.accent,
        (false, true) => field.lerp_to_gamma(theme.ui.text, 0.10),
        (false, false) => field,
    };
    let stroke = if *checked {
        egui::Stroke::NONE
    } else {
        egui::Stroke::new(crate::tokens::DESIGN_TOKENS.stroke.hairline, border)
    };
    ui.painter().rect(
        rect,
        theme.radius.sm,
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    if *checked {
        let tick = rect.shrink(size * 0.28);
        ui.painter().add(egui::Shape::line(
            vec![tick.left_center(), tick.center_bottom(), tick.right_top()],
            egui::Stroke::new(
                crate::tokens::DESIGN_TOKENS.stroke.medium,
                theme.ui.accent_text,
            ),
        ));
    }
    crate::styles::focus::draw_focus_ring(ui, &response, theme.radius.sm);
    response
}
