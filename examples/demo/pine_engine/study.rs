//! A parsed Pine script as a chart study: its plots become lines, `fill` becomes a band and
//! `plotshape` becomes markers.

use egui::Color32;
use egui_charts::model::{Bar, Instrument, Timeframe};
use egui_charts::studies::{
    CrossDirection, Indicator, IndicatorMarker, IndicatorValues, InputKind, InputValue, PlotType,
    StudyError, StudyFill, StudyInput,
};

use crate::pine_engine::Script;
use crate::pine_engine::ast::InputDecl;
use crate::pine_engine::runtime::Context;
use crate::pine_engine::value::Value;

/// A Pine script that the chart calculates and draws.
#[derive(Clone)]
pub struct PineStudy {
    script: Script,
    context: Context,
    name: String,
    overlay: bool,
    values: IndicatorValues,
    colors: Vec<Color32>,
    line_names: Vec<String>,
    plot_types: Vec<PlotType>,
    widths: Vec<f32>,
    offsets: Vec<isize>,
    per_bar_colors: Vec<Vec<Option<Color32>>>,
    fills: Vec<StudyFill>,
    markers: Vec<IndicatorMarker>,
    inputs: Vec<StudyInput>,
    visible: bool,
}

impl PineStudy {
    /// Parses `source` as a study, or describes the syntax it cannot read.
    pub fn new(source: &str) -> Result<Self, String> {
        let script = Script::parse(source)?;
        let name = script.program().title.clone();
        let overlay = script.program().overlay;
        let inputs = script.inputs().iter().map(study_input).collect();
        Ok(Self {
            script,
            context: Context::default(),
            name,
            overlay,
            values: IndicatorValues::new(),
            colors: Vec::new(),
            line_names: Vec::new(),
            plot_types: Vec::new(),
            widths: Vec::new(),
            offsets: Vec::new(),
            per_bar_colors: Vec::new(),
            fills: Vec::new(),
            markers: Vec::new(),
            inputs,
            visible: true,
        })
    }

    /// What the last run could not do, with source lines, for the editor.
    pub fn diagnostics(&self) -> &[crate::pine_engine::ast::Diagnostic] {
        self.script.diagnostics()
    }
}

impl Indicator for PineStudy {
    fn name(&self) -> &str {
        &self.name
    }

    fn calculate(&mut self, data: &[Bar]) {
        let output = self.script.run(data, &self.context);
        self.values = IndicatorValues::new();
        let mut row = vec![f64::NAN; output.plots.len()];
        for bar in 0..data.len() {
            for (line, plot) in output.plots.iter().enumerate() {
                row[line] = plot.values.get(bar).copied().unwrap_or(f64::NAN);
            }
            self.values.push_lines(&row);
        }
        self.line_names = output.plots.iter().map(|plot| plot.title.clone()).collect();
        self.colors = output.plots.iter().map(|plot| plot.color).collect();
        self.plot_types = output.plots.iter().map(|plot| plot.plot_type).collect();
        self.widths = output.plots.iter().map(|plot| plot.width).collect();
        self.offsets = output.plots.iter().map(|plot| plot.offset).collect();
        self.per_bar_colors = output
            .plots
            .iter()
            .map(|plot| plot.colors.clone())
            .collect();
        self.fills = output.fills;
        self.markers = output
            .shapes
            .iter()
            .map(|shape| IndicatorMarker {
                bar: shape.bar,
                value: shape.value,
                direction: if shape.above {
                    CrossDirection::Down
                } else {
                    CrossDirection::Up
                },
            })
            .collect();
    }

    fn values(&self) -> &IndicatorValues {
        &self.values
    }

    fn inputs(&self) -> Vec<StudyInput> {
        self.inputs.clone()
    }

    fn set_input(&mut self, id: &str, value: InputValue) -> Result<(), StudyError> {
        let Some(index) = self.inputs.iter().position(|input| input.id == id) else {
            return Err(StudyError::UnknownInput(id.into()));
        };
        if !self.inputs[index].accepts(&value) {
            return Err(StudyError::InvalidInput {
                id: id.to_owned(),
                reason: "is outside its accepted values".into(),
            });
        }
        let Some(decl) = self
            .script
            .inputs()
            .iter()
            .find(|decl| decl.id.to_string() == id)
            .cloned()
        else {
            return Err(StudyError::UnknownInput(id.into()));
        };
        self.script.set_input(decl.id, pine_value(&decl, &value));
        self.inputs[index].value = value;
        self.values.clear();
        Ok(())
    }

    fn colors(&self) -> Vec<Color32> {
        self.colors.clone()
    }

    fn set_colors(&mut self, colors: Vec<Color32>) {
        if !colors.is_empty() {
            self.colors = colors;
        }
    }

    fn plot_style(&self, line: usize) -> egui_charts::studies::PlotStyle {
        egui_charts::studies::PlotStyle {
            color: self.colors.get(line).copied().unwrap_or(Color32::WHITE),
            width: self.widths.get(line).copied().unwrap_or(1.0),
            pattern: egui_charts::model::LineStyle::Solid,
            plot_type: self.plot_types.get(line).copied().unwrap_or_default(),
            visible: true,
        }
    }

    fn plot_offset(&self, line: usize) -> isize {
        self.offsets.get(line).copied().unwrap_or(0)
    }

    fn plot_color(&self, line: usize, bar: usize) -> Option<Color32> {
        self.per_bar_colors
            .get(line)
            .and_then(|colors| colors.get(bar))
            .copied()
            .flatten()
    }

    fn fills(&self) -> Vec<StudyFill> {
        self.fills.clone()
    }

    fn markers(&self) -> &[IndicatorMarker] {
        &self.markers
    }

    fn line_names(&self) -> Vec<String> {
        self.line_names.clone()
    }

    fn line_count(&self) -> usize {
        self.line_names.len()
    }

    fn is_overlay(&self) -> bool {
        self.overlay
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    fn set_timeframe(&mut self, timeframe: Timeframe) {
        self.context.timeframe = timeframe;
    }

    fn set_instrument(&mut self, instrument: Option<&Instrument>) -> bool {
        let symbol = instrument
            .map(|instrument| instrument.symbol.clone())
            .unwrap_or_default();
        let changed = symbol != self.context.symbol;
        self.context.symbol = symbol;
        changed
    }

    fn clone_box(&self) -> Box<dyn Indicator> {
        Box::new(self.clone())
    }
}

/// The library schema of one `input.*` call.
fn study_input(decl: &InputDecl) -> StudyInput {
    let id = decl.id.to_string();
    let (value, kind) = match (&decl.kind, &decl.default) {
        (crate::pine_engine::ast::InputKind::Integer { min, max }, default) => {
            let value = default.number().unwrap_or(0.0) as i32;
            let min = min.map(|min| min as i32).unwrap_or(i32::MIN);
            let max = max.map(|max| max as i32).unwrap_or(i32::MAX);
            (InputValue::Integer(value), InputKind::Integer(min, max))
        }
        (crate::pine_engine::ast::InputKind::Number { min, max }, default) => {
            let value = default.number().unwrap_or(0.0);
            let min = min.unwrap_or(-1e300);
            let max = max.unwrap_or(1e300);
            (InputValue::Number(value), InputKind::Number(min, max))
        }
        (crate::pine_engine::ast::InputKind::Toggle, default) => (
            InputValue::Toggle(default.flag().unwrap_or(false)),
            InputKind::Toggle,
        ),
        (crate::pine_engine::ast::InputKind::Color, default) => (
            InputValue::Color(default.color().unwrap_or(Color32::WHITE)),
            InputKind::Color,
        ),
        (crate::pine_engine::ast::InputKind::Timeframe, _) => {
            (InputValue::Timeframe(None), InputKind::Timeframe)
        }
        (crate::pine_engine::ast::InputKind::Symbol, _) => {
            (InputValue::Symbol(String::new()), InputKind::Symbol)
        }
        // The library has no session input, so a session is edited as its text.
        (crate::pine_engine::ast::InputKind::Session, default) => (
            InputValue::Text(default.text().unwrap_or_default().to_owned()),
            InputKind::Text,
        ),
        (crate::pine_engine::ast::InputKind::Choice(choices), default) => {
            let index = default
                .text()
                .and_then(|text| choices.iter().position(|choice| choice == text))
                .unwrap_or(0);
            (
                InputValue::Choice(index),
                InputKind::Choice(choices.clone()),
            )
        }
        (crate::pine_engine::ast::InputKind::Text, default) => (
            InputValue::Text(default.text().unwrap_or_default().to_owned()),
            InputKind::Text,
        ),
    };
    StudyInput::new(id, decl.title.clone(), value, kind)
}

/// The Pine value an edited input holds.
fn pine_value(decl: &InputDecl, value: &InputValue) -> Value {
    match value {
        InputValue::Length(length) => Value::Num(*length as f64),
        InputValue::Integer(integer) => Value::Num(f64::from(*integer)),
        InputValue::Number(number) => Value::Num(*number),
        InputValue::Toggle(flag) => Value::Bool(*flag),
        InputValue::Color(color) => Value::Color(*color),
        InputValue::Choice(index) => match &decl.kind {
            crate::pine_engine::ast::InputKind::Choice(choices) => choices
                .get(*index)
                .map(|choice| Value::Str(choice.clone()))
                .unwrap_or(Value::Na),
            _ => Value::Num(*index as f64),
        },
        InputValue::Timeframe(timeframe) => timeframe
            .map(|timeframe| Value::Str(timeframe.as_str().into_owned()))
            .unwrap_or(Value::Na),
        InputValue::Symbol(symbol) => Value::Str(symbol.clone()),
        InputValue::Text(text) => Value::Str(text.clone()),
        _ => Value::Na,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(at, close)| {
                Bar::new(
                    DateTime::UNIX_EPOCH + chrono::Duration::minutes(at as i64),
                    *close,
                    close + 1.0,
                    close - 1.0,
                    *close,
                    1.0,
                )
            })
            .collect()
    }

    const TRIPLE_EMA: &str = "//@version=6\nindicator(\"Tripple EMA\", overlay=true)\nfast = input.int(3, \"Fast\")\nslow = input.int(5, \"Slow\")\na = ta.ema(close, fast)\nb = ta.ema(close, slow)\np1 = plot(a, \"Fast\", color=color.new(color.red, 40))\np2 = plot(b, \"Slow\", color=color.blue)\nfill(p1, p2, color=color.new(color.green, 90))\n";

    #[test]
    fn three_ema_plots_lines_a_fill_and_editable_inputs() {
        let mut study = PineStudy::new(TRIPLE_EMA).unwrap();
        assert!(study.is_overlay());
        assert_eq!(study.name(), "Tripple EMA");
        let data = bars(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        study.calculate(&data);
        assert_eq!(study.line_count(), 2);
        assert_eq!(study.line_names(), ["Fast", "Slow"]);
        assert!(study.values().value(0, 0).is_none());
        assert!(study.values().value(1, 4).is_some());
        assert_eq!(study.fills().len(), 1);
        assert_eq!(study.inputs().len(), 2);
        let fast = study.inputs()[0].id.clone();
        study
            .set_input(&fast, InputValue::Integer(4))
            .expect("the input accepts a length");
        study.calculate(&data);
        assert!(study.values().value(0, 3).is_some());
        assert_eq!(study.inputs()[0].default, InputValue::Integer(3));
    }

    #[test]
    fn a_wrong_input_type_is_rejected() {
        let mut study = PineStudy::new(TRIPLE_EMA).unwrap();
        let fast = study.inputs()[0].id.clone();
        assert!(
            study
                .set_input(&fast, InputValue::Text("x".into()))
                .is_err()
        );
    }

    #[test]
    fn plotshape_becomes_markers() {
        let mut study = PineStudy::new(
            "//@version=4\nstudy(\"Fractals\", overlay=true)\nn = input(title=\"Periods\", defval=2, type=input.integer)\ndn = high[2] > high[1] and high[2] > high[0]\nplotshape(dn, location=location.abovebar, color=color.white)\n",
        )
        .unwrap();
        let data = bars(&[1.0, 2.0, 5.0, 2.0, 1.0]);
        study.calculate(&data);
        assert!(!study.markers().is_empty());
    }
}
