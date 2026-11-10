//! One feed and one quote stream over every venue: the Binance catalogue goes to Binance, the MT5
//! catalogue to the local terminal, every other symbol to Deriv.

use egui_charts::data::{
    Datafeed, FeedCapabilities, HistoryPage, HistoryRequest, Reply, Subscription,
};
use egui_charts::model::{Instrument, Quote};
use egui_charts::services::{Events, Quotes};

use super::binance_feed::{BinanceFeed, BinanceQuotes, is_futures};
use super::catalog::Symbols;
use super::deriv_feed::{DerivFeed, TIMEFRAMES};
use super::deriv_services::DerivQuotes;
use super::mt5_feed::{Connection, Mt5Feed, Mt5Quotes};

/// Whether `symbol` trades on Binance: in its catalogue, or a futures pair before the catalogue
/// finishes loading.
fn on_binance(symbols: &Symbols, symbol: &str) -> bool {
    is_futures(symbol) || symbols.contains(symbol)
}

pub struct CombinedFeed {
    deriv: DerivFeed,
    binance: BinanceFeed,
    mt5: Mt5Feed,
    /// The Binance catalogue, filled when the symbol search loads it.
    binance_symbols: Symbols,
}

impl CombinedFeed {
    /// A feed whose Deriv requests default to `deriv_symbol`.
    pub fn new(
        deriv_symbol: impl Into<String>,
        deriv_symbols: Symbols,
        binance_symbols: Symbols,
        mt5_symbols: Symbols,
        mt5: Option<Connection>,
    ) -> Self {
        Self {
            deriv: DerivFeed::new(deriv_symbol, deriv_symbols),
            binance: BinanceFeed::new(binance_symbols.clone()),
            mt5: Mt5Feed::new(mt5_symbols, mt5),
            binance_symbols,
        }
    }

    /// Whether `symbol` comes from the local MetaTrader 5 terminal.
    fn on_mt5(&self, symbol: &str) -> bool {
        !on_binance(&self.binance_symbols, symbol) && self.mt5.holds(symbol)
    }
}

impl Datafeed for CombinedFeed {
    fn capabilities(&self, reply: Reply<FeedCapabilities>) {
        reply.send(Ok(FeedCapabilities::new()
            .with_timeframes(TIMEFRAMES.iter().copied())
            .with_exchanges([
                "Deriv".to_owned(),
                "Binance".to_owned(),
                "Binance Futures".to_owned(),
                "MT5".to_owned(),
            ])
            .with_symbol_types([
                egui_charts::model::SymbolType::Index,
                egui_charts::model::SymbolType::Forex,
                egui_charts::model::SymbolType::Crypto,
                egui_charts::model::SymbolType::Commodity,
            ])));
    }

    fn resolve(&self, symbol: &str, reply: Reply<Instrument>) {
        if on_binance(&self.binance_symbols, symbol) {
            self.binance.resolve(symbol, reply);
        } else if self.on_mt5(symbol) {
            self.mt5.resolve(symbol, reply);
        } else {
            self.deriv.resolve(symbol, reply);
        }
    }

    fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
        if on_binance(&self.binance_symbols, &request.symbol) {
            self.binance.history(request, reply);
        } else if self.on_mt5(&request.symbol) {
            self.mt5.history(request, reply);
        } else {
            self.deriv.history(request, reply);
        }
    }

    fn subscribe(&self, subscription: Subscription) {
        if on_binance(&self.binance_symbols, &subscription.symbol) {
            self.binance.subscribe(subscription);
        } else if self.on_mt5(&subscription.symbol) {
            self.mt5.subscribe(subscription);
        } else {
            self.deriv.subscribe(subscription);
        }
    }
}

/// Watchlist quotes of every venue.
pub struct CombinedQuotes {
    deriv: DerivQuotes,
    binance: BinanceQuotes,
    mt5: Mt5Quotes,
    binance_symbols: Symbols,
    mt5_symbols: Symbols,
}

impl CombinedQuotes {
    pub fn new(binance_symbols: Symbols, mt5_symbols: Symbols, mt5: Option<Connection>) -> Self {
        Self {
            deriv: DerivQuotes,
            binance: BinanceQuotes,
            mt5: Mt5Quotes::new(mt5),
            binance_symbols,
            mt5_symbols,
        }
    }

    fn on_mt5(&self, symbol: &str) -> bool {
        !on_binance(&self.binance_symbols, symbol) && self.mt5_symbols.contains(symbol)
    }
}

impl Quotes for CombinedQuotes {
    fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
        let mut mt5 = Vec::new();
        for symbol in symbols {
            if on_binance(&self.binance_symbols, symbol) {
                self.binance
                    .subscribe(std::slice::from_ref(symbol), events.clone());
            } else if self.on_mt5(symbol) {
                mt5.push(symbol.clone());
            } else {
                self.deriv
                    .subscribe(std::slice::from_ref(symbol), events.clone());
            }
        }
        // One terminal connection serves every MT5 quote, so they go together.
        if !mt5.is_empty() {
            self.mt5.subscribe(&mt5, events);
        }
    }
}
