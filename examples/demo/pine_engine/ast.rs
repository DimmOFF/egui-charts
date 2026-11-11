//! The parsed shape of a script: statements, expressions, inputs and the diagnostics the editor
//! shows for what this subset does not run.

use crate::pine_engine::value::Value;

/// Identifies a call site, so a builtin keeps its state (`ta.ema`'s previous value) between bars
/// and an `input.*` keeps the value the user set.
pub type NodeId = u32;

/// A parse-time note about a construct this subset parses but does not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub line: usize,
    pub message: String,
}

/// What kind of edit an `input.*` call offers.
#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    Integer { min: Option<f64>, max: Option<f64> },
    Number { min: Option<f64>, max: Option<f64> },
    Toggle,
    Text,
    Color,
    Timeframe,
    Session,
    Symbol,
    Choice(Vec<String>),
}

/// One `input.*` call, in source order.
#[derive(Clone, Debug)]
pub struct InputDecl {
    pub id: NodeId,
    pub kind: InputKind,
    pub title: String,
    pub default: Value,
}

/// A call argument, named when the script names it.
#[derive(Clone, Debug)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Number(f64),
    Bool(bool),
    Str(String),
    Color(u32),
    Na,
    Ident(String),
    Member {
        target: Box<Expr>,
        name: String,
    },
    Call {
        id: NodeId,
        /// `ta.ema` is `["ta", "ema"]`; a script's own function is `["myFunc"]`.
        callee: Vec<String>,
        args: Vec<Arg>,
        line: usize,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: &'static str,
        value: Box<Expr>,
    },
    Binary {
        op: &'static str,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// Pine's `x = if cond` … `else` …, which yields the last value of the taken block.
    If {
        cond: Box<Expr>,
        then: Block,
        otherwise: Option<Block>,
    },
    /// A literal list, as `input.string` options use.
    Array(Vec<Expr>),
    /// Pine's `switch`: the body of the first arm whose condition matches the subject.
    Switch {
        /// The value the arms compare against; absent for a plain boolean `switch`.
        subject: Option<Box<Expr>>,
        arms: Vec<SwitchArm>,
    },
}

/// One branch of a `switch`.
#[derive(Clone, Debug)]
pub struct SwitchArm {
    /// The value or condition to compare; `None` for the default `=> …` arm.
    pub cond: Option<Expr>,
    /// The statements whose last value the arm answers with.
    pub body: Block,
}

pub type Block = Vec<Stmt>;

#[derive(Clone, Debug)]
pub enum Stmt {
    Assign {
        name: String,
        value: Expr,
        /// `var x = …` evaluates on the first bar only.
        sticky: bool,
    },
    If {
        cond: Expr,
        then: Block,
        otherwise: Option<Block>,
    },
    Expr(Expr),
    Function(Function),
    /// Parsed and reported, not run: loops, `switch`, `strategy.*` calls and the like.
    Unsupported {
        line: usize,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<String>,
    pub body: Block,
}

/// A whole script.
#[derive(Clone, Debug)]
pub struct Program {
    pub title: String,
    pub overlay: bool,
    pub body: Block,
    pub inputs: Vec<InputDecl>,
    pub diagnostics: Vec<Diagnostic>,
}
