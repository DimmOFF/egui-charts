//! A Pine Script subset on top of [`egui_charts`]: lexer, parser, evaluator, the [`PineStudy`]
//! that charts a script, and the editor panel a host can embed.
//!
//! The subset covers what a user's ordinary indicator needs: series with `[n]`, `var`, `:=`,
//! assignments and `if` values, user functions, `input.*`, `ta.*`, `math.*`, `color.*`, `str.*`,
//! `plot`, `fill`, `plotshape` and `hline`. Everything else parses into a diagnostic with its
//! line, so the editor can say what a script uses that this engine does not run.
//!
//! The engine, the [`Script`] and its [`runtime::RunOutput`] need only the data types of
//! [`egui_charts`]; `study` adds the [`Indicator`](egui_charts::studies::Indicator) adapter,
//! `store` the folder of user scripts, and `editor` the panel, so a host takes just the layers it
//! wants:
//!
//! ```toml
//! egui-charts-pine = { path = "../pine", default-features = false, features = ["study"] }
//! ```
//!
//! A host that embeds the editor hands the panel the chart each frame:
//!
//! ```text
//! let mut pine = PineScripts::load("TradeVision");
//! // In the frame:
//! let on_chart = pine.on_chart(chart);
//! if pine.show(ui, on_chart) {
//!     terminal.set_indicator_factory(pine.factory());
//! }
//! if let Some(push) = pine.take_chart_push() {
//!     chart.add_indicator(push.indicator); // or replace the series in `push.replace`
//! }
//! ```
//!
//! The editor keeps the user's scripts in the folder `store` resolves for the application: the
//! `config/pinescript/` beside a portable executable, or the per-user config folder. A host that
//! already owns storage implements [`store::ScriptStore`] and opens the panel on it instead.
//!
//! A study added from the script list, or picked by name in the indicator menu, is a
//! [`PineStudy`], so a host can also build one itself with `chart.add_indicator(Box::new(
//! PineStudy::new(source)?))`.

pub mod ast;
pub mod builtins;
#[cfg(feature = "editor")]
pub mod editor;
pub mod lexer;
pub mod parser;
pub mod runtime;
#[cfg(feature = "study")]
pub mod study;
#[cfg(feature = "store")]
pub mod store;
pub mod value;

use std::collections::HashMap;

#[cfg(feature = "editor")]
pub use editor::PineScripts;
#[cfg(feature = "study")]
pub use study::PineStudy;

use crate::ast::{Block, Diagnostic, InputDecl, NodeId, Program, Stmt};
use crate::runtime::{Context, RunOutput};
use crate::value::Value;

/// A parsed script and its current input values.
#[derive(Clone, Debug)]
pub struct Script {
    program: Program,
    values: HashMap<NodeId, Value>,
    diagnostics: Vec<Diagnostic>,
}

impl Script {
    /// Parses `source`, or fails on syntax the parser cannot read at all.
    pub fn parse(source: &str) -> Result<Self, String> {
        let program = parser::Parser::parse(source)?;
        let mut diagnostics = program.diagnostics.clone();
        collect_unsupported(&program.body, &mut diagnostics);
        dedup(&mut diagnostics);
        let values = program
            .inputs
            .iter()
            .map(|input| (input.id, input.default.clone()))
            .collect();
        Ok(Self {
            program,
            values,
            diagnostics,
        })
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The `input.*` schema, in source order.
    pub fn inputs(&self) -> &[InputDecl] {
        &self.program.inputs
    }

    /// Constructs the value of one input.
    pub fn set_input(&mut self, id: NodeId, value: Value) {
        self.values.insert(id, value);
    }

    /// Runs every bar. `diagnostics` then holds what the run itself could not do.
    pub fn run(
        &self,
        bars: &[egui_charts::model::Bar],
        context: &Context,
        sources: &[runtime::SecurityData],
    ) -> RunOutput {
        let mut output =
            runtime::Evaluator::new(&self.program, bars, context, &self.values, sources).run();
        output.diagnostics.extend(self.diagnostics.iter().cloned());
        dedup(&mut output.diagnostics);
        output
    }

    /// The notes about constructs this subset does not run, with their lines.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Collects the `Stmt::Unsupported` notes the parser left in the body.
fn collect_unsupported(block: &Block, diagnostics: &mut Vec<Diagnostic>) {
    for statement in block {
        match statement {
            Stmt::Unsupported { line, message } => diagnostics.push(Diagnostic {
                line: *line,
                message: message.clone(),
            }),
            Stmt::If {
                then, otherwise, ..
            } => {
                collect_unsupported(then, diagnostics);
                if let Some(otherwise) = otherwise {
                    collect_unsupported(otherwise, diagnostics);
                }
            }
            Stmt::For { body, .. } | Stmt::ForIn { body, .. } | Stmt::While { body, .. } => {
                collect_unsupported(body, diagnostics);
            }
            Stmt::Function(function) => collect_unsupported(&function.body, diagnostics),
            _ => {}
        }
    }
}

fn dedup(diagnostics: &mut Vec<Diagnostic>) {
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|diagnostic| seen.insert((diagnostic.line, diagnostic.message.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    fn legacy_and_modern_inputs_keep_their_defaults() {
        let script = Script::parse(
            "//@version=4\nstudy(\"W\")\nn = input(title=\"Periods\", defval=2, minval=2, type=input.integer)\nmode = input(title=\"Mode\", defval=\"3\", options=[\"3\", \"5\"])\nlen = input.int(14, \"Length\")\ncolor = input.color(color.red, \"Color\")\ntint = input.color(color.new(color.gray, 85), \"Tint\")\nbull = input.color(color.rgb(103, 154, 129), \"Bullish\")\nfg = input.color(chart.fg_color, \"Foreground\")\nfaded = input.color(color.new(chart.fg_color, 50), \"Faded\")\nhidden = input.int(5, \"Hidden\", display = display.none)\nshown = input.bool(true, \"Loud\", display = display.all)\nquiet = input.bool(true, \"Quiet\")\n",
        )
        .unwrap();
        let inputs = script.inputs();
        assert_eq!(inputs.len(), 11);
        assert_eq!(inputs[0].title, "Periods");
        assert_eq!(inputs[0].default, Value::Num(2.0));
        assert!(matches!(inputs[1].kind, ast::InputKind::Choice(_)));
        assert_eq!(inputs[2].title, "Length");
        assert_eq!(inputs[2].default, Value::Num(14.0));
        assert!(matches!(inputs[3].default, Value::Color(_)));
        // The default keeps the grey it names, at the transparency it names.
        assert_eq!(
            inputs[4].default,
            Value::Color(egui::Color32::from_rgba_unmultiplied(160, 160, 160, 38))
        );
        // `color.rgb` in a default is the colour it builds, not the white fallback.
        assert_eq!(
            inputs[5].default,
            Value::Color(egui::Color32::from_rgb(103, 154, 129))
        );
        // `chart.fg_color` is the colour the script reads at run time, in a default too.
        assert_eq!(inputs[6].default, Value::Color(crate::builtins::CHART_FG_COLOR));
        assert_eq!(
            inputs[7].default,
            Value::Color(egui::Color32::from_rgba_unmultiplied(209, 212, 220, 128))
        );
        // `display = display.none` keeps the value out of the status line, not out of settings.
        assert!(inputs[8].hidden);
        assert!(!inputs[7].hidden);
        // TradingView leaves a boolean out of the status line unless it asks to be there.
        assert!(inputs[10].hidden);
        assert!(!inputs[9].hidden);
    }

    /// A function body on its own line may start with `not`: the word negates the expression
    /// instead of being read as a type in front of a name. `not` binds looser than `==`.
    #[test]
    fn a_negated_function_body_keeps_its_not() {
        let script = Script::parse(
            "indicator(\"T\")\nf(string s) =>\n    not na(s)\nplot(f(\"x\") ? 1 : 0)\ng(string s) =>\n    not s == \"\"\nplot(g(\"x\") ? 1 : 0)\n",
        )
        .unwrap();
        let bar =
            egui_charts::model::Bar::new(chrono::DateTime::UNIX_EPOCH, 1.0, 2.0, 0.5, 1.5, 1.0);
        let output = script.run(&[bar], &Context::default(), &[]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let values: Vec<f64> = output.plots.iter().map(|plot| plot.values[0]).collect();
        assert_eq!(values, vec![1.0, 1.0]);
    }

    #[test]
    fn inputs_keep_their_group_inline_and_tooltip() {
        let script = Script::parse(
            "indicator(\"T\")\ng = \"EMA\"\nfast = input.int(3, \"Fast\", inline = \"1\", group = g, tooltip = \"Speed\")\nslow = input.int(5, \"Slow\", group = g)\n",
        )
        .unwrap();
        let inputs = script.inputs();
        assert_eq!(inputs[0].group.as_deref(), Some("EMA"));
        assert_eq!(inputs[0].inline.as_deref(), Some("1"));
        assert_eq!(inputs[0].tooltip.as_deref(), Some("Speed"));
        assert_eq!(inputs[1].group.as_deref(), Some("EMA"));
        assert_eq!(inputs[1].inline, None);
    }

    #[test]
    fn loops_parse_without_diagnostics() {
        let script =
            Script::parse("indicator(\"T\")\nfor i = 0 to 3\n    plot(close)\n").unwrap();
        assert!(script.diagnostics().is_empty(), "{:?}", script.diagnostics());
    }

    #[test]
    fn a_user_script_parses_and_runs() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../temp/pine/WICK.ED Fractals.txt"
        ));
        let Ok(source) = source else {
            // The user's scripts are not part of the repository checkout.
            return;
        };
        let script = Script::parse(&source).unwrap();
        assert_eq!(script.program().title, "Williams Fractals");
        let bars: Vec<_> = (0..10)
            .map(|at| {
                egui_charts::model::Bar::new(
                    chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(at),
                    at as f64,
                    at as f64 + 1.0,
                    at as f64 - 1.0,
                    at as f64,
                    1.0,
                )
            })
            .collect();
        let output = script.run(&bars, &Context::default(), &[]);
        assert!(output.plots.is_empty());
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    }

    #[test]
    fn parser_still_reads_a_plain_script() {
        assert!(Parser::parse("indicator(\"T\")\nplot(close)\n").is_ok());
    }

    #[test]
    fn the_sessions_script_draws_its_boxes_and_open_levels() {
        let Ok(source) = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../temp/pine/Trading Toolkit X Sessions.pine"
        )) else {
            // The user's scripts are not part of the repository checkout.
            return;
        };
        let script = Script::parse(&source).unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 5)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let bars: Vec<_> = (0..1500)
            .map(|at| {
                let close = 100.0 + (at as f64 * 0.7).sin() * 5.0;
                let open = 100.0 + ((at as f64 - 1.0) * 0.7).sin() * 5.0;
                egui_charts::model::Bar::new(
                    start + chrono::Duration::minutes(at),
                    open,
                    close.max(open) + 1.0,
                    close.min(open) - 1.0,
                    close,
                    10.0,
                )
            })
            .collect();
        let output = script.run(&bars, &Context::default(), &[]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let boxes: Vec<_> = output
            .drawings
            .iter()
            .filter(|drawing| drawing.kind == crate::runtime::DrawingKind::Box)
            .collect();
        assert!(
            boxes
                .iter()
                .any(|box_| box_.second.0 > box_.first.0 + 10.0),
            "a session box spans the session: {boxes:?}"
        );
        // The `input.color` defaults reach the boxes: the grey session tint and the orange one,
        // both at the 10% opacity the background setting asks for.
        let tints: Vec<[u8; 4]> = boxes
            .iter()
            .filter_map(|box_| box_.fill_color.map(|color| color.to_srgba_unmultiplied()))
            .collect();
        assert!(
            tints
                .iter()
                .any(|&[red, green, blue, alpha]| red == green
                    && green == blue
                    && (150..=170).contains(&red)
                    && alpha < 40),
            "the grey session tint: {tints:?}"
        );
        assert!(
            tints
                .iter()
                .any(|&[red, _, blue, alpha]| red > 200 && blue < 40 && alpha < 40),
            "the orange session tint: {tints:?}"
        );
        // `plotchar` weekday names, pinned to the pane edge. The bars span a Monday and part of
        // a Tuesday, so the Monday label is the one the day change puts on them.
        let monday = output
            .shapes
            .iter()
            .find(|shape| shape.text == "MONDAY")
            .unwrap_or_else(|| {
                panic!(
                    "{:?}",
                    output
                        .shapes
                        .iter()
                        .map(|shape| (&shape.text, shape.bar))
                        .collect::<Vec<_>>()
                )
            });
        assert!(monday.pinned, "the weekday mark sits on the pane edge");
        // Each session keeps its own name, since every call site has its own `var`s.
        for name in ["Asia", "London", "New-York"] {
            assert!(
                output
                    .drawings
                    .iter()
                    .any(|drawing| drawing.text == name),
                "{name} label: {:?}",
                output.drawings.iter().map(|d| &d.text).collect::<Vec<_>>()
            );
        }
        // The open levels draw once `isActive` dispatches on `Level` rather than on `PXHLLevel`.
        for level in ["DO", "NYM"] {
            assert!(
                output
                    .drawings
                    .iter()
                    .any(|drawing| drawing.text == level),
                "{level} line: {:?}",
                output.drawings.iter().map(|d| &d.text).collect::<Vec<_>>()
            );
        }
        // `renderLevel` offsets the label by `offset * timeframe.in_seconds()` bars, which lands
        // in the empty space past the last bar rather than on it. (`NYM` stops at its 18:00
        // cutoff, which is before the last bar here.)
        let daily_open = output
            .drawings
            .iter()
            .find(|drawing| drawing.text == "DO")
            .unwrap();
        assert!(
            daily_open.first.0 > 1499.0,
            "the DO label stays offset past the last bar: {daily_open:?}"
        );
    }

    /// The monthly high and low wait for `timeframe.change("M")`, so they hold the extremes of
    /// the bars before the new month started.
    #[test]
    fn the_sessions_script_keeps_the_previous_months_extremes() {
        let Ok(source) = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../temp/pine/Trading Toolkit X Sessions.pine"
        )) else {
            return;
        };
        let script = Script::parse(&source).unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 31)
            .unwrap()
            .and_hms_opt(20, 0, 0)
            .unwrap()
            .and_utc();
        let bars: Vec<_> = (0..120)
            .map(|at| {
                let close = 100.0 + (at as f64 * 0.7).sin() * 5.0;
                let open = 100.0 + ((at as f64 - 1.0) * 0.7).sin() * 5.0;
                egui_charts::model::Bar::new(
                    start + chrono::Duration::minutes(at * 5),
                    open,
                    close.max(open) + 1.0,
                    close.min(open) - 1.0,
                    close,
                    10.0,
                )
            })
            .collect();
        let before_february: Vec<_> = bars
            .iter()
            .filter(|bar| bar.time.format("%Y-%m").to_string() == "2026-01")
            .collect();
        let january_high = before_february
            .iter()
            .map(|bar| bar.high)
            .fold(f64::NEG_INFINITY, f64::max);
        let january_low = before_february
            .iter()
            .map(|bar| bar.low)
            .fold(f64::INFINITY, f64::min);
        let context = Context {
            timeframe: egui_charts::model::Timeframe::Min5,
            ..Context::default()
        };
        let output = script.run(&bars, &context, &[]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let price_of = |name: &str| {
            output
                .drawings
                .iter()
                .find(|drawing| drawing.text == name)
                .unwrap_or_else(|| {
                    panic!(
                        "{name}: {:?}",
                        output.drawings.iter().map(|d| &d.text).collect::<Vec<_>>()
                    )
                })
                .first
                .1
        };
        assert!(
            (price_of("PMH") - january_high).abs() < 1e-6,
            "PMH is the January high {january_high}, not {}",
            price_of("PMH")
        );
        assert!(
            (price_of("PML") - january_low).abs() < 1e-6,
            "PML is the January low {january_low}, not {}",
            price_of("PML")
        );
    }

    /// One `request.security` call site asked for several intervals — `updatePeriodData` is
    /// called with each — still answers each interval from its own higher bars.
    #[test]
    fn the_sessions_script_opens_each_level_from_its_own_period() {
        let Ok(source) = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../temp/pine/Trading Toolkit X Sessions.pine"
        )) else {
            return;
        };
        let script = Script::parse(&source).unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 26)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let bars: Vec<_> = (0..240)
            .map(|at| {
                // Every hour opens a cent above the last, so each day, week and month opens at
                // its own price.
                let open = 100.0 + at as f64 * 0.01;
                egui_charts::model::Bar::new(
                    start + chrono::Duration::hours(at),
                    open,
                    open + 0.5,
                    open + 1.5,
                    open - 1.0,
                    10.0,
                )
            })
            .collect();
        let context = Context {
            timeframe: egui_charts::model::Timeframe::Hour1,
            ..Context::default()
        };
        let output = script.run(&bars, &context, &[]);
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let price_of = |name: &str| {
            output
                .drawings
                .iter()
                .find(|drawing| drawing.text == name)
                .unwrap_or_else(|| {
                    panic!(
                        "{name}: {:?}",
                        output.drawings.iter().map(|d| &d.text).collect::<Vec<_>>()
                    )
                })
                .first
                .1
        };
        // The last bar is February 4th 23:00, its day, week and month each starting earlier.
        assert!((price_of("DO") - (100.0 + 216.0 * 0.01)).abs() < 1e-6, "{}", price_of("DO"));
        assert!((price_of("WO") - (100.0 + 168.0 * 0.01)).abs() < 1e-6, "{}", price_of("WO"));
        assert!((price_of("MO") - (100.0 + 144.0 * 0.01)).abs() < 1e-6, "{}", price_of("MO"));
    }

    #[test]
    fn every_script_in_temp_pine_parses_and_runs() {
        let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/../../temp/pine");
        let Ok(entries) = std::fs::read_dir(directory) else {
            // The user's scripts are not part of the repository checkout.
            return;
        };
        let bars: Vec<_> = (0..300)
            .map(|at| {
                let close = 100.0 + (at as f64 * 0.7).sin() * 5.0;
                // A real bar opens where the previous one closed, so a study that reads the
                // candle body sees one.
                let open = 100.0 + ((at as f64 - 1.0) * 0.7).sin() * 5.0;
                egui_charts::model::Bar::new(
                    chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(at * 5),
                    open,
                    close.max(open) + 1.0,
                    close.min(open) - 1.0,
                    close,
                    10.0 + at as f64,
                )
            })
            .collect();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let script = match Script::parse(&source) {
                Ok(script) => script,
                Err(error) => panic!("{name}: {error}"),
            };
            let output = script.run(&bars, &Context::default(), &[]);
            println!(
                "{name}: {} notes, {} inputs, {} plots, {} shapes, {} drawings, {} table rows, {} run notes: {}",
                script.diagnostics().len(),
                script.inputs().len(),
                output.plots.len(),
                output.shapes.len(),
                output.drawings.len(),
                output.tables.first().map_or(0, |table| table.rows.len()),
                output.diagnostics.len(),
                output
                    .diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.line, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
    }
}
