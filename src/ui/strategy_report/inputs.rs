//! The input row shared by study settings and the strategy tester.

use crate::localization as l10n;
use crate::model::Timeframe;
use crate::studies::{InputKind, InputValue, StudyInput, StudySource};

/// The width of a value field. TradingView keeps them narrow and lets the label take the row, so
/// a long title shortens instead of pushing the field out.
pub(crate) const FIELD_WIDTH: f32 = 96.0;

/// The width a row reserves for the control of `kind`, or `None` when the control sizes itself,
/// as a choice or colour picker does.
pub(crate) fn control_width(kind: &InputKind) -> Option<f32> {
    match kind {
        InputKind::Length(..)
        | InputKind::Integer(..)
        | InputKind::Number(..)
        | InputKind::Symbol
        | InputKind::Text => Some(FIELD_WIDTH),
        // Two time fields with the gap between them.
        InputKind::Session => Some(2.0 * FIELD_WIDTH + 8.0),
        _ => None,
    }
}

pub(crate) fn show_input(
    ui: &mut egui::Ui,
    input: &mut StudyInput,
    sources: &[(StudySource, String)],
) -> bool {
    let previous = input.value.clone();
    let label = ui.label(l10n::text(ui.ctx(), &input.label));
    let response = show_input_control(ui, input, sources, Some(label.id));
    response.labelled_by(label.id);
    ui.end_row();
    previous != input.value
}

/// The widget of an input, without its label or grid row, for a row an editor lays out itself.
/// `label` names the fields a control splits into, such as a custom interval's seconds.
pub(crate) fn show_input_control(
    ui: &mut egui::Ui,
    input: &mut StudyInput,
    sources: &[(StudySource, String)],
    label: Option<egui::Id>,
) -> egui::Response {
    match (&input.kind, &mut input.value) {
        (InputKind::Length(min, max), InputValue::Length(value)) => ui.add_sized(
            [FIELD_WIDTH, ui.spacing().interact_size.y],
            egui::DragValue::new(value).range(*min..=*max),
        ),
        (InputKind::Integer(min, max), InputValue::Integer(value)) => ui.add_sized(
            [FIELD_WIDTH, ui.spacing().interact_size.y],
            egui::DragValue::new(value).range(*min..=*max),
        ),
        (InputKind::Number(min, max), InputValue::Number(value)) => ui.add_sized(
            [FIELD_WIDTH, ui.spacing().interact_size.y],
            egui::DragValue::new(value).speed(0.1).range(*min..=*max),
        ),
        (InputKind::Choice(choices), InputValue::Choice(value)) => {
            egui::ComboBox::from_id_salt(&input.id)
                .selected_text(l10n::text(
                    ui.ctx(),
                    choices.get(*value).map_or("", String::as_str),
                ))
                .show_ui(ui, |ui| {
                    for (index, label) in choices.iter().enumerate() {
                        ui.selectable_value(value, index, l10n::text(ui.ctx(), label));
                    }
                })
                .response
        }
        (InputKind::Toggle, InputValue::Toggle(value)) => ui.checkbox(value, ""),
        (InputKind::Source, InputValue::Source(value)) => {
            egui::ComboBox::from_id_salt(&input.id)
                .selected_text(l10n::text(
                    ui.ctx(),
                    match value {
                        crate::studies::StudySource::Price(source) => source.label().to_owned(),
                        crate::studies::StudySource::Study { .. } => sources
                            .iter()
                            .find(|(source, _)| source == value)
                            .map(|(_, label)| label.clone())
                            .unwrap_or_else(|| "Unavailable study".to_owned()),
                    },
                ))
                .show_ui(ui, |ui| {
                    for (source, label) in sources {
                        ui.selectable_value(value, *source, l10n::text(ui.ctx(), label));
                    }
                    for source in crate::model::PriceSource::all() {
                        ui.selectable_value(
                            value,
                            crate::studies::StudySource::Price(source),
                            l10n::text(ui.ctx(), source.label()),
                        );
                    }
                })
                .response
        }
        (InputKind::Color, InputValue::Color(value)) => ui.color_edit_button_srgba(value),
        (InputKind::Symbol, InputValue::Symbol(value)) => ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(FIELD_WIDTH)
                .hint_text(l10n::text(ui.ctx(), "Chart symbol")),
        ),
        (InputKind::Text, InputValue::Text(value)) => {
            ui.add(egui::TextEdit::singleline(value).desired_width(FIELD_WIDTH))
        }
        (InputKind::Session, InputValue::Text(value)) => session_fields(ui, value),
        (InputKind::Timeframe, InputValue::Timeframe(value)) => {
            let response = egui::ComboBox::from_id_salt(&input.id)
                .selected_text(l10n::text(
                    ui.ctx(),
                    value.map_or_else(|| "Chart interval".to_owned(), |tf| tf.to_string()),
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(value, None, l10n::text(ui.ctx(), "Chart interval"));
                    for tf in Timeframe::all() {
                        ui.selectable_value(value, Some(tf), l10n::text(ui.ctx(), tf.to_string()));
                    }
                    if ui
                        .selectable_label(
                            value.is_some_and(|tf| tf.is_custom()),
                            l10n::text(ui.ctx(), "Custom"),
                        )
                        .clicked()
                    {
                        *value = Some(Timeframe::Custom(std::num::NonZeroU64::new(60).unwrap()));
                    }
                })
                .response;
            if let Some(Timeframe::Custom(seconds)) = value {
                let mut count = seconds.get();
                let response = ui.add(
                    egui::DragValue::new(&mut count)
                        .range(1..=u64::MAX)
                        .suffix(l10n::text(ui.ctx(), " s")),
                );
                let response = match label {
                    Some(label) => response.labelled_by(label),
                    None => response,
                };
                if response.changed() {
                    *seconds = std::num::NonZeroU64::new(count).unwrap();
                }
            }
            response
        }
        _ => ui.label(l10n::text(ui.ctx(), "This input is unavailable.")),
    }
}

/// The start and end of a `HHMM-HHMM` or `HH:MM-HH:MM` session, or `None` for any other text,
/// such as one that names its days, which the row then edits as a single field.
fn session_times(text: &str) -> Option<(&str, &str)> {
    let (start, end) = text.split_once('-')?;
    let time = |part: &str| {
        let digits = part.replace(':', "");
        (0..=4).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
    };
    (time(start) && time(end)).then_some((start, end))
}

/// A session input as the two time fields TradingView shows, writing back the `HHMM-HHMM` text
/// the study reads.
fn session_fields(ui: &mut egui::Ui, value: &mut String) -> egui::Response {
    let Some((start, end)) = session_times(value) else {
        return ui.add(egui::TextEdit::singleline(value).desired_width(FIELD_WIDTH));
    };
    let (mut start, mut end) = (start.to_owned(), end.to_owned());
    let start_response = ui.add(
        egui::TextEdit::singleline(&mut start)
            .desired_width(FIELD_WIDTH)
            .hint_text(l10n::text(ui.ctx(), "Start")),
    );
    let end_response = ui.add(
        egui::TextEdit::singleline(&mut end)
            .desired_width(FIELD_WIDTH)
            .hint_text(l10n::text(ui.ctx(), "End")),
    );
    if start_response.changed() || end_response.changed() {
        *value = format!("{start}-{end}");
    }
    start_response.union(end_response)
}

#[cfg(test)]
mod tests {
    use super::session_times;

    #[test]
    fn a_session_splits_into_its_times_and_keeps_unusual_text_whole() {
        assert_eq!(session_times("0112-0212"), Some(("0112", "0212")));
        assert_eq!(session_times("09:30-16:00"), Some(("09:30", "16:00")));
        // Half-typed and empty times stay two fields, so they can be edited.
        assert_eq!(session_times("09-"), Some(("09", "")));
        assert_eq!(session_times("-"), Some(("", "")));
        // A session that names its days, and a value with no range, stay one field.
        assert_eq!(session_times("mon-fri:0930-1600"), None);
        assert_eq!(session_times("0112"), None);
    }
}
