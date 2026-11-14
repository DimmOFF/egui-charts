//! Text label and anchored text rendering

use crate::drawings::domain::{Drawing, TextAnchor};
use crate::drawings::rendering::ChartView;
use crate::styles::typography::font_size;
use crate::tokens::DESIGN_TOKENS;
use egui::epaint::StrokeKind;

/// Where the text's top-left corner sits from the point, in points: the box keeps the edge or
/// corner its anchor names on the point, and one set to a side holds it `spacing.sm` off.
fn text_offset(anchor: TextAnchor, size: egui::Vec2) -> egui::Vec2 {
    // The share of the box that is left of the point and above it.
    let (x, y) = match anchor {
        TextAnchor::Overlay => return egui::Vec2::ZERO,
        TextAnchor::Below => (0.5, 0.0),
        TextAnchor::Above => (0.5, 1.0),
        TextAnchor::Right => (0.0, 0.5),
        TextAnchor::Left => (1.0, 0.5),
        TextAnchor::BelowRight => (0.0, 0.0),
        TextAnchor::BelowLeft => (1.0, 0.0),
        TextAnchor::AboveRight => (0.0, 1.0),
        TextAnchor::AboveLeft => (1.0, 1.0),
        TextAnchor::Centered => (0.5, 0.5),
    };
    let gap = DESIGN_TOKENS.spacing.sm;
    egui::vec2(
        -size.x * x + gap * (1.0 - 2.0 * x),
        -size.y * y + gap * (1.0 - 2.0 * y),
    )
}

impl Drawing {
    /// Renders Text and Anchored text with its box on the first anchor as
    /// [`text_anchor`](Drawing::text_anchor) places it, surrounded by a background and border when
    /// enabled. Anchored text differs only in how its anchor follows the pane
    /// (`Drawing::update_pane_coords`). Wrapped text breaks at word boundaries, and inside a word
    /// wider than the wrap width. An anchored box centres each of its lines, as a Pine label does.
    /// Text that follows the zoom scales its font, wrap width and box padding with the bar spacing.
    pub(crate) fn render_text_label(&self, painter: &egui::Painter, view: &ChartView) {
        let Some(&pos) = self.points.first() else {
            return;
        };
        let colors = self.text_box_colors(painter.ctx());
        let text = self.display_text(painter.ctx());
        let mut galley = self.annotation_galley(painter, text, colors.text);
        // The font stays within the sizes text is laid out at, and the box keeps its proportions.
        let set = self.annotation_font().size;
        let font = font_size(set * self.text_zoom(view.bar_to_x, pos.x));
        let scale = font / set;
        let anchored = self.text_anchor != TextAnchor::Overlay;
        if self.text_wrap || scale != 1.0 || anchored {
            let mut job = (*galley.job).clone();
            for section in &mut job.sections {
                section.format.font_id.size = font;
            }
            if self.text_wrap {
                job.wrap.max_width = self.text_wrap_width * scale;
            }
            if anchored {
                job.halign = egui::Align::Center;
            }
            galley = painter.layout_job(job);
        }
        // `galley.rect` starts left of the paint position for centred lines.
        let min = pos + text_offset(self.text_anchor, galley.size()) - galley.rect.min.to_vec2();
        if colors.fill.a() > 0 || !colors.border.is_empty() {
            let padding = egui::vec2(
                DESIGN_TOKENS.sizing.annotation.label_padding_x,
                DESIGN_TOKENS.sizing.annotation.text_padding_y,
            ) * scale;
            let box_rect =
                egui::Rect::from_min_size(min - padding, galley.size() + padding * 2.0);
            painter.rect(
                box_rect,
                DESIGN_TOKENS.rounding.md * scale,
                colors.fill,
                colors.border,
                StrokeKind::Outside,
            );
        }
        painter.galley(min, galley, colors.text);
    }
}
