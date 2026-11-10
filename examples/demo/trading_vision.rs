//! Full egui-charts terminal on Deriv's and Binance's public endpoints, plus the local
//! MetaTrader 5 terminal when one runs.
//!
//! Run natively with `cargo run --release --bin TradingVision`.

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

use catalog::Catalog;
use combined::{CombinedFeed, CombinedQuotes};
use deriv_services::FileStorage;
use egui_charts::model::Timeframe;
use egui_charts::services::AlertEngine;
use egui_charts::ui::export::ExportFormat;
use egui_charts_terminal::{ExportContents, Terminal};
use std::sync::Arc;

const SYMBOL: &str = "R_100";

/// How long a closing window waits for the pending writes to land.
const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn main() -> eframe::Result {
    // rustls cannot pick a provider when dependency feature sets are unified; use the ring one.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Every venue catalogue loads in the background; the search waits for them.
    let catalog = Catalog::load();
    let deriv_symbols = catalog.deriv();
    let binance_symbols = catalog.binance();
    let mt5_symbols = catalog.mt5();
    let mt5 = catalog.connection();
    let mut terminal = Terminal::new(Arc::new(CombinedFeed::new(
        SYMBOL,
        deriv_symbols,
        binance_symbols.clone(),
        mt5_symbols.clone(),
        mt5.clone(),
    )))
    .with_symbol(SYMBOL)
    .with_timeframe(Timeframe::Min1)
    .with_symbol_search(Arc::new(catalog))
    .with_symbol_recents(false)
    .with_quotes(Arc::new(CombinedQuotes::new(
        binance_symbols,
        mt5_symbols,
        mt5,
    )))
    .with_alerts(Arc::new(AlertEngine::new()))
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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 900.0]),
        ..Default::default()
    };
    let mut flushing = false;
    let mut deadline: Option<std::time::Instant> = None;
    eframe::run_ui_native(
        "TradingVision: Deriv + Binance",
        options,
        move |ui, _frame| {
            // Actions the terminal leaves to its host: host entries and those without a default.
            for action in terminal.show(ui) {
                let _ = action;
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
