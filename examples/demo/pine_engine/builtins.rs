//! The `ta.*`, `math.*`, `color.*` and `str.*` calls of the subset, with the per-call-site
//! state a series function needs. The runtime keys [`TaState`] by call site, so `ta.ema` at two
//! places in a script keeps two independent averages.

use std::collections::VecDeque;

use egui::Color32;
use egui_charts::model::Bar;

use crate::pine_engine::value::{self, Value};

/// How many past source values the windowed functions keep.
const MAX_WINDOW: usize = 8_192;

/// The largest array a script may build, which bounds the memory one can take.
const MAX_ARRAY_SIZE: usize = 100_000;

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
    prev_high: Option<f64>,
    prev_low: Option<f64>,
    window: VecDeque<f64>,
    /// A second window, for the two-series functions such as `vwma`.
    secondary: VecDeque<f64>,
    occurrences: Vec<f64>,
    bars_since: Option<usize>,
    total: f64,
    /// The running volume of `vwap` and the source sum of `vwma`.
    total_other: f64,
    /// The smoothers of the calls that run several series at once: `ta.kc`'s range,
    /// `ta.dmi`'s movements and range and its DX, and `ta.supertrend`'s ATR.
    basis: Smoothing,
    range: Smoothing,
    up: Smoothing,
    down: Smoothing,
    dx: Smoothing,
    atr: Smoothing,
    /// The band of `ta.supertrend` and the direction of both it and `ta.sar`.
    upper_band: Option<f64>,
    lower_band: Option<f64>,
    trend: Option<bool>,
    /// `ta.sar`'s value, acceleration factor and extreme point.
    sar: Option<f64>,
    sar_af: f64,
    sar_ep: Option<f64>,
}

/// One series' running average between bars, for the calls that need more than one.
#[derive(Clone, Debug, Default)]
struct Smoothing {
    average: Option<f64>,
    seed: f64,
    count: usize,
}

impl Smoothing {
    /// Wilder's smoothing, seeded with the mean of the first `len` values.
    fn rma(&mut self, value: f64, len: usize) -> Option<f64> {
        if len <= 1 {
            return Some(value);
        }
        self.count += 1;
        let Some(previous) = self.average else {
            self.seed += value;
            if self.count < len {
                return None;
            }
            let start = self.seed / len as f64;
            self.average = Some(start);
            return Some(start);
        };
        let next = previous + (value - previous) / len as f64;
        self.average = Some(next);
        Some(next)
    }

    /// Pine's EMA, seeded with the mean of the first `len` values.
    fn ema(&mut self, value: f64, len: usize) -> Option<f64> {
        if len <= 1 {
            return Some(value);
        }
        self.count += 1;
        let Some(previous) = self.average else {
            self.seed += value;
            if self.count < len {
                return None;
            }
            let start = self.seed / len as f64;
            self.average = Some(start);
            return Some(start);
        };
        let next = previous + 2.0 / (len as f64 + 1.0) * (value - previous);
        self.average = Some(next);
        Some(next)
    }
}

impl TaState {
    fn push(&mut self, value: f64, keep: usize) {
        self.window.push_back(value);
        let keep = keep.max(1).min(MAX_WINDOW);
        while self.window.len() > keep {
            self.window.pop_front();
        }
    }

    /// The same, for the second series of the two-series functions.
    fn push_other(&mut self, value: f64, keep: usize) {
        self.secondary.push_back(value);
        let keep = keep.max(1).min(MAX_WINDOW);
        while self.secondary.len() > keep {
            self.secondary.pop_front();
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
        "rising",
        "falling",
        "mom",
        "roc",
        "median",
        "variance",
        "dev",
        "highestbars",
        "lowestbars",
        "linreg",
        "swma",
        "vwma",
        "vwap",
        "obv",
        "cci",
        "cmo",
        "wpr",
        "correlation",
        "mfi",
        "cmf",
        "range",
        "percentile_linear_interpolation",
        "percentile_nearest_rank",
        "alma",
        "bb",
        "bbw",
        "kc",
        "kcw",
        "dmi",
        "adx",
        "stoch",
        "supertrend",
        "sar",
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
            let pivot_high = name == "pivothigh";
            // `ta.pivothigh(source, leftbars, rightbars)` or the two-argument high/low form.
            let (source, left_at, right_at) = if args.len() > 2 {
                (args.first().and_then(number)?, 1, 2)
            } else {
                (
                    if pivot_high { bar.high } else { bar.low },
                    0,
                    1,
                )
            };
            let left = length(args.get(left_at), 1);
            let right = length(args.get(right_at), 1);
            let span = left + right + 1;
            state.push(source, span);
            if state.window.len() < span {
                return na();
            }
            // Pine answers on the bar where the pivot is confirmed, so the candidate sits `right`
            // bars behind the last one.
            let at = state.window.len() - 1 - right;
            let candidate = state.window[at];
            let is_pivot = state.window.iter().enumerate().all(|(index, &value)| {
                index == at
                    || if pivot_high {
                        value < candidate
                    } else {
                        value > candidate
                    }
            });
            if is_pivot { num(candidate) } else { na() }
        }
        "rising" | "falling" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len + 1);
            if state.window.len() <= len {
                return Some(Value::Bool(false));
            }
            let up = name == "rising";
            let monotonic = state
                .window
                .iter()
                .zip(state.window.iter().skip(1))
                .all(|(previous, next)| if up { next > previous } else { next < previous });
            Some(Value::Bool(monotonic))
        }
        "mom" | "roc" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len + 1);
            if state.window.len() <= len {
                return na();
            }
            let past = *state.window.front()?;
            if name == "mom" {
                num(src - past)
            } else if past == 0.0 {
                na()
            } else {
                num(100.0 * (src - past) / past)
            }
        }
        "median" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let mut sorted: Vec<f64> = state.window.iter().copied().collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let middle = sorted.len() / 2;
            num(if sorted.len() % 2 == 0 {
                (sorted[middle - 1] + sorted[middle]) / 2.0
            } else {
                sorted[middle]
            })
        }
        "variance" | "dev" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let mean = state.window.iter().sum::<f64>() / len as f64;
            if name == "variance" {
                num(state
                    .window
                    .iter()
                    .map(|value| (value - mean).powi(2))
                    .sum::<f64>()
                    / (len as f64 - 1.0).max(1.0))
            } else {
                num(state
                    .window
                    .iter()
                    .map(|value| (value - mean).abs())
                    .sum::<f64>()
                    / len as f64)
            }
        }
        "highestbars" | "lowestbars" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let highest = name == "highestbars";
            let mut best: Option<(usize, f64)> = None;
            for (index, value) in state.window.iter().enumerate() {
                let better = match best {
                    None => true,
                    Some((_, current)) => {
                        if highest { *value >= current } else { *value <= current }
                    }
                };
                if better {
                    best = Some((index, *value));
                }
            }
            let (index, _) = best?;
            // Pine answers the offset in bars back, which is zero or negative.
            num(-((state.window.len() - 1 - index) as f64))
        }
        "linreg" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            let offset = arg(args, 2).unwrap_or(0.0);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let count = len as f64;
            let sum_x = (0..len).map(|x| x as f64).sum::<f64>();
            let sum_y = state.window.iter().sum::<f64>();
            let sum_xy = state
                .window
                .iter()
                .enumerate()
                .map(|(x, y)| x as f64 * y)
                .sum::<f64>();
            let sum_xx = (0..len).map(|x| (x * x) as f64).sum::<f64>();
            let denominator = count * sum_xx - sum_x * sum_x;
            if denominator == 0.0 {
                return na();
            }
            let slope = (count * sum_xy - sum_x * sum_y) / denominator;
            let intercept = (sum_y - slope * sum_x) / count;
            num(intercept + slope * (len as f64 - 1.0 - offset))
        }
        "swma" => {
            let src = args.first().and_then(number)?;
            state.push(src, 4);
            if state.window.len() < 4 {
                return na();
            }
            let weighted: f64 = state
                .window
                .iter()
                .zip([1.0, 2.0, 3.0, 4.0])
                .map(|(value, weight)| value * weight)
                .sum();
            num(weighted / 10.0)
        }
        "vwma" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src * bar.volume, len);
            state.push_other(bar.volume, len);
            if state.window.len() < len || state.secondary.len() < len {
                return na();
            }
            let volume: f64 = state.secondary.iter().sum();
            if volume == 0.0 {
                return na();
            }
            num(state.window.iter().sum::<f64>() / volume)
        }
        "vwap" => {
            let src = args.first().and_then(number)?;
            state.total += src * bar.volume;
            state.total_other += bar.volume;
            if state.total_other == 0.0 {
                return na();
            }
            num(state.total / state.total_other)
        }
        "obv" => {
            let src = args.first().and_then(number)?;
            if let Some(previous) = state.prev {
                if src > previous {
                    state.total += bar.volume;
                } else if src < previous {
                    state.total -= bar.volume;
                }
            }
            state.prev = Some(src);
            num(state.total)
        }
        "cci" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 20);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let mean = state.window.iter().sum::<f64>() / len as f64;
            let deviation = state
                .window
                .iter()
                .map(|value| (value - mean).abs())
                .sum::<f64>()
                / len as f64;
            if deviation == 0.0 {
                return na();
            }
            num((src - mean) / (0.015 * deviation))
        }
        "cmo" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 14);
            state.push(src, len + 1);
            if state.window.len() <= len {
                return na();
            }
            let mut up = 0.0;
            let mut down = 0.0;
            for (previous, next) in state.window.iter().zip(state.window.iter().skip(1)) {
                if next > previous {
                    up += next - previous;
                } else {
                    down += previous - next;
                }
            }
            let total = up + down;
            if total == 0.0 {
                return na();
            }
            num(100.0 * (up - down) / total)
        }
        "wpr" => {
            let len = length(args.first(), 14);
            state.push(bar.high, len);
            state.push_other(bar.low, len);
            if state.window.len() < len || state.secondary.len() < len {
                return na();
            }
            let highest = state.window.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let lowest = state.secondary.iter().copied().fold(f64::INFINITY, f64::min);
            if highest == lowest {
                return na();
            }
            num((highest - bar.close) / (highest - lowest) * -100.0)
        }
        "correlation" => {
            let first = args.first().and_then(number)?;
            let second = args.get(1).and_then(number)?;
            let len = length(args.get(2), 1);
            state.push(first, len);
            state.push_other(second, len);
            if state.window.len() < len || state.secondary.len() < len {
                return na();
            }
            let mean_first = state.window.iter().sum::<f64>() / len as f64;
            let mean_second = state.secondary.iter().sum::<f64>() / len as f64;
            let (mut covariance, mut first_var, mut second_var) = (0.0, 0.0, 0.0);
            for (first, second) in state.window.iter().zip(state.secondary.iter()) {
                let first = first - mean_first;
                let second = second - mean_second;
                covariance += first * second;
                first_var += first * first;
                second_var += second * second;
            }
            let denominator = (first_var * second_var).sqrt();
            if denominator == 0.0 {
                return na();
            }
            num(covariance / denominator)
        }
        "mfi" => {
            let len = length(args.first(), 14);
            let typical = (bar.high + bar.low + bar.close) / 3.0;
            let flow = typical * bar.volume;
            let (positive, negative) = match state.prev_other {
                Some(previous) if typical > previous => (flow, 0.0),
                Some(previous) if typical < previous => (0.0, flow),
                _ => (0.0, 0.0),
            };
            state.prev_other = Some(typical);
            state.push(positive, len);
            state.push_other(negative, len);
            if state.window.len() < len || state.secondary.len() < len {
                return na();
            }
            let positive: f64 = state.window.iter().sum();
            let negative: f64 = state.secondary.iter().sum();
            if negative == 0.0 {
                return num(100.0);
            }
            num(100.0 - 100.0 / (1.0 + positive / negative))
        }
        "cmf" => {
            let len = length(args.first(), 20);
            let range = bar.high - bar.low;
            let multiplier = if range == 0.0 {
                0.0
            } else {
                ((bar.close - bar.low) - (bar.high - bar.close)) / range
            };
            state.push(multiplier * bar.volume, len);
            state.push_other(bar.volume, len);
            if state.window.len() < len || state.secondary.len() < len {
                return na();
            }
            let volume: f64 = state.secondary.iter().sum();
            if volume == 0.0 {
                return na();
            }
            num(state.window.iter().sum::<f64>() / volume)
        }
        "na" => Some(Value::Bool(true)),
        "range" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let highest = state
                .window
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            let lowest = state.window.iter().copied().fold(f64::INFINITY, f64::min);
            num(highest - lowest)
        }
        "percentile_linear_interpolation" | "percentile_nearest_rank" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 1);
            let percentage = arg(args, 2)?.clamp(0.0, 100.0);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let mut sorted: Vec<f64> = state.window.iter().copied().collect();
            sorted.sort_by(f64::total_cmp);
            if name == "percentile_nearest_rank" {
                let rank = ((percentage / 100.0) * len as f64).ceil() as usize;
                return num(sorted[rank.saturating_sub(1).min(len - 1)]);
            }
            let position = percentage / 100.0 * (len as f64 - 1.0);
            let low = position.floor() as usize;
            let high = position.ceil() as usize;
            let weight = position - low as f64;
            num(sorted[low] * (1.0 - weight) + sorted[high] * weight)
        }
        "alma" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 9);
            let offset = arg(args, 2).unwrap_or(0.85);
            let sigma = arg(args, 3).unwrap_or(6.0);
            state.push(src, len);
            if state.window.len() < len {
                return na();
            }
            let centre = offset * (len as f64 - 1.0);
            let width = len as f64 / sigma.max(1e-9);
            let (mut weighted, mut weights) = (0.0, 0.0);
            for (at, value) in state.window.iter().enumerate() {
                let weight = (-((at as f64 - centre).powi(2)) / (2.0 * width * width)).exp();
                weighted += value * weight;
                weights += weight;
            }
            if weights == 0.0 {
                return na();
            }
            num(weighted / weights)
        }
        "bb" | "bbw" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 20);
            let mult = arg(args, 2).unwrap_or(2.0);
            state.push(src, len);
            let Some(basis) = mean(&state.window, len) else {
                return if name == "bb" {
                    pair(f64::NAN, f64::NAN, f64::NAN)
                } else {
                    na()
                };
            };
            let variance = state
                .window
                .iter()
                .map(|value| (value - basis).powi(2))
                .sum::<f64>()
                / len as f64;
            let deviation = variance.sqrt() * mult;
            if name == "bb" {
                pair(basis, basis + deviation, basis - deviation)
            } else if basis == 0.0 {
                na()
            } else {
                num(2.0 * deviation / basis)
            }
        }
        "kc" | "kcw" => {
            let src = args.first().and_then(number)?;
            let len = length(args.get(1), 20);
            let mult = arg(args, 2).unwrap_or(2.0);
            // Pine measures the range above the previous close, so it is taken first.
            let range = if args.get(3).and_then(Value::flag).unwrap_or(true) {
                true_range(state, bar)
            } else {
                bar.high - bar.low
            };
            state.prev_close = Some(bar.close);
            let basis = state.basis.ema(src, len);
            let range = state.range.rma(range, len);
            let (Some(basis), Some(range)) = (basis, range) else {
                return if name == "kc" {
                    pair(f64::NAN, f64::NAN, f64::NAN)
                } else {
                    na()
                };
            };
            let deviation = mult * range;
            if name == "kc" {
                pair(basis, basis + deviation, basis - deviation)
            } else if basis == 0.0 {
                na()
            } else {
                num(2.0 * deviation / basis)
            }
        }
        "dmi" | "adx" => {
            let di_len = length(args.first(), 14);
            let adx_len = length(args.get(1), 14);
            let range = true_range(state, bar);
            let (up, down) = match (state.prev_high, state.prev_low) {
                (Some(high), Some(low)) => {
                    let up_move = bar.high - high;
                    let down_move = low - bar.low;
                    (
                        if up_move > down_move && up_move > 0.0 {
                            up_move
                        } else {
                            0.0
                        },
                        if down_move > up_move && down_move > 0.0 {
                            down_move
                        } else {
                            0.0
                        },
                    )
                }
                _ => (0.0, 0.0),
            };
            state.prev_high = Some(bar.high);
            state.prev_low = Some(bar.low);
            state.prev_close = Some(bar.close);
            let up = state.up.rma(up, di_len);
            let down = state.down.rma(down, di_len);
            let range = state.range.rma(range, di_len);
            let (Some(up), Some(down), Some(range)) = (up, down, range) else {
                return if name == "dmi" {
                    pair(f64::NAN, f64::NAN, f64::NAN)
                } else {
                    na()
                };
            };
            let share = |value: f64| {
                if range == 0.0 {
                    0.0
                } else {
                    100.0 * value / range
                }
            };
            let (plus, minus) = (share(up), share(down));
            let total = plus + minus;
            let dx = if total == 0.0 {
                0.0
            } else {
                100.0 * (plus - minus).abs() / total
            };
            let adx = state.dx.rma(dx, adx_len);
            if name == "dmi" {
                pair(plus, minus, adx.unwrap_or(f64::NAN))
            } else {
                adx.map_or_else(na, |value| num(value))
            }
        }
        "stoch" => {
            let src = args.first().and_then(number)?;
            let high = args.get(1).and_then(number)?;
            let low = args.get(2).and_then(number)?;
            let len = length(args.get(3), 14);
            state.push(high, len);
            state.push_other(low, len);
            if state.window.len() < len || state.secondary.len() < len {
                return na();
            }
            let highest = state
                .window
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            let lowest = state
                .secondary
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min);
            if highest == lowest {
                return na();
            }
            num(100.0 * (src - lowest) / (highest - lowest))
        }
        "supertrend" => {
            let factor = arg(args, 0).unwrap_or(3.0);
            let len = length(args.get(1), 10);
            let range = true_range(state, bar);
            let previous_close = state.prev_close;
            state.prev_close = Some(bar.close);
            let Some(atr) = state.atr.rma(range, len) else {
                return pair(f64::NAN, f64::NAN, f64::NAN);
            };
            let mid = (bar.high + bar.low) / 2.0;
            let (upper_basic, lower_basic) = (mid + factor * atr, mid - factor * atr);
            let previous_upper = state.upper_band.unwrap_or(upper_basic);
            let previous_lower = state.lower_band.unwrap_or(lower_basic);
            // A band only moves towards the price, unless the close left it.
            let lower = if lower_basic > previous_lower
                || previous_close.is_some_and(|close| close < previous_lower)
            {
                lower_basic
            } else {
                previous_lower
            };
            let upper = if upper_basic < previous_upper
                || previous_close.is_some_and(|close| close > previous_upper)
            {
                upper_basic
            } else {
                previous_upper
            };
            let up = match state.trend {
                Some(up) if up => bar.close >= lower,
                Some(_) => bar.close <= upper,
                // Pine starts on the down direction, where the upper band is the trend line.
                None => false,
            };
            state.upper_band = Some(upper);
            state.lower_band = Some(lower);
            state.trend = Some(up);
            let line = if up { lower } else { upper };
            pair(line, if up { -1.0 } else { 1.0 }, f64::NAN)
        }
        "sar" => {
            let start = arg(args, 0).unwrap_or(0.02);
            let increment = arg(args, 1).unwrap_or(0.02);
            let maximum = arg(args, 2).unwrap_or(0.2);
            let previous_high = state.prev_high;
            let previous_low = state.prev_low;
            let mut up = state.trend.unwrap_or(true);
            let (mut sar, mut extreme, mut af) = match (state.sar, state.sar_ep) {
                (Some(sar), Some(extreme)) => (sar, extreme, state.sar_af),
                _ => (bar.low, bar.high, start),
            };
            if state.sar.is_some() {
                let next = sar + af * (extreme - sar);
                if up {
                    // The dot may not enter the two bars it just ran above.
                    let floor = previous_low.unwrap_or(bar.low).min(bar.low);
                    let next = next.min(floor);
                    if bar.low < next {
                        up = false;
                        sar = extreme;
                        extreme = bar.low;
                        af = start;
                    } else {
                        sar = next;
                        if bar.high > extreme {
                            extreme = bar.high;
                            af = (af + increment).min(maximum);
                        }
                    }
                } else {
                    let ceiling = previous_high.unwrap_or(bar.high).max(bar.high);
                    let next = next.max(ceiling);
                    if bar.high > next {
                        up = true;
                        sar = extreme;
                        extreme = bar.high;
                        af = start;
                    } else {
                        sar = next;
                        if bar.low < extreme {
                            extreme = bar.low;
                            af = (af + increment).min(maximum);
                        }
                    }
                }
            }
            state.prev_high = Some(bar.high);
            state.prev_low = Some(bar.low);
            state.prev_close = Some(bar.close);
            state.sar = Some(sar);
            state.sar_ep = Some(extreme);
            state.sar_af = af;
            state.trend = Some(up);
            num(sar)
        }
        _ => None,
    }
}

/// The mean of a full window, or `None` while it warms up.
fn mean(window: &VecDeque<f64>, len: usize) -> Option<f64> {
    (window.len() >= len).then(|| window.iter().sum::<f64>() / len as f64)
}

/// Pine's multi-value answer, which a tuple assignment reads value by value.
fn pair(first: f64, second: f64, third: f64) -> Option<Value> {
    Some(Value::array_of(vec![
        number_value(first),
        number_value(second),
        number_value(third),
    ]))
}

fn number_value(value: f64) -> Value {
    if value.is_finite() {
        Value::Num(value)
    } else {
        Value::Na
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
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "hypot",
        "clamp",
        "random",
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
        "sin" => num(first()?.sin()),
        "cos" => num(first()?.cos()),
        "tan" => num(first()?.tan()),
        "asin" => num(first()?.asin()),
        "acos" => num(first()?.acos()),
        "atan" => num(first()?.atan()),
        "hypot" => num(first()?.hypot(second()?)),
        "clamp" => {
            let value = first()?;
            let low = second()?;
            let high = args.get(2).and_then(number)?;
            num(value.max(low).min(high))
        }
        "random" => {
            let low = args.first().and_then(number).unwrap_or(0.0);
            let high = args.get(1).and_then(number).unwrap_or(1.0);
            let seed = args.get(2).and_then(number).map(|seed| seed as i64 as u64);
            num(low + random_unit(seed) * (high - low))
        }
        _ => None,
    }
}

/// A number in `[0, 1)` from a shared xorshift sequence, so a seeded script repeats itself.
fn random_unit(seed: Option<u64>) -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut state = match seed {
        Some(seed) => seed ^ 0x9E37_79B9_7F4A_7C15,
        None => STATE.load(Ordering::Relaxed),
    };
    if state == 0 {
        state = 0x2545_F491_4F6C_DD1D;
    }
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    STATE.store(state, Ordering::Relaxed);
    (state >> 11) as f64 / (1u64 << 53) as f64
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
        "split",
        "replace_all",
        "format_time",
        "pos",
        "trim",
        "repeat",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(text_known(name, args).unwrap_or(Value::Na))
}

fn text_known(name: &str, args: &[Value]) -> Option<Value> {
    let render = display;
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
        "split" => {
            let text = args.first()?.text()?;
            let separator = args.get(1).and_then(Value::text).unwrap_or("");
            let parts: Vec<Value> = if separator.is_empty() {
                text.chars().map(|part| Value::Str(part.to_string())).collect()
            } else {
                text.split(separator)
                    .map(|part| Value::Str(part.to_owned()))
                    .collect()
            };
            Value::array_of(parts)
        }
        "pos" => {
            let text = args.first()?.text()?;
            let sub = args.get(1)?.text()?;
            match text.find(sub) {
                Some(byte) => Value::Num(text[..byte].chars().count() as f64),
                None => Value::Na,
            }
        }
        "trim" => {
            let text = args.first()?.text()?;
            let left = args.get(1).and_then(Value::flag).unwrap_or(true);
            let right = args.get(2).and_then(Value::flag).unwrap_or(true);
            let trimmed = match (left, right) {
                (true, true) => text.trim(),
                (true, false) => text.trim_start(),
                (false, true) => text.trim_end(),
                (false, false) => text,
            };
            Value::Str(trimmed.to_owned())
        }
        "repeat" => {
            let text = args.first()?.text()?;
            let count = arg(args, 1).unwrap_or(0.0).max(0.0).min(1_000.0) as usize;
            // Clamp: a stray large count would allocate gigabytes.
            let text: String = text.chars().take(1_000).collect();
            Value::Str(text.repeat(count))
        }
        "replace_all" => {
            let text = args.first()?.text()?;
            let from = args.get(1).and_then(Value::text)?;
            let to = args.get(2).and_then(Value::text).unwrap_or("");
            Value::Str(text.replace(from, to))
        }
        "format_time" => {
            let millis = args.first().and_then(Value::number)?;
            let pattern = args.get(1).and_then(Value::text).unwrap_or("yyyy-MM-dd");
            let zone = args
                .get(2)
                .and_then(Value::text)
                .and_then(egui_charts::model::TimezoneId::named)
                .unwrap_or_else(egui_charts::model::TimezoneId::utc);
            let Some(time) = chrono::DateTime::from_timestamp_millis(millis as i64) else {
                return Some(Value::Na);
            };
            Value::Str(
                time.with_timezone(&zone)
                    .format(&pine_time_format(pattern))
                    .to_string(),
            )
        }
        _ => return None,
    })
}

/// Translates Pine's date tokens (`yyyy`, `MMM`, `HH`) into chrono's `strftime` ones.
/// Other characters pass through, so `dd MMM yyyy` reads `%d %b %Y`.
fn pine_time_format(pattern: &str) -> String {
    let characters: Vec<char> = pattern.chars().collect();
    let mut result = String::new();
    let mut at = 0;
    while at < characters.len() {
        let run: String = characters[at..]
            .iter()
            .take_while(|character| **character == characters[at])
            .collect();
        let replacement = match run.as_str() {
            "yyyy" => Some("%Y"),
            "yy" => Some("%y"),
            "MMMM" => Some("%B"),
            "MMM" => Some("%b"),
            "MM" => Some("%m"),
            "M" => Some("%-m"),
            "dd" => Some("%d"),
            "d" => Some("%-d"),
            "EEEE" => Some("%A"),
            "EEE" | "EE" => Some("%a"),
            "HH" => Some("%H"),
            "H" => Some("%-H"),
            "hh" => Some("%I"),
            "h" => Some("%-I"),
            "mm" => Some("%M"),
            "m" => Some("%-M"),
            "ss" => Some("%S"),
            "s" => Some("%-S"),
            "a" => Some("%p"),
            _ => None,
        };
        match replacement {
            Some(replacement) => result.push_str(replacement),
            None => result.push_str(&run),
        }
        at += run.chars().count();
    }
    result
}

fn value_at(args: &[Value], at: usize) -> Option<&Value> {
    args.get(at)
}

/// How a value reads in text, as `str.tostring` renders it.
pub fn display(value: &Value) -> String {
    match value {
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
        Value::Num(_) | Value::Na | Value::Array(_) | Value::Object(_) | Value::Plot(_) => {
            "NaN".to_owned()
        }
    }
}

/// Calls `array.*`; `None` for a name this subset does not run.
pub fn array(name: &str, args: &[Value]) -> Option<Value> {
    const NAMES: &[&str] = &[
        "new",
        "new_int",
        "new_float",
        "new_bool",
        "new_string",
        "new_color",
        "new_label",
        "new_line",
        "new_box",
        "new_table",
        "from",
        "push",
        "pop",
        "get",
        "set",
        "size",
        "first",
        "last",
        "clear",
        "remove",
        "insert",
        "shift",
        "unshift",
        "copy",
        "fill",
        "concat",
        "reverse",
        "slice",
        "includes",
        "indexof",
        "lastindexof",
        "join",
        "sum",
        "avg",
        "min",
        "max",
        "median",
        "stdev",
        "variance",
        "range",
        "sort",
        "max_index",
        "min_index",
        "abs",
        "mode",
        "percentile_linear_interpolation",
        "percentile_nearest_rank",
        "binary_search",
        "binary_search_leftmost",
        "binary_search_rightmost",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some(array_known(name, args).unwrap_or(Value::Na))
}

fn array_known(name: &str, args: &[Value]) -> Option<Value> {
    let target = || args.first().and_then(Value::array);
    let index_at = |at: usize| -> Option<usize> {
        let value = args.get(at).and_then(number)?;
        if value < 0.0 {
            None
        } else {
            Some((value.round() as usize).min(MAX_ARRAY_SIZE))
        }
    };
    Some(match name {
        "new" | "new_int" | "new_float" | "new_bool" | "new_string" | "new_color" | "new_label"
        | "new_line" | "new_box" | "new_table" => {
            let size = index_at(0).unwrap_or(0);
            let initial = args.get(1).cloned().unwrap_or(Value::Na);
            new_array(vec![initial; size])
        }
        "from" => new_array(args.to_vec()),
        "push" => {
            let items = target()?;
            let mut items = value::lock(&items);
            for value in args.iter().skip(1) {
                items.push(value.clone());
            }
            Value::Na
        }
        "pop" => {
            let items = target()?;
            value::lock(&items).pop().unwrap_or(Value::Na)
        }
        "shift" => {
            let items = target()?;
            let mut items = value::lock(&items);
            if items.is_empty() {
                Value::Na
            } else {
                items.remove(0)
            }
        }
        "unshift" => {
            let items = target()?;
            let mut items = value::lock(&items);
            for (at, value) in args.iter().skip(1).enumerate() {
                let at = at.min(items.len());
                items.insert(at, value.clone());
            }
            Value::Na
        }
        "get" => {
            let items = target()?;
            let items = value::lock(&items);
            index_at(1)
                .and_then(|at| items.get(at).cloned())
                .unwrap_or(Value::Na)
        }
        "set" => {
            let items = target()?;
            let mut items = value::lock(&items);
            let at = index_at(1);
            if let Some(at) = at
                && at < items.len()
            {
                items[at] = args.get(2).cloned().unwrap_or(Value::Na);
            }
            Value::Na
        }
        "size" => {
            let items = target()?;
            let size = value::lock(&items).len();
            Value::Num(size as f64)
        }
        "first" => {
            let items = target()?;
            value::lock(&items).first().cloned().unwrap_or(Value::Na)
        }
        "last" => {
            let items = target()?;
            value::lock(&items).last().cloned().unwrap_or(Value::Na)
        }
        "clear" => {
            let items = target()?;
            value::lock(&items).clear();
            Value::Na
        }
        "remove" => {
            let items = target()?;
            let mut items = value::lock(&items);
            let at = index_at(1);
            match at {
                Some(at) if at < items.len() => items.remove(at),
                _ => Value::Na,
            }
        }
        "insert" => {
            let items = target()?;
            let mut items = value::lock(&items);
            if let Some(at) = index_at(1) {
                let at = at.min(items.len());
                items.insert(at, args.get(2).cloned().unwrap_or(Value::Na));
            }
            Value::Na
        }
        "copy" => {
            let items = target()?;
            let copy = value::lock(&items).clone();
            new_array(copy)
        }
        "fill" => {
            let items = target()?;
            let value = args.get(1).cloned().unwrap_or(Value::Na);
            for item in value::lock(&items).iter_mut() {
                *item = value.clone();
            }
            Value::Na
        }
        "concat" => {
            let items = target()?;
            let mut result = value::lock(&items).clone();
            if let Some(second) = args.get(1).and_then(Value::array) {
                // A copy first: `array.concat(a, a)` must not lock the same array twice.
                let tail = value::lock(&second).clone();
                result.extend(tail);
            }
            new_array(result)
        }
        "reverse" => {
            let items = target()?;
            value::lock(&items).reverse();
            Value::Na
        }
        "slice" => {
            let items = target()?;
            let items = value::lock(&items);
            let from = index_at(1).unwrap_or(0).min(items.len());
            let to = index_at(2)
                .unwrap_or(items.len())
                .min(items.len())
                .max(from);
            new_array(items[from..to].to_vec())
        }
        "includes" => {
            let items = target()?;
            let items = value::lock(&items);
            let found = args.get(1).is_some_and(|value| items.contains(value));
            Value::Bool(found)
        }
        "indexof" | "lastindexof" => {
            let items = target()?;
            let items = value::lock(&items);
            let found = args.get(1).and_then(|value| {
                if name == "indexof" {
                    items.iter().position(|item| item == value)
                } else {
                    items.iter().rposition(|item| item == value)
                }
            });
            Value::Num(found.map_or(-1.0, |at| at as f64))
        }
        "join" => {
            let items = target()?;
            let items = value::lock(&items);
            let separator = args.get(1).and_then(Value::text).unwrap_or("");
            Value::Str(
                items
                    .iter()
                    .map(display)
                    .collect::<Vec<_>>()
                    .join(separator),
            )
        }
        "abs" => {
            let items = target()?;
            for item in value::lock(&items).iter_mut() {
                if let Value::Num(number) = item {
                    *item = Value::Num(number.abs());
                }
            }
            Value::Na
        }
        "sum" | "avg" | "min" | "max" | "median" | "stdev" | "variance" | "range" => {
            let items = target()?;
            let items = value::lock(&items);
            let values: Vec<f64> = items.iter().filter_map(number).collect();
            if values.is_empty() {
                return Some(Value::Na);
            }
            let result = match name {
                "sum" => values.iter().sum(),
                "avg" => values.iter().sum::<f64>() / values.len() as f64,
                "min" => values.iter().copied().fold(f64::INFINITY, f64::min),
                "max" => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                "median" => {
                    let mut sorted = values.clone();
                    sorted.sort_by(|a, b| a.total_cmp(b));
                    let middle = sorted.len() / 2;
                    if sorted.len() % 2 == 0 {
                        (sorted[middle - 1] + sorted[middle]) / 2.0
                    } else {
                        sorted[middle]
                    }
                }
                "variance" | "stdev" => {
                    let mean = values.iter().sum::<f64>() / values.len() as f64;
                    let biased = args.get(1).and_then(Value::flag).unwrap_or(true);
                    let divisor = if biased || values.len() < 2 {
                        values.len() as f64
                    } else {
                        values.len() as f64 - 1.0
                    };
                    let variance = values
                        .iter()
                        .map(|value| (value - mean).powi(2))
                        .sum::<f64>()
                        / divisor;
                    if name == "stdev" {
                        variance.sqrt()
                    } else {
                        variance
                    }
                }
                "range" => {
                    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
                    max - min
                }
                _ => return None,
            };
            if result.is_finite() {
                Value::Num(result)
            } else {
                Value::Na
            }
        }
        "sort" => {
            let items = target()?;
            let descending = match args.get(1) {
                Some(Value::Str(order)) => order == "descending",
                Some(value) => value.number().is_some_and(|order| order == 0.0),
                None => false,
            };
            let mut items = value::lock(&items);
            items.sort_by(|a, b| match (number(a), number(b)) {
                (Some(a), Some(b)) => a.total_cmp(&b),
                _ => std::cmp::Ordering::Equal,
            });
            if descending {
                items.reverse();
            }
            Value::Na
        }
        "max_index" | "min_index" => {
            let items = target()?;
            let items = value::lock(&items);
            let found = items
                .iter()
                .enumerate()
                .filter_map(|(at, value)| number(value).map(|value| (at, value)))
                .reduce(|best, next| {
                    let better = if name == "max_index" {
                        next.1 > best.1
                    } else {
                        next.1 < best.1
                    };
                    if better { next } else { best }
                });
            Value::Num(found.map_or(-1.0, |(at, _)| at as f64))
        }
        "mode" => {
            let items = target()?;
            let items = value::lock(&items);
            // The value seen most often; on a tie the one seen first, as Pine answers.
            let mut counts: Vec<(Value, usize)> = Vec::new();
            for item in items.iter() {
                match counts.iter_mut().find(|(value, _)| value == item) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((item.clone(), 1)),
                }
            }
            counts
                .into_iter()
                .max_by_key(|(_, count)| *count)
                .map(|(value, _)| value)
                .unwrap_or(Value::Na)
        }
        "percentile_linear_interpolation" | "percentile_nearest_rank" => {
            let items = target()?;
            let percentage = arg(args, 1)?.clamp(0.0, 100.0);
            let items = value::lock(&items);
            let mut sorted: Vec<f64> = items.iter().filter_map(number).collect();
            if sorted.is_empty() {
                return Some(Value::Na);
            }
            sorted.sort_by(f64::total_cmp);
            let len = sorted.len();
            if name == "percentile_nearest_rank" {
                let rank = ((percentage / 100.0) * len as f64).ceil() as usize;
                return num(sorted[rank.saturating_sub(1).min(len - 1)]);
            }
            let position = percentage / 100.0 * (len as f64 - 1.0);
            let low = position.floor() as usize;
            let high = position.ceil() as usize;
            let weight = position - low as f64;
            num(sorted[low] * (1.0 - weight) + sorted[high] * weight).unwrap_or(Value::Na)
        }
        "binary_search" | "binary_search_leftmost" | "binary_search_rightmost" => {
            let items = target()?;
            let needle = arg(args, 1)?;
            let items = value::lock(&items);
            let mut values: Vec<f64> = items.iter().filter_map(number).collect();
            if values.is_empty() {
                return Some(Value::Na);
            }
            values.sort_by(f64::total_cmp);
            let found = values.binary_search_by(|value| value.total_cmp(&needle));
            let index = match (name, found) {
                ("binary_search", Ok(at)) => at as i64,
                ("binary_search", Err(_)) => -1,
                // A miss answers with the neighbour the value would sit beside.
                ("binary_search_leftmost", Ok(at)) => at as i64,
                ("binary_search_leftmost", Err(at)) => at.min(values.len() - 1) as i64,
                (_, Ok(at)) => at as i64,
                (_, Err(at)) => at.saturating_sub(1) as i64,
            };
            num(index as f64).unwrap_or(Value::Na)
        }
        _ => return None,
    })
}

/// A new array value around `values`.
fn new_array(values: Vec<Value>) -> Value {
    Value::array_of(values)
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
