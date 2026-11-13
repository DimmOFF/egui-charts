//! One value of a running script: numbers cover ints, and `Na` is Pine's missing value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use egui::Color32;

/// A Pine lambda: its parameters and the expression its body evaluates.
#[derive(Clone, Debug)]
pub struct Lambda {
    pub params: Vec<String>,
    pub body: crate::pine_engine::ast::Expr,
}

/// A Pine array, shared by every variable that refers to it, as Pine arrays are references.
/// The chart runs a script off the UI thread, so the shared cell is a `Mutex`.
pub type ArrayRef = Arc<Mutex<Vec<Value>>>;

/// A user-defined type's instance: its fields by name.
pub type ObjectRef = Arc<Mutex<HashMap<String, Value>>>;

/// The contents of an array, whichever thread last wrote them.
pub fn lock(items: &ArrayRef) -> MutexGuard<'_, Vec<Value>> {
    items.lock().unwrap_or_else(|error| error.into_inner())
}

/// The fields of an object, whichever thread last wrote them.
pub fn lock_object(object: &ObjectRef) -> MutexGuard<'_, HashMap<String, Value>> {
    object.lock().unwrap_or_else(|error| error.into_inner())
}

/// A script value. Pine's int and float are both numbers here, its `na` is [`Value::Na`].
#[derive(Clone, Debug)]
pub enum Value {
    Num(f64),
    Bool(bool),
    Str(String),
    Color(Color32),
    /// Pine's `na`.
    Na,
    /// A Pine array or the tuple a call answers with.
    Array(ArrayRef),
    /// An instance of a user-defined `type`.
    Object(ObjectRef),
    /// A lambda passed to `array.every`, `array.some` and their siblings.
    Lambda(Arc<Lambda>),
    /// The plot a `plot()` call declared, for `fill()`.
    Plot(usize),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Num(first), Value::Num(second)) => first == second,
            (Value::Bool(first), Value::Bool(second)) => first == second,
            (Value::Str(first), Value::Str(second)) => first == second,
            (Value::Color(first), Value::Color(second)) => first == second,
            // Pine compares arrays by reference, which also keeps the comparison lock-free.
            (Value::Array(first), Value::Array(second)) => Arc::ptr_eq(first, second),
            (Value::Object(first), Value::Object(second)) => Arc::ptr_eq(first, second),
            (Value::Lambda(first), Value::Lambda(second)) => Arc::ptr_eq(first, second),
            (Value::Plot(first), Value::Plot(second)) => first == second,
            (Value::Na, Value::Na) => true,
            _ => false,
        }
    }
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

    /// The array this value is, when it is one.
    pub fn array(&self) -> Option<ArrayRef> {
        match self {
            Value::Array(items) => Some(items.clone()),
            _ => None,
        }
    }

    /// A new array value around `items`.
    pub fn array_of(items: Vec<Value>) -> Self {
        Value::Array(Arc::new(Mutex::new(items)))
    }

    /// The object this value is, when it is one.
    pub fn object(&self) -> Option<ObjectRef> {
        match self {
            Value::Object(fields) => Some(fields.clone()),
            _ => None,
        }
    }

    /// The lambda this value is, when it is one.
    pub fn lambda(&self) -> Option<Arc<Lambda>> {
        match self {
            Value::Lambda(lambda) => Some(lambda.clone()),
            _ => None,
        }
    }

    /// A new object with `fields`.
    pub fn object_of(fields: HashMap<String, Value>) -> Self {
        Value::Object(Arc::new(Mutex::new(fields)))
    }
}
