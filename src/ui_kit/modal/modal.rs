//! Modal Widget
//!
//! A modal dialog in the dialog chrome, centred on the screen above a dimmed backdrop.

use crate::localization as l10n;
use egui::{Color32, Context, Id, Ui};

use crate::theme::ThemeExt;
use crate::tokens::DESIGN_TOKENS;
use crate::ui_kit::dialog::{dialog_frame, title_row_with_drag};

/// A modal dialog widget
#[derive(Debug)]
pub struct Modal {
    id: Id,
    title: String,
    width: Option<f32>,
    height: Option<f32>,
    min_height: Option<f32>,
    closable: bool,
    dim_background: bool,
    rounding: Option<f32>,
    fill: Option<Color32>,
    backdrop: Option<Color32>,
    top: Option<f32>,
    draggable: bool,
}

impl Modal {
    /// Create a new modal with the given title. An empty title leaves the header to the content.
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, title: impl Into<String>) -> Self {
        Self {
            id: Id::unique(id),
            title: title.into(),
            width: None,
            height: None,
            min_height: None,
            closable: true,
            dim_background: true,
            rounding: None,
            fill: None,
            backdrop: None,
            top: None,
            draggable: false,
        }
    }

    /// Set the outer width of the modal, including its frame padding
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Set the outer height of the modal, including its 16 px padding, for content that fills
    /// the modal, such as a list in a scroll area. The modal keeps 24 px free above and below it
    /// in the window and follows the window when it shrinks. Overrides [`Self::min_height`].
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// Set the minimum outer height of the modal, including its 16 px padding
    pub fn min_height(mut self, height: f32) -> Self {
        self.min_height = Some(height);
        self
    }

    /// Set whether the modal can be closed with the close button, Escape or a backdrop click
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// Override the surface fill, leaving the theme's panel fill as the default.
    pub fn fill(mut self, fill: Color32) -> Self {
        self.fill = Some(fill);
        self
    }

    /// Override the backdrop colour, leaving the theme's modal overlay as the default.
    pub fn backdrop(mut self, color: Color32) -> Self {
        self.backdrop = Some(color);
        self
    }

    /// Override the corner radius, leaving the theme's radius as the default.
    pub fn rounding(mut self, rounding: f32) -> Self {
        self.rounding = Some(rounding);
        self
    }

    /// Keep a fixed top edge while content height changes; clamp it inside the window.
    pub fn top(mut self, top: f32) -> Self {
        self.top = Some(top);
        self
    }

    /// Set whether to dim the background
    pub fn dim_background(mut self, dim: bool) -> Self {
        self.dim_background = dim;
        self
    }

    /// Let the user move the modal by its title row. The place is remembered for this id.
    pub fn draggable(mut self, draggable: bool) -> Self {
        self.draggable = draggable;
        self
    }

    /// Show the modal
    ///
    /// Returns `Some(R)` if the modal was shown, where R is the result of the content function.
    /// The `open` parameter controls whether the modal is visible and can be set to `false`
    /// to close the modal.
    pub fn show<R>(
        self,
        ctx: &Context,
        open: &mut bool,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<ModalResponse<R>> {
        if !*open {
            return None;
        }

        let mut frame = dialog_frame(ctx);
        if let Some(fill) = self.fill {
            frame = frame.fill(fill);
        }
        if let Some(rounding) = self.rounding {
            frame = frame.corner_radius(rounding);
        }
        let screen_height =
            (ctx.content_rect().height() - 2.0 * ctx.active_theme().spacing.xxxl).max(0.0);
        let height = self.height.map(|height| height.min(screen_height));
        let size = egui::vec2(
            self.width
                .unwrap_or(DESIGN_TOKENS.sizing.dialog.default_width)
                .min((ctx.content_rect().width() - 48.0).max(0.0)),
            height
                .or(self.min_height)
                .unwrap_or(DESIGN_TOKENS.sizing.dialog.default_min_height)
                .min(screen_height),
        ) - frame.total_margin().sum();
        let size = size.max(egui::Vec2::ZERO);
        let max_height = (height.unwrap_or(screen_height) - frame.total_margin().sum().y).max(0.0);
        let backdrop = if self.dim_background {
            self.backdrop.unwrap_or(ctx.active_theme().ui.modal_overlay)
        } else {
            Color32::TRANSPARENT
        };

        let mut close_clicked = false;
        let mut modal = egui::Modal::new(self.id);
        let drag_id = self.id.with("title_drag");
        let offset_id = self.id.with("drag_offset");
        let offset = ctx
            .data(|data| data.get_temp::<egui::Vec2>(offset_id))
            .unwrap_or(egui::Vec2::ZERO);
        if self.top.is_some() || self.draggable {
            let screen = ctx.content_rect();
            let (pivot, mut pos) = match self.top {
                Some(top) => (
                    egui::Align2::CENTER_TOP,
                    egui::pos2(
                        screen.center().x,
                        top.clamp(
                            screen.top() + 24.0,
                            (screen.bottom() - height.unwrap_or(0.0) - 24.0)
                                .max(screen.top() + 24.0),
                        ),
                    ),
                ),
                None => (egui::Align2::CENTER_CENTER, screen.center()),
            };
            if self.draggable {
                pos += offset;
            }
            modal = modal.area(
                egui::Area::new(self.id)
                    .order(egui::Order::Foreground)
                    .pivot(pivot)
                    .fixed_pos(pos),
            );
        }
        let response = modal.frame(frame).backdrop_color(backdrop).show(ctx, |ui| {
            ui.set_width(size.x);
            ui.set_min_height(size.y);
            // Start from the window cap each frame, so scrolling content can grow and shrink.
            ui.set_max_height(max_height);
            if !self.title.is_empty() {
                let drag = self.draggable.then_some(drag_id);
                close_clicked = title_row_with_drag(ui, &self.title, self.closable, drag);
            }
            add_contents(ui)
        });

        if self.draggable {
            // Apply the pointer motion one frame late, so the drag never fights the layout, and
            // keep every edge of the moved modal inside the window.
            let screen = ctx.content_rect();
            let rect = response.response.rect;
            let delta = if ctx.is_being_dragged(drag_id) {
                ctx.input(|input| input.pointer.delta())
            } else {
                egui::Vec2::ZERO
            };
            let clamp = |screen_lo: f32, screen_hi: f32, lo: f32, hi: f32, shift: f32| {
                let (low, high) = (screen_lo - lo, screen_hi - hi);
                if low <= high {
                    shift.clamp(low, high)
                } else {
                    0.0
                }
            };
            let moved = offset
                + egui::vec2(
                    clamp(
                        screen.left(),
                        screen.right(),
                        rect.left(),
                        rect.right(),
                        delta.x,
                    ),
                    clamp(
                        screen.top(),
                        screen.bottom(),
                        rect.top(),
                        rect.bottom(),
                        delta.y,
                    ),
                );
            if moved != offset {
                ctx.data_mut(|data| data.insert_temp(offset_id, moved));
            }
        }

        if !self.title.is_empty() {
            let translated_label = l10n::text(ctx, &self.title);
            response.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Dialog, true, &translated_label)
            });
            ctx.accesskit_node_builder(response.response.id, |node| node.set_modal());
        }

        // `should_close` covers Escape and a click on the backdrop.
        let close_requested = self.closable && (close_clicked || response.should_close());
        if close_requested {
            *open = false;
        }

        Some(ModalResponse {
            inner: response.inner,
            close_requested,
        })
    }
}

/// Response from showing a Modal
#[derive(Debug)]
pub struct ModalResponse<R> {
    /// The result of the content function
    pub inner: R,
    /// Whether the modal was requested to close
    pub close_requested: bool,
}

/// A simple modal that manages its own open state
#[derive(Debug)]
pub struct SimpleModal {
    modal: Modal,
    open: bool,
}

impl SimpleModal {
    /// Create a new simple modal
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, title: impl Into<String>) -> Self {
        Self {
            modal: Modal::new(id, title),
            open: false,
        }
    }

    /// Set the width
    pub fn width(mut self, width: f32) -> Self {
        self.modal = self.modal.width(width);
        self
    }

    /// Set closable
    pub fn closable(mut self, closable: bool) -> Self {
        self.modal = self.modal.closable(closable);
        self
    }

    /// Open the modal
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Close the modal
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Check if the modal is open
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Show the modal
    pub fn show<R>(
        &mut self,
        ctx: &Context,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> Option<ModalResponse<R>> {
        // Clone modal config to avoid borrowing issues
        let modal = Modal::new(self.modal.id, &self.modal.title)
            .width(
                self.modal
                    .width
                    .unwrap_or(DESIGN_TOKENS.sizing.dialog.default_width),
            )
            .closable(self.modal.closable)
            .dim_background(self.modal.dim_background);

        modal.show(ctx, &mut self.open, add_contents)
    }
}

/// Helper function to show a centered modal with standard styling
pub fn show_modal<R>(
    ctx: &Context,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: impl Into<String>,
    open: &mut bool,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<ModalResponse<R>> {
    Modal::new(id, title).show(ctx, open, add_contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, RawInput, Rect, ScrollArea, Vec2};

    #[test]
    fn scrolling_modals_follow_the_window_and_shrinking_content() {
        for fixed in [false, true] {
            let ctx = Context::default();
            let mut open = true;
            let frame = |screen_height: f32, rows: usize, open: &mut bool| {
                let mut area = Id::NULL;
                let mut title_top = 0.0;
                let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, screen_height));
                for _ in 0..3 {
                    ctx.run_ui(
                        RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        },
                        |ui| {
                            let modal = Modal::new("scroll", "Title").width(500.0);
                            let modal = if fixed { modal.height(800.0) } else { modal };
                            modal.show(ui.ctx(), open, |ui| {
                                area = ui.layer_id().id;
                                title_top = ui.min_rect().top();
                                ScrollArea::vertical()
                                    .max_height(ui.available_height())
                                    .show(ui, |ui| {
                                        for i in 0..rows {
                                            ui.label(format!("Row {i}"));
                                        }
                                    });
                            });
                        },
                    )
                    .drop_without_applying_deltas();
                }
                let rect = ctx.memory(|m| m.area_rect(area)).unwrap();
                assert!(
                    rect.top() >= 23.0 && rect.bottom() <= screen_height - 23.0,
                    "fixed {fixed}: {rect:?}"
                );
                assert!(title_top >= 24.0, "title above the window");
                rect
            };
            let tall = frame(900.0, 100, &mut open);
            assert!(tall.height() >= 790.0, "fixed {fixed}: {tall:?}");
            let short = frame(450.0, 100, &mut open);
            assert!(short.height() <= 403.0);
            let small = frame(900.0, 3, &mut open);
            if !fixed {
                assert!(small.height() < 300.0, "{small:?}");
            }
        }
    }
    /// A draggable modal follows the pointer from its title row and never leaves the window.
    #[test]
    fn a_draggable_modal_follows_its_title() {
        let ctx = Context::default();
        let mut open = true;
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let area = std::cell::Cell::new(Id::NULL);
        let press = |pos: Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut frame = |events: Vec<egui::Event>| {
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    Modal::new("drag", "Title")
                        .width(400.0)
                        .draggable(true)
                        .show(ui.ctx(), &mut open, |ui| {
                            area.set(ui.layer_id().id);
                            ui.label("Body");
                        });
                },
            )
            .drop_without_applying_deltas();
        };
        frame(Vec::new());
        frame(Vec::new());
        let before = ctx.memory(|memory| memory.area_rect(area.get())).unwrap();
        let at = Pos2::new(before.center().x, before.top() + 30.0);
        frame(vec![egui::Event::PointerMoved(at), press(at, true)]);
        frame(vec![egui::Event::PointerMoved(at + Vec2::new(120.0, 90.0))]);
        frame(Vec::new());
        frame(Vec::new());
        let moved = ctx.memory(|memory| memory.area_rect(area.get())).unwrap();
        assert!(
            (moved.left() - before.left() - 120.0).abs() < 1.0
                && (moved.top() - before.top() - 90.0).abs() < 1.0,
            "{before:?} {moved:?}"
        );
        // The window edge stops it, however far the pointer goes past it.
        let at = Pos2::new(moved.center().x, moved.top() + 30.0);
        frame(vec![press(at, false), press(at, true)]);
        frame(vec![egui::Event::PointerMoved(Pos2::new(-4000.0, -4000.0))]);
        frame(Vec::new());
        let kept = ctx.memory(|memory| memory.area_rect(area.get())).unwrap();
        assert!(
            kept.left() >= 0.0 && kept.top() >= 0.0,
            "{moved:?} {kept:?}"
        );
    }
}
