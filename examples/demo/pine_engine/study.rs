//! A parsed Pine script as a chart study: its plots become lines, `fill` becomes a band and
//! `plotshape` becomes text labels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::Color32;
use egui_charts::drawings::{ChartPoint, Drawing, DrawingToolType, TextAnchor};
use egui_charts::model::{Bar, Instrument, Timeframe};
use egui_charts::studies::{
    Indicator, IndicatorMarker, IndicatorValues, InputKind, InputValue, PlotType, StudyCandle,
    StudyData, StudyDataRequest, StudyError, StudyFill, StudyInput, StudyTable,
};

use crate::pine_engine::Script;
use crate::pine_engine::ast::InputDecl;
use crate::pine_engine::runtime::{
    Context, DrawingKind, DrawingOutput, RunOutput, SecurityData, ShapeMark,
};
use crate::pine_engine::value::Value;

/// A script run for one state of the bars.
#[derive(Clone)]
struct RunTask {
    /// The finished run, once its thread has put it here.
    result: Arc<Mutex<Option<RunOutput>>>,
    /// The script, context and bars the run was given.
    version: Version,
    /// Set when the run's bars are gone, so it stops instead of finishing for nothing.
    cancel: Arc<AtomicBool>,
}

/// What a run was calculated from, so a finished run is only applied to the same state.
type Version = (u64, usize, Option<i64>, Option<i64>);

/// The script with a calculation running on a thread of its own.
///
/// A run over a long history takes seconds, and the chart asks for it in the middle of a frame:
/// a pan that pages older bars in would stop the window. The chart keeps the previous picture
/// while the thread works and collects the result through
/// [`update_ready`](Indicator::update_ready) on a later frame.
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
    patterns: Vec<egui_charts::model::LineStyle>,
    plot_visible: Vec<bool>,
    widths: Vec<f32>,
    offsets: Vec<isize>,
    per_bar_colors: Vec<Vec<Option<Color32>>>,
    candles: Vec<StudyCandle>,
    fills: Vec<StudyFill>,
    drawings: Vec<Drawing>,
    tables: Vec<StudyTable>,
    /// Histories the last run read for `request.security` on other series.
    sources: Vec<SecurityData>,
    /// Histories the script asked the chart for besides its own series, as `(symbol, interval)`.
    requests: Vec<(String, Timeframe)>,
    inputs: Vec<StudyInput>,
    visible: bool,
    /// Where the next frame is drawn: a finished run asks it to repaint so it is collected.
    ctx: Option<egui::Context>,
    task: Option<RunTask>,
    /// The bars the drawn values belong to, so a run for other bars clears them and a run for
    /// these keeps the picture until it lands.
    stored: Option<Version>,
    /// The last bar of that picture, which tells an added bar from bars replaced by others.
    stored_tail: Option<Bar>,
    /// Counts edits of the script and of its chart context, so a run from before one is dropped.
    generation: u64,
    /// Whether the next update must finish in the calling thread, for export and snapshots.
    settle: bool,
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
            patterns: Vec::new(),
            plot_visible: Vec::new(),
            widths: Vec::new(),
            offsets: Vec::new(),
            per_bar_colors: Vec::new(),
            candles: Vec::new(),
            fills: Vec::new(),
            drawings: Vec::new(),
            tables: Vec::new(),
            sources: Vec::new(),
            requests: Vec::new(),
            inputs,
            visible: true,
            ctx: None,
            task: None,
            stored: None,
            stored_tail: None,
            generation: 0,
            settle: false,
        })
    }

    /// Draws the finished runs and asks for frames on `ctx`; without it every run is finished
    /// in the calling thread, as before.
    pub fn set_context(&mut self, ctx: Option<egui::Context>) {
        self.ctx = ctx;
    }

    /// Takes `output`, a run over `bars` bars, as the values and drawings to draw.
    fn store(&mut self, output: RunOutput, bars: usize) {
        self.values = IndicatorValues::new();
        let mut row = vec![f64::NAN; output.plots.len()];
        for bar in 0..bars {
            for (line, plot) in output.plots.iter().enumerate() {
                row[line] = plot.values.get(bar).copied().unwrap_or(f64::NAN);
            }
            self.values.push_lines(&row);
        }
        self.line_names = output.plots.iter().map(|plot| plot.title.clone()).collect();
        self.colors = output.plots.iter().map(|plot| plot.color).collect();
        self.plot_types = output.plots.iter().map(|plot| plot.plot_type).collect();
        self.patterns = output.plots.iter().map(|plot| plot.pattern).collect();
        self.plot_visible = output.plots.iter().map(|plot| plot.visible).collect();
        self.widths = output.plots.iter().map(|plot| plot.width).collect();
        self.offsets = output.plots.iter().map(|plot| plot.offset).collect();
        self.per_bar_colors = output
            .plots
            .iter()
            .map(|plot| plot.colors.clone())
            .collect();
        self.fills = output.fills;
        self.candles = output
            .candles
            .iter()
            .map(|candle| StudyCandle {
                bars: candle.bar..candle.bar + 1,
                bar: candle.candle.clone(),
                plot: candle.plot,
                color: candle.color,
                bar_style: candle.bar_style,
            })
            .collect();
        self.drawings = output.drawings.iter().map(study_drawing).collect();
        // The chart paints no study markers, so each `plotshape` mark becomes a text label with
        // the shape's glyph or the script's `text=`.
        self.drawings.extend(
            output
                .shapes
                .iter()
                .filter(|shape| !shape.text.is_empty())
                .map(study_shape),
        );
        self.tables = output.tables;
        self.requests = output.security_requests;
    }

    /// Drops the outputs that belong to bars this run does not have, keeping the styles.
    fn clear_output(&mut self) {
        self.stored = None;
        self.stored_tail = None;
        self.values.clear();
        self.per_bar_colors.clear();
        self.candles.clear();
        self.fills.clear();
        self.drawings.clear();
        self.tables.clear();
    }

    /// Whether the drawn picture still belongs to `data`: the same script and first bar, with
    /// no bars lost, and either the same count or the last drawn bar still the same one. A new
    /// bar or a tick then keeps the picture until its run lands instead of blanking the chart.
    fn keeps_picture(&self, version: Version, data: &[Bar]) -> bool {
        let (Some(stored), Some(tail)) = (self.stored, self.stored_tail.as_ref()) else {
            return false;
        };
        if stored.0 != version.0 || stored.2 != version.2 || stored.1 > version.1 {
            return false;
        }
        // A tick updates the last drawn bar in place; an added bar leaves it as it was drawn.
        stored.1 == version.1 || data.get(stored.1.saturating_sub(1)) == Some(tail)
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
        let output = self.script.run(data, &self.context, &self.sources);
        self.store(output, data.len());
        self.stored = Some(state_of(self.generation, data));
        self.stored_tail = data.last().cloned();
    }

    fn values(&self) -> &IndicatorValues {
        &self.values
    }

    fn data_requests(&self) -> Vec<StudyDataRequest> {
        self.requests
            .iter()
            .map(|(symbol, timeframe)| StudyDataRequest {
                symbol: symbol.clone(),
                timeframe: Some(*timeframe),
                metric: None,
            })
            .collect()
    }

    fn update_data(&mut self, data: &[Bar], _from: usize, sources: &[StudyData<'_>]) {
        if data.is_empty() {
            // The chart drops the values of bars it no longer has. The styles and drawings stay
            // until the next finished run replaces them, and so do values for bars it still has.
            if !self.update_ready() {
                self.values.clear();
            }
            return;
        }
        let version = state_of(self.generation, data);
        if self.settle {
            // Export, a snapshot or an undo step reads the values at once.
            self.settle = false;
            if let Some(task) = self.task.take() {
                task.cancel.store(true, Ordering::Relaxed);
            }
            self.sources = security_sources(sources);
            self.calculate(data);
            return;
        }
        if let Some(task) = self.task.take() {
            let output = take_result(&task.result);
            match output {
                Some(output) if task.version == version => {
                    self.store(output, data.len());
                    self.stored = Some(version);
                    self.stored_tail = data.last().cloned();
                    return;
                }
                // A run for other bars is stale; the newer bars below start another.
                Some(_) => {}
                // The same bars: the running run is the one wanted, and the picture stays
                // until it lands.
                None if task.version == version => {
                    self.task = Some(task);
                    if !self.keeps_picture(version, data) {
                        self.clear_output();
                    }
                    return;
                }
                // The bars moved on, as when a drag pages older ones in: stop this run and
                // start the one these bars need instead of waiting for a result to discard.
                None => task.cancel.store(true, Ordering::Relaxed),
            }
        }
        let Some(ctx) = self.ctx.clone() else {
            self.sources = security_sources(sources);
            self.calculate(data);
            return;
        };
        // Values of other bars must not stay on these: a prepend moves every one of them. Values
        // for these bars are the picture the run replaces, so they stay while it runs.
        if !self.keeps_picture(version, data) {
            self.clear_output();
        }
        let sources = security_sources(sources);
        let script = self.script.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut context = self.context.clone();
        context.cancel = Some(Arc::clone(&cancel));
        let bars = data.to_vec();
        let result: Arc<Mutex<Option<RunOutput>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&result);
        let finished = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let output = script.run(&bars, &context, &sources);
            if !finished.load(Ordering::Relaxed) {
                *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(output);
            }
            ctx.request_repaint();
        });
        self.task = Some(RunTask {
            result,
            version,
            cancel,
        });
    }

    fn update_ready(&mut self) -> bool {
        self.task.as_ref().is_some_and(|task| {
            task.result
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some()
        })
    }

    fn settle(&mut self) {
        self.settle = true;
    }

    fn update(&mut self, data: &[Bar], from: usize) {
        if data.is_empty() {
            // The chart blanks a study whose values belong to other bars. A finished run already
            // holds the values for these bars, so the lines keep their picture until it lands.
            if !self.update_ready() {
                self.values.clear();
            }
            return;
        }
        self.update_data(data, from, &[]);
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
        self.generation += 1;
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
            pattern: self.patterns.get(line).copied().unwrap_or_default(),
            plot_type: self.plot_types.get(line).copied().unwrap_or_default(),
            visible: self.plot_visible.get(line).copied().unwrap_or(true),
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

    fn candles(&self) -> &[StudyCandle] {
        &self.candles
    }

    fn table(&self) -> Option<&StudyTable> {
        self.tables.first()
    }

    fn tables(&self) -> Vec<&StudyTable> {
        self.tables.iter().collect()
    }

    fn markers(&self) -> &[IndicatorMarker] {
        // `plotshape` marks are drawn as the glyph labels in `drawings`; the chart's own marker
        // triangles would draw a second, differently sized and coloured set.
        &[]
    }

    fn drawings(&self) -> &[Drawing] {
        &self.drawings
    }

    fn keeps_drawing_style(&self) -> bool {
        true
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
        if self.context.timeframe != timeframe {
            self.generation += 1;
        }
        self.context.timeframe = timeframe;
    }

    fn set_instrument(&mut self, instrument: Option<&Instrument>) -> bool {
        let symbol = instrument
            .map(|instrument| instrument.symbol.clone())
            .unwrap_or_default();
        let mintick = instrument
            .and_then(|instrument| instrument.tick_size)
            .filter(|tick| *tick > 0.0)
            .unwrap_or(0.01);
        let exchange = instrument
            .and_then(|instrument| instrument.exchange.clone())
            .unwrap_or_default();
        let root = instrument
            .and_then(|instrument| instrument.root.clone())
            .unwrap_or_else(|| symbol.clone());
        let symbol_type = instrument
            .and_then(|instrument| instrument.symbol_type)
            .map(|kind| kind.to_string())
            .unwrap_or_default();
        let changed = symbol != self.context.symbol
            || mintick != self.context.mintick
            || exchange != self.context.exchange
            || root != self.context.root
            || symbol_type != self.context.symbol_type;
        self.context.symbol = symbol;
        self.context.mintick = mintick;
        self.context.exchange = exchange;
        self.context.root = root;
        self.context.symbol_type = symbol_type;
        if changed {
            self.generation += 1;
        }
        changed
    }

    fn clone_box(&self) -> Box<dyn Indicator> {
        Box::new(self.clone())
    }
}

/// The histories a run reads besides its own bars.
fn security_sources(sources: &[StudyData<'_>]) -> Vec<SecurityData> {
    sources
        .iter()
        .map(|source| SecurityData {
            symbol: source.request.symbol.clone(),
            timeframe: source.timeframe,
            bars: source.bars.to_vec(),
        })
        .collect()
}

/// What a run was calculated from: the script and context generation and the ends of the bars.
fn state_of(generation: u64, bars: &[Bar]) -> Version {
    (
        generation,
        bars.len(),
        bars.first().map(|bar| bar.time.timestamp_millis()),
        bars.last().map(|bar| bar.time.timestamp_millis()),
    )
}

/// The finished output of `result`, if its thread has put one there.
fn take_result(result: &Arc<Mutex<Option<RunOutput>>>) -> Option<RunOutput> {
    result
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
}

/// One Pine drawing as a chart drawing: a trend line, a text label or a rectangle.
fn study_drawing(output: &DrawingOutput) -> Drawing {
    let (tool, points) = match output.kind {
        DrawingKind::Line => (
            DrawingToolType::TrendLine,
            vec![output.first, output.second],
        ),
        DrawingKind::Label => (DrawingToolType::TextLabel, vec![output.first]),
        DrawingKind::Box => (DrawingToolType::Rect, vec![output.first, output.second]),
        // A `linefill` is the polygon through both lines' ends.
        DrawingKind::Fill => {
            let mut points = vec![output.first, output.second];
            points.extend(output.quad.into_iter().flatten());
            (DrawingToolType::Polyline, points)
        }
    };
    let mut drawing = Drawing::new(0, tool);
    drawing.chart_points = points
        .into_iter()
        .map(|(bar, price)| ChartPoint::new(bar, price))
        .collect();
    drawing.completed = true;
    drawing.closed = output.kind == DrawingKind::Fill;
    drawing.color = output.color.to_srgba_unmultiplied();
    drawing.stroke_width = output.width;
    drawing.line_style = output.style;
    drawing.font_size = output.font_size;
    drawing.fill_color = output.fill_color.map(|color| color.to_srgba_unmultiplied());
    drawing.text_color = output.text_color.map(|color| color.to_srgba_unmultiplied());
    drawing.text_anchor = output.text_anchor;
    if !output.text.is_empty() {
        drawing.text = Some(output.text.clone());
    }
    drawing
}

/// A `plotshape` mark as a text label at its bar and price.
fn study_shape(mark: &ShapeMark) -> Drawing {
    let mut drawing = Drawing::new(0, DrawingToolType::TextLabel);
    drawing.chart_points = vec![ChartPoint::new(mark.bar as f32, mark.value)];
    drawing.completed = true;
    if mark.pinned {
        // The chart pins this label to the pane edge `above` names instead of to its price.
        drawing.screen_anchor = Some([0.5, if mark.above { 0.0 } else { 1.0 }]);
    }
    // The glyph sits just outside the point: above the high an `abovebar` mark names, or below
    // its low. A mark pinned to the pane edge keeps its text inside the pane instead.
    drawing.text_anchor = match (mark.pinned, mark.above) {
        (true, true) => TextAnchor::Below,
        (true, false) => TextAnchor::Above,
        (false, true) => TextAnchor::Above,
        (false, false) => TextAnchor::Below,
    };
    if mark.auto_size {
        // `size.auto`: the mark is as wide as a bar, so its point size tracks the bar spacing.
        drawing.text_follows_zoom = true;
        drawing.text_zoom_spacing = Some(mark.font_size);
    }
    drawing.color = mark.color.to_srgba_unmultiplied();
    drawing.font_size = mark.font_size;
    drawing.text_color = Some(
        mark.text_color
            .unwrap_or(mark.color)
            .to_srgba_unmultiplied(),
    );
    drawing.text = Some(mark.text.clone());
    drawing
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
    let mut input = StudyInput::new(id, decl.title.clone(), value, kind);
    input.group = decl.group.clone();
    input.inline = decl.inline.clone();
    input.tooltip = decl.tooltip.clone();
    input
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
                .map(|choice| Value::Str(choice.clone().into()))
                .unwrap_or(Value::Na),
            _ => Value::Num(*index as f64),
        },
        InputValue::Timeframe(timeframe) => timeframe
            .map(|timeframe| Value::Str(timeframe.as_str().into_owned().into()))
            .unwrap_or(Value::Na),
        InputValue::Symbol(symbol) => Value::Str(symbol.clone().into()),
        InputValue::Text(text) => Value::Str(text.clone().into()),
        _ => Value::Na,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use egui_charts::studies::StudyCell;

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

    /// Waits for the run in flight to put its result where the next update takes it.
    fn wait_for_run(study: &mut PineStudy) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !study.update_ready() {
            assert!(std::time::Instant::now() < deadline, "the run finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
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
    fn display_none_reaches_the_plot_style() {
        let mut study =
            PineStudy::new("indicator(\"T\")\nplot(close, display=display.none)\n").unwrap();
        study.calculate(&bars(&[1.0]));
        assert!(!study.plot_style(0).visible);
    }

    #[test]
    fn plotcandle_reaches_the_chart_candles() {
        let mut study = PineStudy::new(
            "indicator(\"T\", overlay=true)\nplotcandle(open, high, low, close, color=color.green)\n",
        )
        .unwrap();
        let chart = bars(&[1.0, 2.0]);
        study.calculate(&chart);
        let candles = study.candles();
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[1].bars, 1..2);
        assert_eq!(candles[1].bar.high, 3.0);
        assert_eq!(candles[1].color, Some(Color32::GREEN));
        assert_eq!(candles[1].plot, 0);
        assert!(study.plot_style(0).visible);
    }

    #[test]
    fn another_symbol_becomes_a_study_data_request() {
        let mut study =
            PineStudy::new("indicator(\"T\")\nplot(request.security(\"BTCUSDT\", \"5\", close))\n")
                .unwrap();
        assert!(study.data_requests().is_empty());
        study.calculate(&bars(&[1.0, 2.0]));
        let requests = study.data_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].symbol, "BTCUSDT");
        assert_eq!(requests[0].timeframe, Some(Timeframe::Min5));
    }

    #[test]
    fn another_symbol_history_reaches_the_plot() {
        let mut study =
            PineStudy::new("indicator(\"T\")\nplot(request.security(\"OTHER\", \"2\", close))\n")
                .unwrap();
        let chart = bars(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        study.calculate(&chart);
        let request = study.data_requests().pop().expect("another symbol");
        assert_eq!(
            request.timeframe,
            Some(Timeframe::Custom(120.try_into().unwrap()))
        );
        let source: Vec<Bar> = [100.0, 200.0, 300.0, 400.0, 500.0]
            .iter()
            .enumerate()
            .map(|(at, close)| {
                Bar::new(
                    DateTime::UNIX_EPOCH + chrono::Duration::minutes(at as i64 * 2),
                    *close,
                    *close,
                    *close,
                    *close,
                    1.0,
                )
            })
            .collect();
        let timeframe = Timeframe::Custom(120.try_into().unwrap());
        let data = StudyData {
            request,
            bars: &source,
            clock: egui_charts::model::BarClock::new(timeframe),
            instrument: None,
            timeframe,
        };
        study.update_data(&chart, 0, &[data]);
        assert!(study.values().value(0, 0).is_none());
        assert!(study.values().value(0, 1).is_none());
        assert_eq!(study.values().value(0, 2), Some(100.0));
        assert_eq!(study.values().value(0, 3), Some(100.0));
        assert_eq!(study.values().value(0, 4), Some(200.0));
    }

    /// The CISD table's shape: a `request.security` row that reads `Loading` until the feed's
    /// finer history reaches the last chart bar, then the finer trend.
    #[test]
    fn a_lower_timeframe_row_leaves_loading_once_its_history_arrives() {
        let mut study = PineStudy::new(
            "//@version=6\nindicator(\"MTF\", overlay=true)\ntrend = close > open ? 1 : -1\nhtf = request.security(syminfo.tickerid, \"1\", trend)\nvar table t = table.new(position.top_right, 2, 2)\nif barstate.islast\n    table.cell(t, 0, 0, \"HTF\")\n    table.cell(t, 1, 0, htf == 1 ? \"UP\" : htf == -1 ? \"DOWN\" : \"Loading\")\n",
        )
        .unwrap();
        study.set_timeframe(Timeframe::Hour4);
        let chart: Vec<Bar> = [0, 4]
            .iter()
            .map(|hour| {
                Bar::new(
                    DateTime::UNIX_EPOCH + chrono::Duration::hours(*hour),
                    11.0,
                    12.0,
                    8.0,
                    9.0,
                    1.0,
                )
            })
            .collect();
        let trend_cell = |study: &PineStudy| study.tables()[0].rows[0][1].clone();

        study.calculate(&chart);
        let request = study.data_requests().pop().expect("the finer timeframe");
        assert_eq!(request.timeframe, Some(Timeframe::Min1));
        assert_eq!(trend_cell(&study), StudyCell::Text("Loading".into()));

        let source: Vec<Bar> = (0..480)
            .map(|minute| {
                Bar::new(
                    DateTime::UNIX_EPOCH + chrono::Duration::minutes(minute),
                    10.0,
                    11.0,
                    9.0,
                    10.5,
                    1.0,
                )
            })
            .collect();
        let data = StudyData {
            request,
            bars: &source,
            clock: egui_charts::model::BarClock::new(Timeframe::Min1),
            instrument: None,
            timeframe: Timeframe::Min1,
        };
        study.update_data(&chart, 0, &[data]);
        assert_eq!(trend_cell(&study), StudyCell::Text("UP".into()));
    }

    #[test]
    fn plotchar_text_is_drawn_as_a_label() {
        let mut study = PineStudy::new(
            "indicator(\"T\", overlay=true)\nplotchar(close > 0, text=\"Mon\", location=location.abovebar)\n",
        )
        .unwrap();
        study.calculate(&bars(&[10.0]));
        let drawings = study.drawings();
        assert_eq!(drawings.len(), 1);
        assert_eq!(drawings[0].text.as_deref(), Some("Mon"));
        assert_eq!(drawings[0].chart_points[0].price, 11.0);
    }

    #[test]
    fn syminfo_reads_the_instrument() {
        let mut study = PineStudy::new(
            "indicator(\"T\")\nplot(syminfo.type == \"crypto\" ? 1 : 0)\nplot(str.length(syminfo.prefix))\nplot(str.length(syminfo.root))\n",
        )
        .unwrap();
        let mut instrument = Instrument::new("BTCUSDT");
        instrument.exchange = Some("BINANCE".into());
        instrument.root = Some("BTC".into());
        instrument.symbol_type = Some(egui_charts::model::SymbolType::Crypto);
        assert!(study.set_instrument(Some(&instrument)));
        study.calculate(&bars(&[1.0]));
        assert_eq!(study.values().value(0, 0), Some(1.0));
        assert_eq!(study.values().value(1, 0), Some(7.0));
        assert_eq!(study.values().value(2, 0), Some(3.0));
    }

    #[test]
    fn a_pine_line_becomes_a_chart_drawing() {
        let mut study = PineStudy::new(
            "//@version=6\nindicator(\"L\", overlay=true)\nif barstate.islast\n    line.new(0, low, bar_index, high, color=color.red, width=2)\n",
        )
        .unwrap();
        let data = bars(&[1.0, 2.0, 3.0]);
        study.calculate(&data);
        let drawings = study.drawings();
        assert_eq!(drawings.len(), 1);
        assert_eq!(drawings[0].chart_points.len(), 2);
        assert_eq!(drawings[0].chart_points[0].bar_idx, 0.0);
        assert_eq!(drawings[0].chart_points[1].bar_idx, 2.0);
        assert!(study.keeps_drawing_style());
    }

    #[test]
    fn a_plotshape_mark_is_one_label_and_no_chart_marker() {
        let mut study = PineStudy::new(
            "//@version=4\nstudy(\"Fractals\", overlay=true)\nn = input(title=\"Periods\", defval=2, type=input.integer)\ndn = high[2] > high[1] and high[2] > high[0]\nplotshape(dn, location=location.abovebar, color=color.white)\n",
        )
        .unwrap();
        let data = bars(&[1.0, 2.0, 5.0, 2.0, 1.0]);
        study.calculate(&data);
        let drawings = study.drawings();
        assert_eq!(drawings.len(), 1, "{drawings:?}");
        // The glyph sits above the high, and the chart draws no second, red or green, triangle.
        assert_eq!(drawings[0].text_anchor, TextAnchor::Above);
        assert!(study.markers().is_empty());
    }

    /// A prepend over a long history runs on its own thread and lands on a later update.
    #[test]
    fn a_background_run_lands_after_the_bars_it_ran_on() {
        let mut study =
            PineStudy::new("//@version=6\nindicator(\"T\")\nplot(ta.ema(close, 3))").unwrap();
        study.set_context(Some(egui::Context::default()));
        let tail = bars(&[1.0, 2.0, 3.0, 4.0]);
        study.calculate(&tail);
        let mut longer = bars(&[0.5; 1200]);
        longer.extend(tail.iter().cloned());
        study.update_data(&longer, 0, &[]);
        assert!(
            study.values().is_empty(),
            "the values of the old bars are gone until the run lands"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !study.update_ready() {
            assert!(std::time::Instant::now() < deadline, "the run finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        study.update_data(&longer, 0, &[]);
        assert_eq!(study.values().len(), longer.len());
        assert!(study.values().value(0, longer.len() - 1).is_some());
    }

    /// A new bar keeps the drawn picture until the run for it lands, so the chart does not blink.
    #[test]
    fn a_new_bar_keeps_the_picture_until_its_run_lands() {
        let mut study = PineStudy::new(
            "//@version=6\nindicator(\"L\", overlay=true)\nplot(close)\nif barstate.islast\n    line.new(0, low, bar_index, high)\n",
        )
        .unwrap();
        study.set_context(Some(egui::Context::default()));
        let data = bars(&[1.0; 8]);
        study.calculate(&data);
        assert_eq!(study.values().len(), 8);
        assert_eq!(study.drawings().len(), 1);
        // A bar on the end leaves every drawn bar at its index.
        let longer = bars(&[1.0; 9]);
        study.update_data(&longer, 8, &[]);
        assert_eq!(
            study.values().len(),
            8,
            "the picture stays while the new run is in flight"
        );
        assert_eq!(study.drawings().len(), 1);
        wait_for_run(&mut study);
        study.update_data(&longer, 8, &[]);
        assert_eq!(study.values().len(), 9);
        assert_eq!(study.drawings()[0].chart_points[1].bar_idx, 8.0);
        // A tick rewrites the last drawn bar in place, and the picture still lines up.
        let mut ticked = longer.clone();
        ticked[8].close = 1.5;
        study.update_data(&ticked, 8, &[]);
        assert_eq!(study.values().len(), 9, "a tick must not blank the chart");
        assert_eq!(study.drawings().len(), 1);
    }

    /// Export and undo settle a running script, so their values belong to the bars they hold.
    #[test]
    fn settling_calculates_without_waiting_for_a_frame() {
        let mut study =
            PineStudy::new("//@version=6\nindicator(\"T\")\nplot(ta.ema(close, 3))").unwrap();
        study.set_context(Some(egui::Context::default()));
        let tail = bars(&[1.0, 2.0, 3.0, 4.0]);
        study.calculate(&tail);
        let mut longer = bars(&[0.5; 1200]);
        longer.extend(tail.iter().cloned());
        study.update_data(&longer, 0, &[]);
        study.settle();
        study.update_data(&longer, 0, &[]);
        assert_eq!(study.values().len(), longer.len());
        assert!(!study.update_ready());
    }

    /// Adding a study starts its first run on a thread, not in the frame that adds it. The
    /// chart's blanking call drops the values of the old bars and keeps the styles.
    #[test]
    fn the_first_run_leaves_the_calling_thread() {
        let mut study = PineStudy::new("//@version=6\nindicator(\"T\")\nplot(close)").unwrap();
        study.set_context(Some(egui::Context::default()));
        let data = bars(&[1.0; 1200]);
        study.update_data(&data, 0, &[]);
        assert!(study.values().is_empty(), "the run is still on its thread");
        wait_for_run(&mut study);
        study.update_data(&data, 0, &[]);
        assert_eq!(study.values().len(), data.len());
        let names = study.line_names.len();
        study.update(&[], 0);
        assert!(study.values().is_empty());
        assert_eq!(study.line_names.len(), names, "the styles stay");
    }

    /// The chart blanks a study whose turn has not come while a finished run waits to be drawn:
    /// the lines keep their picture until that run lands, or they blink once per run.
    #[test]
    fn a_finished_run_keeps_the_lines_until_they_are_drawn() {
        let mut study = PineStudy::new("//@version=6\nindicator(\"T\")\nplot(close)").unwrap();
        study.set_context(Some(egui::Context::default()));
        let data = bars(&[1.0; 1200]);
        study.update_data(&data, 0, &[]);
        wait_for_run(&mut study);
        study.update_data(&data, 0, &[]);
        assert_eq!(study.values().len(), data.len());
        // A tick starts the next run; the blanking call comes before that run is drawn.
        study.update_data(&data, data.len() - 1, &[]);
        assert_eq!(study.values().len(), data.len(), "the lines blinked");
        wait_for_run(&mut study);
        study.update(&[], 0);
        assert_eq!(study.values().len(), data.len(), "the lines blinked");
        study.update_data(&data, 0, &[]);
        assert_eq!(study.values().len(), data.len());
    }

    /// The chart asks for a full recompute whenever a history the script reads changes. The bars
    /// are the same then, so the picture drawn stays until the new run lands instead of blinking.
    #[test]
    fn a_full_recompute_keeps_the_picture_of_the_same_bars() {
        let mut study = PineStudy::new("//@version=6\nindicator(\"T\")\nplot(close)").unwrap();
        study.set_context(Some(egui::Context::default()));
        let data = bars(&[1.0; 1200]);
        study.update_data(&data, 0, &[]);
        wait_for_run(&mut study);
        study.update_data(&data, 0, &[]);
        assert_eq!(study.values().len(), data.len());
        study.update_data(&data, 0, &[]);
        assert_eq!(study.values().len(), data.len(), "the lines blinked");
        wait_for_run(&mut study);
        study.update_data(&data, 0, &[]);
        assert_eq!(study.values().len(), data.len());
        // A picture of other bars still goes: a prepend moves every value.
        let mut longer = bars(&[0.5; 100]);
        longer.extend(data);
        study.update_data(&longer, 0, &[]);
        assert!(
            study.values().is_empty(),
            "values of other bars stay on these"
        );
    }
}
