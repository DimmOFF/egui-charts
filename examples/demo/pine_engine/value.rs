//! One value of a running script: numbers cover ints, and `Na` is Pine's missing value.

use egui::Color32;

/// A script value. Pine's int and float are both numbers here, its `na` is [`Value::Na`].
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Bool(bool),
    Str(String),
    Color(Color32),
    /// Pine's `na`.
    Na,
    /// The plot a `plot()` call declared, for `fill()`.
    Plot(usize),
}

impl Value {
    pub fn number(&self) -> Option<f64> {
        match self {
            Value::Num(number) if number.is_finite() => Some(*number),
            Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    pub fn flag(&self) -> Option<bool> {
        match self {
            Value::Bool(flag) => Some(*flag),
            Value::Num(number) => Some(*number != 0.0),
            Value::Na => Some(false),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn color(&self) -> Option<Color32> {
        match self {
            Value::Color(color) => Some(*color),
            _ => None,
        }
    }
}
