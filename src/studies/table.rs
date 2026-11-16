//! Labelled study tables, independent of bar-indexed plots.

/// A cell in a study table.
#[derive(Clone, Debug, PartialEq)]
pub enum StudyCell {
    /// A row label or other text.
    Text(String),
    /// A percentage, coloured by sign; `None` displays an unavailable value.
    Percent(Option<f64>),
}

/// A labelled table drawn in the lower right of the study's pane.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudyTable {
    /// Caption above the column headings.
    pub title: String,
    /// One heading per column.
    pub columns: Vec<String>,
    /// Cells in each row, in column order.
    pub rows: Vec<Vec<StudyCell>>,
    /// Optional background colour per cell, aligned with `rows`; empty or `None` uses the theme.
    pub cell_bg: Vec<Vec<Option<egui::Color32>>>,
    /// Optional text colour per cell, aligned with `rows`; empty or `None` uses the theme.
    pub cell_text: Vec<Vec<Option<egui::Color32>>>,
    /// How each cell places its text, aligned with `rows`; missing cells read as left centred.
    pub cell_align: Vec<Vec<egui::Align2>>,
    /// The lines between cells, from a Pine table's `border_color`; `None` draws no grid.
    pub border_color: Option<egui::Color32>,
    /// The table's own fill, from a Pine table's `bgcolor`; `None` uses the theme.
    pub bg: Option<egui::Color32>,
    /// The pane corner or edge the table sits in.
    pub position: StudyTablePosition,
}

/// Where a study table sits in its pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum StudyTablePosition {
    /// The upper left corner.
    TopLeft,
    /// The top edge, centred.
    TopCenter,
    /// The upper right corner.
    TopRight,
    /// The left edge, centred.
    MiddleLeft,
    /// The centre of the pane.
    MiddleCenter,
    /// The right edge, centred.
    MiddleRight,
    /// The lower left corner.
    BottomLeft,
    /// The bottom edge, centred.
    BottomCenter,
    /// The lower right corner.
    #[default]
    BottomRight,
}

#[cfg(feature = "widget")]
fn rating_color(rating: &str) -> egui::Color32 {
    match rating {
        "Strong Buy" => super::palette::BLUE,
        "Buy" => egui::Color32::from_rgb(91, 156, 246),
        "Strong Sell" => super::palette::BRIGHT_RED,
        "Sell" => egui::Color32::from_rgb(239, 154, 154),
        _ => super::palette::DISABLED,
    }
}

#[cfg(feature = "widget")]
pub(crate) fn paint(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    table: &StudyTable,
    id: egui::Id,
) -> egui::Rect {
    use crate::theme::ThemeExt;
    use egui::{Align2, FontId, Rect, Stroke, StrokeKind, pos2, vec2};

    let rating_table = table.title == "Technical Ratings";
    let painter = ui.painter().clone();
    let theme = ui.active_theme();
    let font = FontId::proportional(11.0);
    let text = |cell: &StudyCell| match cell {
        StudyCell::Text(text) => text.clone(),
        StudyCell::Percent(Some(value)) if value.is_finite() => format!("{value:+.2}%"),
        StudyCell::Percent(_) => "N/A".into(),
    };
    let mut widths = vec![
        0.0;
        table
            .columns
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0))
    ];
    for (width, column) in widths.iter_mut().zip(&table.columns) {
        *width = painter
            .layout_no_wrap(column.clone(), font.clone(), theme.ui.text)
            .size()
            .x
            + 16.0;
    }
    for row in &table.rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = width.max(
                painter
                    .layout_no_wrap(text(cell), font.clone(), theme.ui.text)
                    .size()
                    .x
                    + 16.0,
            );
        }
    }
    let width = widths.iter().sum::<f32>();
    // A Pine table has no caption, while a built-in one is titled and keeps to half the pane
    // so it leaves the chart readable. A Pine table is sized by its script, so it may fill the
    // room the legend leaves.
    let caption = if table.title.is_empty() { 0.0 } else { 22.0 };
    // A Pine table declares no heading row; a built-in one names its columns instead.
    let headings = usize::from(!table.columns.is_empty());
    let height = 22.0 * (table.rows.len() + headings) as f32 + caption;
    let mut available = rect.shrink(8.0);
    available.min.y += 24.0;
    if !available.is_positive() || rect.height() < 44.0 {
        return Rect::from_min_size(rect.min, egui::Vec2::ZERO);
    }
    let title_width = if caption == 0.0 {
        0.0
    } else {
        painter
            .layout_no_wrap(table.title.clone(), font.clone(), theme.ui.text)
            .size()
            .x
            + 16.0
    };
    let limit = if caption == 0.0 {
        available.height()
    } else {
        rect.height() * 0.5
    };
    let size = vec2(
        width.max(title_width).min(available.width()),
        height.min(available.height()).min(limit),
    );
    let (align_x, align_y) = match table.position {
        StudyTablePosition::TopLeft => (0.0, 0.0),
        StudyTablePosition::TopCenter => (0.5, 0.0),
        StudyTablePosition::TopRight => (1.0, 0.0),
        StudyTablePosition::MiddleLeft => (0.0, 0.5),
        StudyTablePosition::MiddleCenter => (0.5, 0.5),
        StudyTablePosition::MiddleRight => (1.0, 0.5),
        StudyTablePosition::BottomLeft => (0.0, 1.0),
        StudyTablePosition::BottomCenter => (0.5, 1.0),
        StudyTablePosition::BottomRight => (1.0, 1.0),
    };
    let bounds = Rect::from_min_size(
        available.min
            + vec2(
                (available.width() - size.x) * align_x,
                (available.height() - size.y) * align_y,
            ),
        size,
    );
    let painter = painter.with_clip_rect(bounds.intersect(ui.clip_rect()));
    painter.rect_filled(
        bounds,
        3.0,
        table.bg.unwrap_or(theme.ui.panel_bg_floating),
    );
    let border = table
        .border_color
        .or((!table.title.is_empty()).then_some(theme.ui.border));
    if let Some(color) = border {
        painter.rect_stroke(
            bounds,
            3.0,
            Stroke::new(
                if rating_table { 2.0 } else { 1.0 },
                if rating_table {
                    egui::Color32::WHITE
                } else {
                    color
                },
            ),
            StrokeKind::Inside,
        );
    }
    if caption > 0.0 {
        painter.text(
            bounds.min + vec2(8.0, 11.0),
            Align2::LEFT_CENTER,
            &table.title,
            font.clone(),
            theme.ui.text,
        );
    }
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(Rect::from_min_max(bounds.min + vec2(0.0, caption), bounds.max)),
    );
    child.set_clip_rect(painter.clip_rect());
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            let (content, _) =
                ui.allocate_exact_size(vec2(width, height - caption), egui::Sense::hover());
            let painter = ui.painter();
            let mut y = content.top();
            let heading: Vec<StudyCell> = table
                .columns
                .iter()
                .cloned()
                .map(StudyCell::Text)
                .collect();
            let heading = (!heading.is_empty()).then_some(heading);
            for (row_index, row) in heading.iter().chain(table.rows.iter()).enumerate() {
                let mut x = content.left();
                // The heading row is synthesised above `rows`, so data colours shift by one.
                let data_row = row_index.checked_sub(headings);
                for (column, (cell, width)) in row.iter().zip(&widths).enumerate() {
                    let cell_rect = Rect::from_min_size(pos2(x, y), vec2(*width, 22.0));
                    let own = |colors: &Vec<Vec<Option<egui::Color32>>>| {
                        data_row
                            .and_then(|row| colors.get(row))
                            .and_then(|cells| cells.get(column))
                            .copied()
                            .flatten()
                    };
                    let bg = own(&table.cell_bg);
                    let ink = own(&table.cell_text);
                    let mut color = match cell {
                        _ if rating_table && row_index == 0 => {
                            painter.rect_filled(
                                cell_rect,
                                0.0,
                                egui::Color32::from_rgb(41, 91, 121),
                            );
                            egui::Color32::WHITE
                        }
                        _ if rating_table => {
                            let rating = match row.get(2) {
                                Some(StudyCell::Text(text)) => text.as_str(),
                                _ => "Neutral",
                            };
                            let fill = rating_color(rating);
                            painter.rect_filled(cell_rect, 0.0, fill);
                            painter.rect_stroke(
                                cell_rect,
                                0.0,
                                Stroke::new(1.0, egui::Color32::WHITE),
                                StrokeKind::Inside,
                            );
                            crate::theme::readable_color(
                                egui::Color32::WHITE,
                                egui::Color32::BLACK,
                                &[fill],
                                4.5,
                            )
                        }
                        StudyCell::Percent(Some(value)) if value.is_finite() => {
                            let color = if *value >= 0.0 {
                                egui::Color32::from_rgb(8, 153, 129)
                            } else {
                                egui::Color32::from_rgb(242, 55, 69)
                            };
                            let tint = color.gamma_multiply(
                                0.1 + if table.title == "Seasonality" {
                                    0.4
                                } else {
                                    0.2
                                } * (value.abs() / 10.0).min(1.0) as f32,
                            );
                            painter.rect_filled(
                                Rect::from_min_size(pos2(x, y), vec2(*width, 22.0)),
                                0.0,
                                tint,
                            );
                            crate::theme::readable_color(
                                color,
                                theme.ui.text,
                                &[theme.ui.panel_bg_floating.blend(tint)],
                                4.5,
                            )
                        }
                        _ if matches!(table.title.as_str(), "Performance" | "Seasonality") => {
                            let alpha = if matches!(cell, StudyCell::Percent(None)) {
                                if table.title == "Seasonality" {
                                    128
                                } else {
                                    26
                                }
                            } else {
                                51
                            };
                            painter.rect_filled(
                                cell_rect,
                                0.0,
                                egui::Color32::from_rgba_unmultiplied(120, 123, 134, alpha),
                            );
                            theme.ui.text
                        }
                        _ => theme.ui.text,
                    };
                    if let Some(bg) = bg {
                        painter.rect_filled(cell_rect, 0.0, bg);
                        color = theme.ui.text;
                    }
                    if let Some(ink) = ink {
                        color = ink;
                    }
                    let align = data_row
                        .and_then(|row| table.cell_align.get(row))
                        .and_then(|cells| cells.get(column))
                        .copied()
                        .unwrap_or(Align2::LEFT_CENTER);
                    painter.text(
                        align.pos_in_rect(&cell_rect.shrink2(vec2(8.0, 0.0))),
                        align,
                        text(cell),
                        font.clone(),
                        color,
                    );
                    x += width;
                }
                y += 22.0;
            }
            // shortcut: every grid line is one pixel, until a script asks for a wider border.
            if let Some(color) = table.border_color {
                let stroke = Stroke::new(1.0, color);
                let mut x = content.left();
                for width in widths.iter().take(widths.len().saturating_sub(1)) {
                    x += width;
                    painter.vline(x, content.y_range(), stroke);
                }
                for row in 1..(table.rows.len() + headings) {
                    painter.hline(content.x_range(), content.top() + 22.0 * row as f32, stroke);
                }
            }
        });
    bounds
}

/// Paints the tables of one or more studies into a pane: each table sits in the corner it asks
/// for, and tables asking for the same corner stack above one another. A table asking for another
/// corner gets the whole rect again, so a checklist in one corner does not push a watermark in
/// another up the pane.
#[cfg(feature = "widget")]
#[derive(Default)]
pub(crate) struct TableStack {
    corners: Vec<(StudyTablePosition, egui::Rect)>,
}

#[cfg(feature = "widget")]
impl TableStack {
    pub(crate) fn paint(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        tables: &[&StudyTable],
        salt: impl std::hash::Hash + std::fmt::Debug,
    ) -> Vec<egui::Rect> {
        tables
            .iter()
            .enumerate()
            .map(|(index, table)| {
                let slot = self
                    .corners
                    .iter()
                    .position(|(corner, _)| *corner == table.position)
                    .unwrap_or_else(|| {
                        self.corners.push((table.position, rect));
                        self.corners.len() - 1
                    });
                let cursor = &mut self.corners[slot].1;
                let bounds = paint(ui, *cursor, table, ui.scope_id().with((&salt, index)));
                // The next table in this corner goes above this one.
                cursor.max.y = bounds.top() - 8.0;
                bounds
            })
            .collect()
    }
}

#[cfg(all(test, feature = "widget"))]
mod tests {
    use super::*;

    /// A bottom-centre table sits at the bottom of the pane even when a taller table in another
    /// corner is painted first, while two tables in one corner still stack.
    #[test]
    fn tables_in_different_corners_do_not_stack() {
        use egui::{RawInput, Rect, Shape, pos2, vec2};

        let table = |text: &str, rows: usize, position| StudyTable {
            columns: vec![String::new()],
            rows: (0..rows)
                .map(|row| vec![StudyCell::Text(format!("{text} {row}"))])
                .collect(),
            position,
            ..StudyTable::default()
        };
        let checklist = table("checklist", 8, StudyTablePosition::BottomRight);
        let watermark = table("watermark", 2, StudyTablePosition::BottomCenter);
        let stacked = table("stacked", 2, StudyTablePosition::BottomRight);
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
        let ctx = egui::Context::default();
        let output = ctx.run_ui(
            RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ui| {
                TableStack::default().paint(
                    ui,
                    rect,
                    &[&checklist, &watermark, &stacked],
                    "tables",
                );
            },
        );
        let bottom = |text: &str| {
            output
                .shapes
                .iter()
                .flat_map(|clipped| match &clipped.shape {
                    Shape::Text(shape) if shape.galley.text() == text => Some(shape.pos.y),
                    _ => None,
                })
                .fold(f32::MIN, f32::max)
        };
        let watermark = bottom("watermark 1");
        assert!(
            watermark > rect.bottom() - 40.0,
            "the watermark must stay at the pane bottom: {watermark}"
        );
        assert!(
            bottom("stacked 1") < bottom("checklist 4"),
            "tables in one corner must stack: {} vs {}",
            bottom("stacked 1"),
            bottom("checklist 4")
        );
        output.drop_without_applying_deltas();
    }

    /// A narrow or short pane keeps overflowing columns and years within a scrollable table.
    #[test]
    fn overflowing_tables_fit_the_pane() {
        use egui::{Event, MouseWheelUnit, RawInput, Rect, Shape, TouchPhase, vec2};

        let table = StudyTable {
            title: "Seasonality".into(),
            columns: (0..13).map(|i| format!("Column {i}")).collect(),
            rows: (0..40)
                .map(|i| {
                    let mut row = vec![StudyCell::Percent(Some(12.34)); 13];
                    row[12] = StudyCell::Text(format!("Last {i}"));
                    row
                })
                .collect(),
            ..StudyTable::default()
        };
        for (_, theme) in crate::theme::presets() {
            for width in [240.0, 800.0] {
                let ctx = egui::Context::default();
                crate::theme::apply_to_egui(&ctx, &theme);
                let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 300.0));
                for frame in 0..20 {
                    let mut events =
                        vec![Event::PointerMoved(rect.right_bottom() - vec2(100.0, 50.0))];
                    if frame == 1 {
                        events.push(Event::MouseWheel {
                            unit: MouseWheelUnit::Point,
                            delta: vec2(-2000.0, -2000.0),
                            phase: TouchPhase::Move,
                            source: egui::MouseWheelSource::Unknown,
                            modifiers: Default::default(),
                        });
                    }
                    let output = ctx.run_ui(
                        RawInput {
                            screen_rect: Some(rect),
                            time: Some(frame as f64 / 60.0),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            let bounds = paint(ui, rect, &table, ui.scope_id().with("table"));
                            assert!(rect.contains_rect(bounds), "{bounds:?} outside {rect:?}");
                        },
                    );
                    if frame == 19 {
                        assert!(output.shapes.iter().any(|clipped| matches!(&clipped.shape,
                            Shape::Text(text) if text.galley.text() == "Last 39"
                                && clipped.clip_rect.contains_rect(Rect::from_min_size(text.pos, text.galley.size()))
                        )), "the last column of the last year must be reachable at width {width}");
                    }
                    output.drop_without_applying_deltas();
                }
            }
        }
    }

    /// A Pine table has no caption row, and one that fits its pane shows every row at once
    /// rather than clipping the last one behind a scroll bar.
    #[test]
    fn an_uncaptioned_table_shows_every_row_it_declares() {
        use egui::{RawInput, Rect, Shape, pos2, vec2};

        let table = StudyTable {
            columns: ["Timeframe".into(), "Trend".into()].into(),
            rows: (0..10)
                .map(|row| vec![StudyCell::Text(format!("{row}m")), StudyCell::Text("UP".into())])
                .collect(),
            ..StudyTable::default()
        };
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
        let output = ctx.run_ui(
            RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ui| {
                paint(ui, rect, &table, ui.scope_id().with("table"));
            },
        );
        let last = output.shapes.iter().find_map(|clipped| match &clipped.shape {
            Shape::Text(text) if text.galley.text() == "9m" => Some((clipped, text)),
            _ => None,
        });
        let Some((clipped, text)) = last else {
            panic!("the last row must be painted");
        };
        assert!(
            clipped
                .clip_rect
                .contains_rect(Rect::from_min_size(text.pos, text.galley.size())),
            "the last row must not be clipped: {:?}",
            clipped.clip_rect
        );
        output.drop_without_applying_deltas();
    }
}
