//! Evaluates a parsed script bar by bar: series and `var` state, user functions, `if` values,
//! `plot`/`fill`/`plotshape`, and the stateful builtins.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Timelike};
use egui::Color32;
use egui_charts::model::Bar;
use egui_charts::studies::{FillBoundary, PlotType, StudyFill};

use crate::pine_engine::ast::{Arg, Diagnostic, Expr, Function, NodeId, Program, Stmt};
use crate::pine_engine::builtins::{self, TaState};
use crate::pine_engine::parser::literal;
use crate::pine_engine::value::Value;

/// The chart facts a script may read: `syminfo.*` and `timeframe.*`.
#[derive(Clone, Debug)]
pub struct Context {
    pub symbol: String,
    pub timeframe: egui_charts::model::Timeframe,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            timeframe: egui_charts::model::Timeframe::Min1,
        }
    }
}

/// One `plot` output: a full column and its presentation.
#[derive(Clone, Debug)]
pub struct PlotSeries {
    pub title: String,
    pub color: Color32,
    pub width: f32,
    pub plot_type: PlotType,
    pub offset: isize,
    pub values: Vec<f64>,
    pub colors: Vec<Option<Color32>>,
}

/// One `plotshape` mark on a bar.
#[derive(Clone, Copy, Debug)]
pub struct ShapeMark {
    pub bar: usize,
    pub value: f64,
    pub above: bool,
    // The library marker carries no colour, so the script's choice is kept but unused.
    #[allow(dead_code)]
    pub color: Color32,
}

/// Everything one run of the script produced.
#[derive(Clone, Debug, Default)]
pub struct RunOutput {
    pub plots: Vec<PlotSeries>,
    pub shapes: Vec<ShapeMark>,
    pub fills: Vec<StudyFill>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A script variable: its value on the current bar and the finished bars behind it.
#[derive(Clone, Debug)]
struct Var {
    value: Value,
    history: Vec<Value>,
}

#[derive(Clone, Debug)]
struct PlotSlot {
    series: PlotSeries,
}

#[derive(Clone, Debug)]
struct FillSlot {
    first: usize,
    second: usize,
    color: Color32,
}

#[derive(Clone, Debug)]
struct ShapeSlot {
    above: bool,
    color: Color32,
    offset: isize,
    marks: Vec<(usize, f64)>,
}

/// Runs one script over a bar series.
pub struct Evaluator<'a> {
    bars: &'a [Bar],
    program: &'a Program,
    context: &'a Context,
    inputs: &'a HashMap<NodeId, Value>,
    vars: HashMap<String, Var>,
    locals: Vec<HashMap<String, Value>>,
    functions: HashMap<&'a str, &'a Function>,
    ta: HashMap<NodeId, TaState>,
    plots: Vec<PlotSlot>,
    plot_at: HashMap<NodeId, usize>,
    fills: Vec<FillSlot>,
    fill_at: HashMap<NodeId, usize>,
    shapes: Vec<ShapeSlot>,
    shape_at: HashMap<NodeId, usize>,
    diagnostics: Vec<Diagnostic>,
    reported: HashSet<(usize, String)>,
    depth: usize,
    bar: usize,
}

impl<'a> Evaluator<'a> {
    pub fn new(
        program: &'a Program,
        bars: &'a [Bar],
        context: &'a Context,
        inputs: &'a HashMap<NodeId, Value>,
    ) -> Self {
        let mut functions = HashMap::new();
        for statement in &program.body {
            if let Stmt::Function(function) = statement {
                functions.insert(function.name.as_str(), function);
            }
        }
        Self {
            bars,
            program,
            context,
            inputs,
            vars: HashMap::new(),
            locals: Vec::new(),
            functions,
            ta: HashMap::new(),
            plots: Vec::new(),
            plot_at: HashMap::new(),
            fills: Vec::new(),
            fill_at: HashMap::new(),
            shapes: Vec::new(),
            shape_at: HashMap::new(),
            diagnostics: Vec::new(),
            reported: HashSet::new(),
            depth: 0,
            bar: 0,
        }
    }

    /// Runs every bar, in order, as a chart's studies are calculated.
    pub fn run(mut self) -> RunOutput {
        let body = self.program.body.as_slice();
        for bar in 0..self.bars.len() {
            self.bar = bar;
            self.run_block(body);
            self.finish_bar();
        }
        let fills = self
            .fills
            .iter()
            .map(|fill| StudyFill {
                first: FillBoundary::Plot(fill.first),
                second: FillBoundary::Plot(fill.second),
                color: fill.color,
                inverted_color: None,
            })
            .collect();
        let shapes = self
            .shapes
            .iter()
            .flat_map(|shape| {
                shape.marks.iter().map(move |&(bar, value)| ShapeMark {
                    bar,
                    value,
                    above: shape.above,
                    color: shape.color,
                })
            })
            .collect();
        RunOutput {
            plots: self.plots.into_iter().map(|slot| slot.series).collect(),
            shapes,
            fills,
            diagnostics: self.diagnostics,
        }
    }

    fn finish_bar(&mut self) {
        for var in self.vars.values_mut() {
            // A bar that does not run an assignment keeps the previous value, as Pine does.
            var.history.push(var.value.clone());
        }
    }

    fn run_block(&mut self, block: &'a [Stmt]) -> Value {
        let mut value = Value::Na;
        for statement in block {
            value = self.run_stmt(statement);
        }
        value
    }

    fn run_stmt(&mut self, statement: &'a Stmt) -> Value {
        match statement {
            Stmt::Assign {
                name,
                value,
                sticky,
            } => {
                let current = self.vars.get(name);
                // `var x = …` runs on the first bar only; a missing one is `na` until assigned.
                if *sticky && self.bar > 0 {
                    return current.map(|var| var.value.clone()).unwrap_or(Value::Na);
                }
                let value = self.eval(value);
                if self.locals.is_empty() {
                    let var = self.vars.entry(name.clone()).or_insert_with(|| Var {
                        value: Value::Na,
                        history: Vec::new(),
                    });
                    var.value = value.clone();
                } else if let Some(scope) = self.locals.last_mut() {
                    scope.insert(name.clone(), value.clone());
                }
                value
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = self.eval(cond).flag().unwrap_or(false);
                if cond {
                    self.run_block(then)
                } else if let Some(otherwise) = otherwise {
                    self.run_block(otherwise)
                } else {
                    Value::Na
                }
            }
            Stmt::Expr(value) => self.eval(value),
            Stmt::Function(_) | Stmt::Unsupported { .. } => Value::Na,
        }
    }

    fn eval(&mut self, expr: &'a Expr) -> Value {
        match expr {
            Expr::Number(number) => Value::Num(*number),
            Expr::Bool(flag) => Value::Bool(*flag),
            Expr::Str(text) => Value::Str(text.clone()),
            Expr::Color(rgba) => Value::Color(crate::pine_engine::parser::rgba_of(*rgba)),
            Expr::Na => Value::Na,
            Expr::Array(_) => Value::Na,
            Expr::Ident(name) => self.ident(name),
            Expr::Member { target, name } => {
                let path = format!("{}.{}", dotted(target).unwrap_or_default(), name);
                self.member(&path)
            }
            Expr::Call {
                id,
                callee,
                args,
                line,
            } => self.call(*id, callee, args, *line),
            Expr::Index { target, index } => self.index(target, index),
            Expr::Unary { op, value } => {
                let value = self.eval(value);
                match *op {
                    "-" => match value.number() {
                        Some(number) => Value::Num(-number),
                        None => Value::Na,
                    },
                    "not" => Value::Bool(!value.flag().unwrap_or(false)),
                    _ => Value::Na,
                }
            }
            Expr::Binary { op, left, right } => match *op {
                "and" => {
                    if !self.eval(left).flag().unwrap_or(false) {
                        Value::Bool(false)
                    } else {
                        Value::Bool(self.eval(right).flag().unwrap_or(false))
                    }
                }
                "or" => {
                    if self.eval(left).flag().unwrap_or(false) {
                        Value::Bool(true)
                    } else {
                        Value::Bool(self.eval(right).flag().unwrap_or(false))
                    }
                }
                _ => {
                    let left = self.eval(left);
                    let right = self.eval(right);
                    binary(op, left, right)
                }
            },
            Expr::Ternary {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond).flag().unwrap_or(false) {
                    self.eval(then)
                } else {
                    self.eval(otherwise)
                }
            }
            Expr::If {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond).flag().unwrap_or(false) {
                    self.run_block(then)
                } else if let Some(otherwise) = otherwise {
                    self.run_block(otherwise)
                } else {
                    Value::Na
                }
            }
            Expr::Switch { subject, arms } => {
                let subject = subject.as_ref().map(|subject| self.eval(subject));
                for arm in arms {
                    let matched = match (&subject, &arm.cond) {
                        (Some(subject), Some(cond)) => {
                            let cond = self.eval(cond);
                            binary("==", subject.clone(), cond).flag().unwrap_or(false)
                        }
                        (_, Some(cond)) => self.eval(cond).flag().unwrap_or(false),
                        // A bare `=>` arm answers when no earlier arm did.
                        (_, None) => true,
                    };
                    if matched {
                        return self.run_block(&arm.body);
                    }
                }
                Value::Na
            }
        }
    }

    fn ident(&self, name: &str) -> Value {
        for scope in self.locals.iter().rev() {
            if let Some(value) = scope.get(name) {
                return value.clone();
            }
        }
        if let Some(var) = self.vars.get(name) {
            return var.value.clone();
        }
        match name {
            "open" => self.bar_value(|bar| bar.open),
            "high" => self.bar_value(|bar| bar.high),
            "low" => self.bar_value(|bar| bar.low),
            "close" => self.bar_value(|bar| bar.close),
            "volume" => self.bar_value(|bar| bar.volume),
            "hl2" => self.bar_average([0.5, 0.0, 0.5, 0.0]),
            "hlc3" => self.bar_average([0.0, 1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]),
            "ohlc4" => self.bar_average([0.25; 4]),
            "bar_index" => Value::Num(self.bar as f64),
            "time" => self
                .bars
                .get(self.bar)
                .map(|bar| Value::Num(bar.time.timestamp_millis() as f64))
                .unwrap_or(Value::Na),
            "hour" => self.clock(|time| time.hour() as f64),
            "minute" => self.clock(|time| time.minute() as f64),
            "second" => self.clock(|time| time.second() as f64),
            "dayofmonth" => self.clock(|time| time.day() as f64),
            "month" => self.clock(|time| time.month() as f64),
            "year" => self.clock(|time| time.year() as f64),
            "dayofweek" => self.clock(|time| time.weekday().num_days_from_sunday() as f64 + 1.0),
            _ => Value::Na,
        }
    }

    fn bar_value(&self, field: impl Fn(&Bar) -> f64) -> Value {
        self.bars
            .get(self.bar)
            .map(|bar| Value::Num(field(bar)))
            .unwrap_or(Value::Na)
    }

    fn bar_average(&self, weights: [f64; 4]) -> Value {
        self.bars
            .get(self.bar)
            .map(|bar| {
                Value::Num(
                    bar.open * weights[0]
                        + bar.high * weights[1]
                        + bar.low * weights[2]
                        + bar.close * weights[3],
                )
            })
            .unwrap_or(Value::Na)
    }

    fn clock(&self, field: impl Fn(chrono::DateTime<chrono::Utc>) -> f64) -> Value {
        self.bars
            .get(self.bar)
            .map(|bar| Value::Num(field(bar.time)))
            .unwrap_or(Value::Na)
    }

    fn member(&self, path: &str) -> Value {
        match path {
            "math.pi" => return Value::Num(std::f64::consts::PI),
            "math.e" => return Value::Num(std::f64::consts::E),
            "barstate.isfirst" => return Value::Bool(self.bar == 0),
            "barstate.islast" => return Value::Bool(self.bar + 1 == self.bars.len()),
            "barstate.isconfirmed" | "barstate.isnew" => return Value::Bool(true),
            "barstate.isrealtime" => return Value::Bool(false),
            "barstate.ishistory" => return Value::Bool(true),
            "syminfo.tickerid" | "syminfo.ticker" => {
                return Value::Str(self.context.symbol.clone());
            }
            "syminfo.mintick" => return Value::Num(0.01),
            "timeframe.period" => return Value::Str(pine_period(self.context.timeframe)),
            "timeframe.multiplier" => return Value::Num(pine_multiplier(self.context.timeframe)),
            "timeframe.in_seconds" | "timeframe.in_seconds()" => {
                return Value::Num((self.context.timeframe.duration_ms() / 1000) as f64);
            }
            _ => {}
        }
        let (namespace, name) = path.split_once('.').unwrap_or(("", path));
        match namespace {
            "color" => builtins::color_constant(name)
                .map(Value::Color)
                .unwrap_or(Value::Na),
            "dayofweek" => match name {
                "sunday" => Value::Num(1.0),
                "monday" => Value::Num(2.0),
                "tuesday" => Value::Num(3.0),
                "wednesday" => Value::Num(4.0),
                "thursday" => Value::Num(5.0),
                "friday" => Value::Num(6.0),
                "saturday" => Value::Num(7.0),
                _ => Value::Na,
            },
            "timeframe" => match name {
                "isintraday" => Value::Bool(self.context.timeframe.duration_ms() < 86_400_000),
                "isminutes" => Value::Bool(self.context.timeframe.duration_ms() < 3_600_000),
                "isseconds" => Value::Bool(self.context.timeframe.duration_ms() < 60_000),
                "isdaily" => Value::Bool(self.context.timeframe.duration_ms() == 86_400_000),
                "isweekly" => Value::Bool(self.context.timeframe.duration_ms() == 604_800_000),
                "ismonthly" => Value::Bool(self.context.timeframe.duration_ms() >= 2_592_000_000),
                _ => Value::Bool(false),
            },
            // `shape.*`, `location.*`, `plot.*`, `size.*` and the like: the names the plot and
            // input calls read back as strings.
            _ if !namespace.is_empty() => Value::Str(name.to_owned()),
            _ => Value::Na,
        }
    }

    fn index(&mut self, target: &'a Expr, index: &'a Expr) -> Value {
        let offset = match self.eval(index).number() {
            Some(number) => number.round(),
            None => return Value::Na,
        };
        if offset < 0.0 {
            return Value::Na;
        }
        let offset = offset as usize;
        let Expr::Ident(name) = target else {
            return Value::Na;
        };
        if let Some(var) = self.vars.get(name) {
            if offset == 0 {
                return var.value.clone();
            }
            return var
                .history
                .len()
                .checked_sub(offset)
                .and_then(|at| var.history.get(at))
                .cloned()
                .unwrap_or(Value::Na);
        }
        let bar = match self.bar.checked_sub(offset) {
            Some(bar) => bar,
            None => return Value::Na,
        };
        match name.as_str() {
            "open" => Value::Num(self.bars[bar].open),
            "high" => Value::Num(self.bars[bar].high),
            "low" => Value::Num(self.bars[bar].low),
            "close" => Value::Num(self.bars[bar].close),
            "volume" => Value::Num(self.bars[bar].volume),
            "time" => Value::Num(self.bars[bar].time.timestamp_millis() as f64),
            "bar_index" => Value::Num(bar as f64),
            _ => Value::Na,
        }
    }

    fn call(&mut self, id: NodeId, callee: &[String], args: &'a [Arg], line: usize) -> Value {
        let name = callee.join(".");
        match name.as_str() {
            "plot" => return self.plot(id, args),
            "plotshape" | "plotchar" | "plotarrow" => return self.plotshape(id, args),
            "fill" => return self.fill(id, args),
            "hline" => return self.hline(id, args),
            "bgcolor" | "barcolor" | "alertcondition" | "alert" => return Value::Na,
            _ => {}
        }
        if name == "input" || name.starts_with("input.") {
            return self.inputs.get(&id).cloned().unwrap_or(Value::Na);
        }
        if name == "nz" || name == "fixnan" {
            let value = args.first().map(|arg| self.eval(&arg.value));
            let replacement = args.get(1).map(|arg| self.eval(&arg.value));
            return match value {
                Some(value) if !matches!(value, Value::Na) => value,
                _ => replacement.unwrap_or(Value::Num(0.0)),
            };
        }
        if name == "na" {
            let value = args.first().map(|arg| self.eval(&arg.value));
            return Value::Bool(matches!(value, None | Some(Value::Na)));
        }
        if let Some(rest) = name.strip_prefix("ta.") {
            let bar = self.bars[self.bar].clone();
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            let state = self.ta.entry(id).or_default();
            return match builtins::ta(rest, state, &values, &bar) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("math.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::math(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("color.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::color(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("str.") {
            let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
            return match builtins::text(rest, &values) {
                Some(value) => value,
                None => self.unsupported(line, &name),
            };
        }
        if let Some(rest) = name.strip_prefix("timeframe.") {
            return match rest {
                "in_seconds" => Value::Num((self.context.timeframe.duration_ms() / 1000) as f64),
                // `timeframe.change` compares the chart interval with the bar's own, which a
                // series of one interval cannot show.
                "change" => Value::Bool(false),
                _ => Value::Na,
            };
        }
        if name == "request.security" {
            return self.unsupported(line, &name);
        }
        if callee.len() == 1
            && let Some(function) = self.functions.get(name.as_str()).copied()
        {
            return self.call_function(function, args);
        }
        self.unsupported(line, &name)
    }

    fn call_function(&mut self, function: &'a Function, args: &'a [Arg]) -> Value {
        if self.depth > 64 {
            return Value::Na;
        }
        let values: Vec<Value> = args.iter().map(|arg| self.eval(&arg.value)).collect();
        let mut scope = HashMap::new();
        for (at, param) in function.params.iter().enumerate() {
            scope.insert(param.clone(), values.get(at).cloned().unwrap_or(Value::Na));
        }
        self.locals.push(scope);
        self.depth += 1;
        let value = self.run_block(&function.body);
        self.depth -= 1;
        self.locals.pop();
        value
    }

    fn plot(&mut self, id: NodeId, args: &'a [Arg]) -> Value {
        let Some(first) = args.first() else {
            return Value::Na;
        };
        let value = self.eval(&first.value);
        let bar_color = named(args, "color")
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let slot = match self.plot_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let index = self.plots.len();
                let title = title_of(args).unwrap_or_else(|| format!("Plot {}", index + 1));
                let color = bar_color.unwrap_or_else(|| plot_color(index));
                let width = named(args, "linewidth")
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.number())
                    .unwrap_or(1.0) as f32;
                let offset = named(args, "offset")
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.number())
                    .unwrap_or(0.0) as isize;
                let plot_type = named(args, "style")
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.text().map(plot_type))
                    .unwrap_or(PlotType::Line);
                self.plots.push(PlotSlot {
                    series: PlotSeries {
                        title,
                        color,
                        width: width.clamp(0.5, 8.0),
                        plot_type,
                        offset,
                        values: vec![f64::NAN; self.bars.len()],
                        colors: vec![None; self.bars.len()],
                    },
                });
                self.plot_at.insert(id, index);
                index
            }
        };
        if let Some(number) = value.number() {
            let plot = &mut self.plots[slot].series;
            plot.values[self.bar] = number;
            plot.colors[self.bar] = bar_color;
        }
        Value::Plot(slot)
    }

    fn hline(&mut self, id: NodeId, args: &'a [Arg]) -> Value {
        let Some(first) = args.first() else {
            return Value::Na;
        };
        let value = self.eval(&first.value);
        let slot = match self.plot_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let title =
                    title_of(args).unwrap_or_else(|| format!("Level {}", self.plots.len() + 1));
                let color = named(args, "color")
                    .map(|arg| self.eval(&arg.value))
                    .and_then(|value| value.color())
                    .unwrap_or_else(|| plot_color(self.plots.len()));
                let index = self.plots.len();
                self.plots.push(PlotSlot {
                    series: PlotSeries {
                        title,
                        color,
                        width: 1.0,
                        plot_type: PlotType::Line,
                        offset: 0,
                        values: vec![f64::NAN; self.bars.len()],
                        colors: vec![None; self.bars.len()],
                    },
                });
                self.plot_at.insert(id, index);
                index
            }
        };
        if let Some(number) = value.number() {
            self.plots[slot].series.values[self.bar] = number;
        }
        Value::Plot(slot)
    }

    fn fill(&mut self, id: NodeId, args: &'a [Arg]) -> Value {
        if self.fill_at.contains_key(&id) || args.len() < 2 {
            return Value::Na;
        }
        let first = self.eval(&args[0].value);
        let second = self.eval(&args[1].value);
        let (Value::Plot(first), Value::Plot(second)) = (first, second) else {
            return Value::Na;
        };
        let fallback = self.plots[first].series.color;
        let color = named(args, "color")
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color())
            .unwrap_or_else(|| {
                Color32::from_rgba_unmultiplied(fallback.r(), fallback.g(), fallback.b(), 40)
            });
        self.fills.push(FillSlot {
            first,
            second,
            color,
        });
        self.fill_at.insert(id, self.fills.len() - 1);
        Value::Na
    }

    fn plotshape(&mut self, id: NodeId, args: &'a [Arg]) -> Value {
        let Some(first) = args.first() else {
            return Value::Na;
        };
        let condition = self.eval(&first.value);
        let above = named(args, "location")
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.text().map(str::to_owned))
            .is_none_or(|location| !location.contains("below"));
        let color = named(args, "color")
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.color());
        let offset = named(args, "offset")
            .map(|arg| self.eval(&arg.value))
            .and_then(|value| value.number())
            .unwrap_or(0.0) as isize;
        let slot = match self.shape_at.get(&id) {
            Some(slot) => *slot,
            None => {
                let color = color.unwrap_or_else(|| plot_color(self.shapes.len()));
                self.shapes.push(ShapeSlot {
                    above,
                    color,
                    offset,
                    marks: Vec::new(),
                });
                self.shape_at.insert(id, self.shapes.len() - 1);
                self.shapes.len() - 1
            }
        };
        let fires = match &condition {
            Value::Bool(flag) => *flag,
            Value::Num(number) => *number != 0.0,
            _ => false,
        };
        if fires {
            let Some(bar) = self.bars.get(self.bar) else {
                return Value::Na;
            };
            let value = condition
                .number()
                .filter(|number| *number > 1.0)
                .unwrap_or(if above { bar.high } else { bar.low });
            if let Some(target) = self.bar.checked_add_signed(self.shapes[slot].offset)
                && target < self.bars.len()
            {
                self.shapes[slot].marks.push((target, value));
            }
        }
        Value::Na
    }

    fn unsupported(&mut self, line: usize, name: &str) -> Value {
        let message = format!("`{name}` не поддерживается интерпретатором");
        if self.reported.insert((line, message.clone())) {
            self.diagnostics.push(Diagnostic { line, message });
        }
        Value::Na
    }
}

fn binary(op: &str, left: Value, right: Value) -> Value {
    if op == "+" && (matches!(left, Value::Str(_)) || matches!(right, Value::Str(_))) {
        let text = |value: &Value| match value {
            Value::Str(text) => text.clone(),
            Value::Na => String::new(),
            other => match other.number() {
                Some(number) => number.to_string(),
                None => String::new(),
            },
        };
        return Value::Str(format!("{}{}", text(&left), text(&right)));
    }
    if matches!(op, "==" | "!=") {
        let equal = match (&left, &right) {
            (Value::Str(first), Value::Str(second)) => first == second,
            (Value::Bool(first), Value::Bool(second)) => first == second,
            (Value::Color(first), Value::Color(second)) => first == second,
            _ => match (left.number(), right.number()) {
                (Some(first), Some(second)) => (first - second).abs() <= 1e-9,
                _ => matches!((&left, &right), (Value::Na, Value::Na)),
            },
        };
        return Value::Bool(if op == "==" { equal } else { !equal });
    }
    let (Some(left), Some(right)) = (left.number(), right.number()) else {
        return Value::Na;
    };
    match op {
        "+" => Value::Num(left + right),
        "-" => Value::Num(left - right),
        "*" => Value::Num(left * right),
        "/" => {
            if right == 0.0 {
                Value::Na
            } else {
                Value::Num(left / right)
            }
        }
        "%" => Value::Num(left % right),
        "<" => Value::Bool(left < right),
        "<=" => Value::Bool(left <= right),
        ">" => Value::Bool(left > right),
        ">=" => Value::Bool(left >= right),
        _ => Value::Na,
    }
}

/// The dotted path of a member expression, e.g. `color.new`.
fn dotted(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        Expr::Member { target, name } => Some(format!("{}.{}", dotted(target)?, name)),
        _ => None,
    }
}

fn named<'a>(args: &'a [Arg], name: &str) -> Option<&'a Arg> {
    args.iter().find(|arg| arg.name.as_deref() == Some(name))
}

/// The caption of a plot: `title=` or the second positional argument.
fn title_of(args: &[Arg]) -> Option<String> {
    named(args, "title")
        .or_else(|| args.get(1).filter(|arg| arg.name.is_none()))
        .map(|arg| &arg.value)
        .and_then(literal)
        .and_then(|value| value.text().map(str::to_owned))
}

fn plot_type(style: &str) -> PlotType {
    match style {
        "stepline" | "stepline_diamond" => PlotType::StepLine,
        "histogram" => PlotType::Histogram,
        "columns" => PlotType::Columns,
        "area" | "areabr" => PlotType::Area,
        "circles" => PlotType::Circles,
        "cross" => PlotType::Crosses,
        _ => PlotType::Line,
    }
}

/// A stable colour for a plot that names none.
fn plot_color(index: usize) -> Color32 {
    const PALETTE: [Color32; 6] = [
        egui_charts::studies::palette::BLUE,
        egui_charts::studies::palette::WARM_ORANGE,
        egui_charts::studies::palette::GREEN,
        egui_charts::studies::palette::PURPLE,
        egui_charts::studies::palette::CYAN,
        egui_charts::studies::palette::YELLOW,
    ];
    PALETTE[index % PALETTE.len()]
}

/// Pine's `timeframe.period`, e.g. `"1"`, `"60"`, `"D"`.
fn pine_period(timeframe: egui_charts::model::Timeframe) -> String {
    let millis = timeframe.duration_ms();
    match millis {
        0 => timeframe.as_str().into_owned(),
        86_400_000 => "D".to_owned(),
        604_800_000 => "W".to_owned(),
        2_592_000_000 => "M".to_owned(),
        millis if millis % 3_600_000 == 0 => (millis / 3_600_000).to_string(),
        millis if millis % 60_000 == 0 => (millis / 60_000).to_string(),
        millis => format!("{}S", millis / 1000),
    }
}

fn pine_multiplier(timeframe: egui_charts::model::Timeframe) -> f64 {
    let millis = timeframe.duration_ms();
    match millis {
        0 => 1.0,
        86_400_000 | 604_800_000 | 2_592_000_000 => 1.0,
        millis if millis % 3_600_000 == 0 => (millis / 3_600_000) as f64,
        millis if millis % 60_000 == 0 => (millis / 60_000) as f64,
        millis => (millis / 1000) as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pine_engine::parser::Parser;
    use chrono::{TimeZone, Utc};

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(at, close)| {
                Bar::new(
                    Utc.with_ymd_and_hms(2024, 1, 1, 0, at as u32 % 60, 0)
                        .unwrap(),
                    *close,
                    close + 1.0,
                    close - 1.0,
                    *close,
                    1.0,
                )
            })
            .collect()
    }

    fn run(source: &str, closes: &[f64]) -> RunOutput {
        let program = Parser::parse(source).unwrap();
        let bars = bars(closes);
        let context = Context::default();
        Evaluator::new(&program, &bars, &context, &HashMap::new()).run()
    }

    #[test]
    fn sma_warms_up_and_plots_one_column() {
        let output = run(
            "//@version=6\nindicator(\"T\")\nv = ta.sma(close, 3)\nplot(v)\n",
            &[1.0, 2.0, 3.0, 4.0, 5.0],
        );
        assert_eq!(output.plots.len(), 1);
        let values = &output.plots[0].values;
        assert!(values[0].is_nan());
        assert!(values[1].is_nan());
        assert!((values[2] - 2.0).abs() < 1e-9);
        assert!((values[3] - 3.0).abs() < 1e-9);
        assert!((values[4] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn series_index_reads_past_bars_and_na_before_them() {
        let output = run("indicator(\"T\")\nplot(close[1])\n", &[1.0, 2.0, 3.0]);
        let values = &output.plots[0].values;
        assert!(values[0].is_nan());
        assert!((values[1] - 1.0).abs() < 1e-9);
        assert!((values[2] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn var_keeps_its_first_value() {
        let output = run(
            "indicator(\"T\")\nvar first = close\nplot(first)\n",
            &[1.0, 2.0, 3.0],
        );
        assert_eq!(output.plots[0].values, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn if_expression_yields_the_assigned_value() {
        let output = run(
            "indicator(\"T\")\nx = if close > 2\n    y = 10\nelse\n    y = 20\nplot(x)\n",
            &[1.0, 3.0],
        );
        assert_eq!(output.plots[0].values, [20.0, 10.0]);
    }

    #[test]
    fn fills_join_two_plots() {
        let output = run(
            "indicator(\"T\")\na = plot(close)\nb = plot(open)\nfill(a, b, color=color.new(color.red, 80))\n",
            &[1.0, 2.0],
        );
        assert_eq!(output.fills.len(), 1);
        assert_eq!(output.fills[0].color.a(), 51);
    }

    #[test]
    fn user_function_runs_inside_the_caller_bar() {
        let output = run(
            "indicator(\"T\")\ndouble(x) =>\n    y = x * 2\n    y\nplot(double(close))\n",
            &[1.0, 2.0],
        );
        assert_eq!(output.plots[0].values, [2.0, 4.0]);
    }

    #[test]
    fn unsupported_calls_report_their_line_once() {
        let output = run(
            "indicator(\"T\")\nplot(array.push(a, 1) + array.push(a, 2))\n",
            &[1.0, 2.0, 3.0],
        );
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(output.diagnostics[0].line, 2);
    }

    #[test]
    fn plotshape_marks_the_bar_two_bars_behind() {
        let output = run(
            "indicator(\"T\")\nplotshape(close > 2, location=location.abovebar, offset=-2)\n",
            &[1.0, 2.0, 3.0, 4.0],
        );
        assert_eq!(output.shapes.len(), 2);
        assert_eq!(output.shapes[0].bar, 0);
        assert_eq!(output.shapes[0].value, 4.0);
        assert!(output.shapes[0].above);
    }

    #[test]
    fn an_hour_bar_feeds_hour() {
        let program = Parser::parse("indicator(\"T\")\nplot(hour)\n").unwrap();
        let bars = bars(&[1.0]);
        let context = Context::default();
        let output = Evaluator::new(&program, &bars, &context, &HashMap::new()).run();
        assert_eq!(output.plots[0].values[0], 0.0);
    }
}
