//! The `ta.*`, `math.*`, `color.*` and `str.*` calls of the subset, with the per-call-site
//! state a series function needs. The runtime keys [`TaState`] by call site, so `ta.ema` at two
//! places in a script keeps two independent averages.

use std::collections::VecDeque;

use egui::Color32;
use egui_charts::model::Bar;

use crate::pine_engine::value::Value;

/// How many past source values the windowed functions keep.
const MAX_WINDOW: usize = 8_192;

/// State of one `ta.*` call site between bars.
#[derive(Clone, Debug, Default)]
pub struct TaState {
    prev: Option<f64>,
    prev_other: Option<f64>,
    /// Running average of `ema`, `rma` and `atr`.
    average: Option<f64>,
    seed: f64,
    count: usize,
    gain: f64,
    loss: f64,
    average_gain: Option<f64>,
    average_loss: Option<f64>,
    prev_close: Option<f64>,
    window: VecDeque<f64>,
    occurrences: Vec<f64>,
    bars_since: Option<usize>,
    total: f64,
}

impl TaState {
    fn push(&mut self, value: f64, keep: usize) {
        self.window.push_back(value);
        let keep = keep.max(1).min(MAX_WINDOW);
        while self.window.len() > keep {
            self.window.pop_front();
        }
    }
}

fn number(value: &Value) -> Option<f64> {
    value.number()
}

fn arg(args: &[Value], at: usize) -> Option<f64> {
    args.get(at).and_then(number)
}

/// A whole-bar count such as a length, at least one.
fn length(value: Option<&Value>, fallback: usize) -> usize {
    value
        .and_then(number)
        .map(|size| (size.round() as i64).max(1) as usize)
        .unwrap_or(fallback)
}

fn na() -> Option<Value> {
    Some(Value::Na)
}

fn num(value: f64) -> Option<Value> {
    Some(if value.is_finite() {
        Value::Num(value)
    } else {
        Value::Na
    })
}

/// Calls `ta.*`; `None` for a name this subset does not run. A known name whose arguments are
/// missing answers `na` rather than reading as unsupported.
pub fn ta(name: &str, state: &mut TaState, args: &[Value], bar: &Bar) -> Option<Value> {
    const NAMES: &[&str] = &[
        "sma",
        "ema",
        "rma",
        "wma",
        "rsi",
        "tr",
        "atr",
        "change",
        "highest",
        "lowest",
        "stdev",
        "crossover",
        "crossunder",
        "cum",
        "barssince",
        "valuewhen",
        "pivothigh",
        "pivotlow",
        "na",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(ta_known(name, state, args, bar).unwrap_or(Value::Na))
}

fn ta_known(name: &str, state: &mut TaState, args: &[Value], bar: &Bar) -> Option<Value> {
    match name {
        "sma" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            num(state.window.iter().sum::<f64>() / len as f64)
        }
        "ema" | "rma" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            if len <= 1 {
                return num(src);
            }
            state.count += 1;
            let Some(average) = state.average else {
                // Pine seeds both averages with the mean of the first `len` values.
                state.seed += src;
                if state.count < len {
                    return na();
                }
                let seed = state.seed / len as f64;
                state.average = Some(seed);
                return num(seed);
            };
            let alpha = if name == "ema" {
                2.0 / (len as f64 + 1.0)
            } else {
                1.0 / len as f64
            };
            let next = average + alpha * (src - average);
            state.average = Some(next);
            num(next)
        }
        "wma" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let mut weighted = 0.0;
            let mut weights = 0.0;
            for (at, value) in state.window.iter().enumerate() {
                let weight = (at + 1) as f64;
                weighted += value * weight;
                weights += weight;
            }
            num(weighted / weights)
        }
        "rsi" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 14);
            let previous = state.prev.replace(src)?;
            let change = src - previous;
            let gain = change.max(0.0);
            let loss = (-change).min(0.0);
            state.count += 1;
            let (average_gain, average_loss) = match (state.average_gain, state.average_loss) {
                (Some(average_gain), Some(average_loss)) => (
                    (average_gain * (len as f64 - 1.0) + gain) / len as f64,
                    (average_loss * (len as f64 - 1.0) + loss) / len as f64,
                ),
                _ => {
                    state.gain += gain;
                    state.loss += loss;
                    if state.count < len {
                        return na();
                    }
                    (state.gain / len as f64, state.loss / len as f64)
                }
            };
            state.average_gain = Some(average_gain);
            state.average_loss = Some(average_loss);
            if average_loss == 0.0 {
                return num(100.0);
            }
            num(100.0 - 100.0 / (1.0 + average_gain / average_loss))
        }
        "tr" => {
            let true_range = true_range(state, bar);
            state.prev_close = Some(bar.close);
            num(true_range)
        }
        "atr" => {
            let len = length(args.first(), 14);
            let true_range = true_range(state, bar);
            state.prev_close = Some(bar.close);
            state.count += 1;
            let Some(average) = state.average else {
                state.seed += true_range;
                if state.count < len {
                    return na();
                }
                let seed = state.seed / len as f64;
                state.average = Some(seed);
                return num(seed);
            };
            let next = average + (true_range - average) / len as f64;
            state.average = Some(next);
            num(next)
        }
        "change" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len + 1);
            if state.window.len() <= len {
                return na();
            }
            let past = state.window[state.window.len() - 1 - len];
            num(src - past)
        }
        "highest" | "lowest" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let picked = if name == "highest" {
                state
                    .window
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            } else {
                state.window.iter().copied().fold(f64::INFINITY, f64::min)
            };
            num(picked)
        }
        "stdev" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let mean = state.window.iter().sum::<f64>() / len as f64;
            let variance = state
                .window
                .iter()
                .map(|value| (value - mean).powi(2))
                .sum::<f64>()
                / (len as f64 - 1.0).max(1.0);
            num(variance.sqrt())
        }
        "crossover" | "crossunder" => {
            let first = args.first().and_then(number)?;
            let second = args.get(1).and_then(number)?;
            let previous = state.prev.zip(state.prev_other);
            state.prev = Some(first);
            state.prev_other = Some(second);
            let Some((previous_first, previous_second)) = previous else {
                return Some(Value::Bool(false));
            };
            let crossed = if name == "crossover" {
                first > second && previous_first <= previous_second
            } else {
                first < second && previous_first >= previous_second
            };
            Some(Value::Bool(crossed))
        }
        "cum" => {
            let src = args.first().and_then(number)?;
            state.total += src;
            num(state.total)
        }
        "barssince" => {
            let cond = args
                .first()
                .and_then(number)
                .is_some_and(|value| value != 0.0);
            state.bars_since = if cond {
                Some(0)
            } else {
                state.bars_since.map(|bars| bars + 1)
            };
            match state.bars_since {
                Some(bars) => num(bars as f64),
                None => na(),
            }
        }
        "valuewhen" => {
            let cond = args
                .first()
                .and_then(number)
                .is_some_and(|value| value != 0.0);
            if cond {
                let src = args.get(1).and_then(number)?;
                state.occurrences.push(src);
                if state.occurrences.len() > MAX_WINDOW {
                    state.occurrences.remove(0);
                }
            }
            let occurrence = length(args.get(2), 1) - 1;
            match state.occurrences.iter().rev().nth(occurrence) {
                Some(value) => num(*value),
                None => na(),
            }
        }
        "pivothigh" | "pivotlow" => {
            // Pine answers once `right` future bars exist; the chart calls a study per bar in
            // order, so the answer for this bar is not known yet.
            na()
        }
        "na" => Some(Value::Bool(true)),
        _ => None,
    }
}

/// Pine's `true range`, which needs the previous close kept in `state`.
fn true_range(state: &TaState, bar: &Bar) -> f64 {
    let high_low = bar.high - bar.low;
    match state.prev_close {
        Some(previous) => high_low
            .max((bar.high - previous).abs())
            .max((bar.low - previous).abs()),
        None => high_low,
    }
}

/// Calls `math.*`; `None` for an unknown name.
pub fn math(name: &str, args: &[Value]) -> Option<Value> {
    const NAMES: &[&str] = &[
        "pi",
        "abs",
        "round",
        "floor",
        "ceil",
        "sqrt",
        "pow",
        "exp",
        "log",
        "log10",
        "sign",
        "max",
        "min",
        "avg",
        "sum",
        "todegrees",
        "toradians",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(math_known(name, args).unwrap_or(Value::Na))
}

fn math_known(name: &str, args: &[Value]) -> Option<Value> {
    let first = || args.first().and_then(number);
    let second = || args.get(1).and_then(number);
    match name {
        "pi" => Some(Value::Num(std::f64::consts::PI)),
        "abs" => num(first()?.abs()),
        "round" => {
            let value = first()?;
            let places = second().unwrap_or(0.0).clamp(0.0, 10.0);
            let factor = 10f64.powi(places as i32);
            num((value * factor).round() / factor)
        }
        "floor" => num(first()?.floor()),
        "ceil" => num(first()?.ceil()),
        "sqrt" => num(first()?.max(0.0).sqrt()),
        "pow" => num(first()?.powf(second()?)),
        "exp" => num(first()?.exp()),
        "log" => {
            let value = first()?;
            let base = second();
            num(match base {
                Some(base) => value.ln() / base.ln(),
                None => value.ln(),
            })
        }
        "log10" => num(first()?.log10()),
        "sign" => num(first()?.signum()),
        "max" => {
            let (a, b) = (first()?, second()?);
            if a.is_nan() || b.is_nan() {
                na()
            } else {
                num(a.max(b))
            }
        }
        "min" => {
            let (a, b) = (first()?, second()?);
            if a.is_nan() || b.is_nan() {
                na()
            } else {
                num(a.min(b))
            }
        }
        "avg" => {
            let values: Vec<f64> = args.iter().filter_map(number).collect();
            if values.is_empty() {
                na()
            } else {
                num(values.iter().sum::<f64>() / values.len() as f64)
            }
        }
        "sum" => {
            let values: Vec<f64> = args.iter().filter_map(number).collect();
            if values.is_empty() {
                na()
            } else {
                num(values.iter().sum())
            }
        }
        "todegrees" => num(first()?.to_degrees()),
        "toradians" => num(first()?.to_radians()),
        _ => None,
    }
}

/// Calls `color.*`; `None` for an unknown name.
pub fn color(name: &str, args: &[Value]) -> Option<Value> {
    const NAMES: &[&str] = &["new", "rgb", "r", "g", "b", "a", "from_gradient"];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(color_known(name, args).unwrap_or(Value::Na))
}

fn color_known(name: &str, args: &[Value]) -> Option<Value> {
    let base = |at: usize| {
        args.get(at)
            .and_then(Value::color)
            .unwrap_or(Color32::WHITE)
    };
    let channel = |at: usize, fallback: u8| {
        args.get(at)
            .and_then(number)
            .map(|value| value.clamp(0.0, 255.0) as u8)
            .unwrap_or(fallback)
    };
    let transparency = || {
        args.get(1)
            .and_then(number)
            .unwrap_or(0.0)
            .clamp(0.0, 100.0)
    };
    Some(match name {
        "new" => {
            let color = base(0);
            let transparent = transparency();
            let factor = 1.0 - transparent / 100.0;
            Value::Color(Color32::from_rgba_unmultiplied(
                color.r(),
                color.g(),
                color.b(),
                (f64::from(color.a()) * factor).round() as u8,
            ))
        }
        "rgb" => Value::Color(Color32::from_rgba_unmultiplied(
            channel(0, 255),
            channel(1, 255),
            channel(2, 255),
            channel(3, 255),
        )),
        "r" => Value::Num(base(0).r() as f64),
        "g" => Value::Num(base(0).g() as f64),
        "b" => Value::Num(base(0).b() as f64),
        "a" => Value::Num(base(0).a() as f64),
        "from_gradient" => {
            let value = args.first().and_then(number)?;
            let bottom = args.get(1).and_then(number)?;
            let top = args.get(2).and_then(number)?;
            let from = base(3);
            let to = base(4);
            let at = if top == bottom {
                0.0
            } else {
                ((value - bottom) / (top - bottom)).clamp(0.0, 1.0)
            };
            let mix = |first: u8, second: u8| {
                (first as f64 + (second as f64 - first as f64) * at).round() as u8
            };
            Value::Color(Color32::from_rgba_unmultiplied(
                mix(from.r(), to.r()),
                mix(from.g(), to.g()),
                mix(from.b(), to.b()),
                mix(from.a(), to.a()),
            ))
        }
        _ => return None,
    })
}

/// Calls `str.*`; `None` for an unknown name.
pub fn text(name: &str, args: &[Value]) -> Option<Value> {
    const NAMES: &[&str] = &[
        "tostring",
        "format",
        "length",
        "upper",
        "lower",
        "contains",
        "startswith",
        "endswith",
        "substring",
        "tonumber",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(text_known(name, args).unwrap_or(Value::Na))
}

fn text_known(name: &str, args: &[Value]) -> Option<Value> {
    let render = |value: &Value| match value {
        Value::Num(number) if number.is_finite() => {
            if number.fract() == 0.0 && number.abs() < 1e15 {
                format!("{}", *number as i64)
            } else {
                format!("{number}")
            }
        }
        Value::Bool(flag) => flag.to_string(),
        Value::Str(text) => text.clone(),
        Value::Color(color) => format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b()),
        Value::Num(_) | Value::Na | Value::Plot(_) => "NaN".to_owned(),
    };
    Some(match name {
        "tostring" => value_at(args, 0).map(|value| Value::Str(render(value)))?,
        "format" => {
            let template = args.first()?.text()?.to_owned();
            let mut result = template;
            for (at, value) in args.iter().skip(1).enumerate() {
                result = result.replace(&format!("{{{at}}}"), &render(value));
            }
            Value::Str(result)
        }
        "length" => Value::Num(args.first()?.text()?.chars().count() as f64),
        "upper" => Value::Str(args.first()?.text()?.to_uppercase()),
        "lower" => Value::Str(args.first()?.text()?.to_lowercase()),
        "contains" => Value::Bool(args.first()?.text()?.contains(args.get(1)?.text()?)),
        "startswith" => Value::Bool(args.first()?.text()?.starts_with(args.get(1)?.text()?)),
        "endswith" => Value::Bool(args.first()?.text()?.ends_with(args.get(1)?.text()?)),
        "substring" => {
            let text = args.first()?.text()?;
            let start = arg(args, 1).unwrap_or(0.0).max(0.0) as usize;
            let end = arg(args, 2).map(|end| end.max(0.0) as usize);
            let chars: Vec<char> = text.chars().collect();
            let end = end.unwrap_or(chars.len()).min(chars.len());
            let start = start.min(end);
            Value::Str(chars[start..end].iter().collect())
        }
        "tonumber" => value_at(args, 0)
            .and_then(Value::number)
            .or_else(|| args.first()?.text()?.trim().parse::<f64>().ok())
            .map(Value::Num)
            .unwrap_or(Value::Na),
        _ => return None,
    })
}

fn value_at(args: &[Value], at: usize) -> Option<&Value> {
    args.get(at)
}

/// Pine's `color.*` constants, which an input default often is.
pub fn color_constant(name: &str) -> Option<Color32> {
    Some(match name {
        "black" => Color32::BLACK,
        "white" => Color32::WHITE,
        "red" => Color32::RED,
        "green" => Color32::GREEN,
        "blue" => Color32::BLUE,
        "gray" => Color32::GRAY,
        "silver" => Color32::from_rgb(192, 192, 192),
        "yellow" => Color32::YELLOW,
        "orange" => Color32::from_rgb(255, 152, 0),
        "purple" => Color32::from_rgb(156, 39, 176),
        "teal" => Color32::from_rgb(0, 150, 136),
        "navy" => Color32::from_rgb(0, 0, 128),
        "aqua" => Color32::from_rgb(0, 255, 255),
        "lime" => Color32::from_rgb(0, 255, 0),
        "fuchsia" => Color32::from_rgb(255, 0, 255),
        "maroon" => Color32::from_rgb(128, 0, 0),
        "olive" => Color32::from_rgb(128, 128, 0),
        _ => return None,
    })
}
