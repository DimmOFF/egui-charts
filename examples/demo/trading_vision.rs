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
use egui_charts::model::Timeframe;
use egui_charts::services::AlertEngine;
use egui_charts::ui::export::ExportFormat;
use egui_charts::ui::top_toolbar::ToolbarEntry;
use egui_charts_terminal::{ExportContents, Terminal};
use pine_editor::PineScripts;
use std::sync::Arc;

const SYMBOL: &str = "R_100";

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
            // Actions the terminal leaves to its host: host entries and those without a default.
            for action in terminal.show(ui) {
                if let egui_charts_terminal::Action::TopToolbar(
                    egui_charts::ui::top_toolbar::TopToolbarAction::Host(id),
                ) = &action
                    && id == "pine"
                {
                    pine.open = true;
                }
            }
            // A recompiled script replaces the one the indicator dialog lists.
            if pine.show(ui.ctx()) {
                terminal.set_indicator_factory(pine.factory());
            }
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
    let mt5_symbols = catalog.mt5();
    let mt5 = catalog.connection();
    let mut terminal = Terminal::new(Arc::new(CombinedFeed::new(
        SYMBOL,
        catalog.deriv(),
        binance_symbols.clone(),
        mt5_symbols.clone(),
        mt5.clone(),
    )))
    .with_symbol(SYMBOL)
    .with_timeframe(Timeframe::Min1)
    .with_symbol_search(catalog)
    .with_symbol_recents(false)
    .with_quotes(Arc::new(CombinedQuotes::new(
        binance_symbols,
        mt5_symbols,
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
    terminal
        .top_toolbar_mut()
        .host_entries
        .push(ToolbarEntry::new("pine", "Pine"));
    terminal
}
