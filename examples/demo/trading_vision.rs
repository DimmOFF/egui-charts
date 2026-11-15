//! Full egui-charts terminal on Deriv's and Binance's public endpoints, plus the local
//! MetaTrader 5 terminal when one runs.
//!
//! Run natively with `cargo run --release --bin TradingVision`.

// The release build is a window, not a console application. Debug keeps the console for its logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[path = "binance_feed.rs"]
mod binance_feed;
#[path = "catalog.rs"]
mod catalog;
#[path = "combined.rs"]
mod combined;
#[path = "deriv_feed.rs"]
mod deriv_feed;
#[path = "deriv_services.rs"]
mod deriv_services;
#[path = "mt5_feed.rs"]
mod mt5_feed;
#[path = "pine_editor.rs"]
mod pine_editor;
#[path = "pine_engine/mod.rs"]
mod pine_engine;

use catalog::Catalog;
use combined::{CombinedFeed, CombinedQuotes};
use deriv_services::FileStorage;
use egui_charts::icons::icons;
use egui_charts::model::Timeframe;
use egui_charts::services::AlertEngine;
use egui_charts::ui::export::ExportFormat;
use egui_charts::ui::widget_bar::{WidgetBarSection, WidgetBarWidget};
use egui_charts::widget::SeriesId;
use egui_charts::widget::settings::ChartSettingsState;
use egui_charts_terminal::{ExportContents, Terminal};
use pine_editor::PineScripts;
use std::sync::Arc;

const SYMBOL: &str = "R_100";

/// The right-rail entry that opens the Pine editor; the terminal's own host widgets use ids near
/// `u64::MAX`.
const PINE_WIDGET: u64 = 1;

/// How long a closing window waits for the pending writes to land.
const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The window and taskbar icon, from the asset the build also puts on the executable.
fn window_icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!("assets/icon.png"))
        .expect("assets/icon.png decodes")
        .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

/// How long the window waits for the catalogues before it opens with what arrived.
const LOAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(15);

fn main() -> eframe::Result {
    // rustls cannot pick a provider when dependency feature sets are unified; use the ring one.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // The catalogues decide which venue serves a symbol. Opening the window before they arrive
    // sent a saved Binance or MT5 symbol to Deriv, which answered `InvalidSymbol`, and the chart
    // only recovered when the symbol was picked again.
    let catalog = Arc::new(Catalog::load());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_icon(Arc::new(window_icon())),
        ..Default::default()
    };
    let mut terminal: Option<Terminal> = None;
    let mut pine = PineScripts::load();
    let started = std::time::Instant::now();
    let mut flushing = false;
    let mut deadline: Option<std::time::Instant> = None;
    let mut scale_off = false;
    eframe::run_ui_native(
        "TradingVision: Deriv + Binance + MT5",
        options,
        move |ui, _frame| {
            if terminal.is_none() {
                if !catalog.is_ready() && started.elapsed() < LOAD_LIMIT {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                    });
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                terminal = Some(build_terminal(catalog.clone(), pine.factory()));
            }
            let terminal = terminal.as_mut().expect("the terminal was built above");
            // A docked editor takes its width from the window before the terminal draws its
            // frame, so the terminal's right rail ends up to its left, as in the reference.
            let on_chart = pine.on_chart(terminal.chart());
            if pine.show(ui, on_chart) {
                terminal.set_indicator_factory(pine.factory());
            }
            // Add to chart and Update compile the script and hand it over from the editor.
            if let Some(push) = pine.take_chart_push() {
                let chart = terminal.chart_mut();
                if let Some(replace) = push.replace {
                    chart.remove_series(replace);
                }
                chart.add_indicator(push.indicator);
            }
            // Actions the terminal leaves to its host: host entries and those without a default.
            for action in terminal.show(ui) {
                match &action {
                    egui_charts_terminal::Action::WidgetBar(
                        egui_charts::ui::widget_bar::WidgetBarAction::WidgetClicked(PINE_WIDGET),
                    ) => pine.open = !pine.open,
                    _ => {}
                }
            }
            // Charts open with a manual price scale. The empty chart has no range to freeze
            // yet, so this waits for its first bars. Once only: afterwards the user's own
            // auto-scale choice stands.
            if !scale_off && !terminal.chart().bars(SeriesId::MAIN).is_empty() {
                scale_off = true;
                let mut settings = ChartSettingsState::from_chart(terminal.chart());
                settings.scales_and_lines.auto_scale = false;
                if let Err(error) = settings.apply_to_chart(terminal.chart_mut()) {
                    eprintln!("Could not turn auto-scale off: {error}");
                }
            }
            // The rail entry stays lit exactly while the editor is open, however it was closed.
            terminal.widget_bar_mut().active_widget = pine.open.then_some(PINE_WIDGET);
            // Keep the window open until the pending writes land, at most `FLUSH_TIMEOUT`.
            if ui.input(|input| input.viewport().close_requested()) && !flushing {
                flushing = true;
                deadline = Some(std::time::Instant::now() + FLUSH_TIMEOUT);
            }
            if !flushing {
                return;
            }
            let too_late = deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
            if terminal.flush_storage() || too_late {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ui.ctx().request_repaint();
            }
        },
    )
}

/// The terminal every venue serves symbols through.
fn build_terminal(
    catalog: Arc<Catalog>,
    factory: egui_charts::studies::IndicatorFactory,
) -> Terminal {
    let binance_symbols = catalog.binance();
    let deriv_symbols = catalog.deriv();
    let mt5 = catalog.mt5();
    let mut terminal = Terminal::new(Arc::new(CombinedFeed::new(
        SYMBOL,
        deriv_symbols.clone(),
        binance_symbols.clone(),
        Arc::clone(&mt5),
    )))
    .with_symbol(SYMBOL)
    .with_timeframe(Timeframe::Min1)
    .with_symbol_search(catalog)
    .with_symbol_recents(false)
    .with_quotes(Arc::new(CombinedQuotes::new(
        binance_symbols,
        deriv_symbols,
        mt5,
    )))
    .with_alerts(Arc::new(AlertEngine::new()))
    .with_indicator_factory(factory)
    .with_storage(Arc::new(FileStorage::open()))
    .with_file_export(
        &[ExportFormat::Png, ExportFormat::Svg, ExportFormat::Csv],
        |file| {
            let path = file
                .path
                .unwrap_or_else(|| std::env::temp_dir().join(&file.name));
            let written = match file.contents {
                ExportContents::Png(bytes) => std::fs::write(&path, bytes),
                ExportContents::Text(text) => std::fs::write(&path, text),
                _ => return Err("Unsupported export contents".into()),
            };
            written.map_err(|error| format!("Could not save {}: {error}", path.display()))
        },
    );
    // Deriv's public candles carry no volume; Binance's do.
    terminal.chart_mut().config_mut().show_volume = false;
    // The values tooltip card is off; the legend still shows the hovered bar's values.
    terminal.chart_mut().set_tooltip_enabled(false);
    // TradingView-like wheel zoom: the plain wheel keeps the right bar in place, Ctrl/Cmd zooms
    // around the pointer.
    let mut chart_options = terminal.chart().chart_options().clone();
    chart_options.time_scale.right_bar_stays_on_scroll = true;
    terminal.chart_mut().set_chart_options(chart_options);
    // The Pine editor sits on the right rail, as in the reference terminal.
    terminal.widget_bar_mut().widgets.push(WidgetBarWidget {
        id: PINE_WIDGET,
        icon: icons::CODE,
        tooltip: "Pine Editor".into(),
        section: WidgetBarSection::Bottom,
    });
    terminal
}
