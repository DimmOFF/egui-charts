//! One feed and one quote stream over every venue: the Binance catalogue goes to Binance, the MT5
//! catalogue to the local terminal, every other symbol to Deriv.

use std::sync::Arc;

use egui_charts::data::{
    Datafeed, FeedCapabilities, HistoryPage, HistoryRequest, Reply, Subscription,
};
use egui_charts::model::{Instrument, Quote};
use egui_charts::services::{Events, Quotes};

use super::binance_feed::{BinanceFeed, BinanceQuotes, is_futures};
use super::catalog::Symbols;
use super::deriv_feed::{DerivFeed, TIMEFRAMES};
use super::deriv_services::DerivQuotes;
use super::mt5_feed::{Mt5Feed, Mt5Link, Mt5Quotes};

/// Whether `symbol` trades on Binance: in its catalogue, or a futures pair before the catalogue
/// finishes loading.
fn on_binance(symbols: &Symbols, symbol: &str) -> bool {
    is_futures(symbol) || symbols.contains(symbol)
}

/// Where a symbol's data comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Venue {
    Binance,
    Mt5,
    Deriv,
}

/// The venue that serves a symbol. Binance wins because its catalogue is the one a saved symbol
/// usually comes from; a symbol the loaded catalogues do not know goes to MT5, which answers that
/// the terminal is off instead of Deriv's `InvalidSymbol`.
fn venue_for(on_binance: bool, on_deriv: bool, deriv_known: bool, on_mt5: bool) -> Venue {
    if on_binance {
        Venue::Binance
    } else if on_deriv {
        // A name both venues list stays with Deriv, as it always did.
        Venue::Deriv
    } else if on_mt5 || deriv_known {
        Venue::Mt5
    } else {
        Venue::Deriv
    }
}

/// The venue of `symbol`, asking the terminal only when its catalogue cannot answer.
fn venue(symbol: &str, binance: &Symbols, deriv: &Symbols, mt5: &Mt5Link) -> Venue {
    let on_binance = on_binance(binance, symbol);
    let deriv_known = !deriv.is_empty();
    let on_deriv = deriv.contains(symbol);
    let on_mt5 = !on_binance && !on_deriv && mt5.holds(symbol);
    venue_for(on_binance, on_deriv, deriv_known, on_mt5)
}

pub struct CombinedFeed {
    deriv: DerivFeed,
    binance: BinanceFeed,
    mt5: Mt5Feed,
    /// The Binance catalogue, filled when the symbol search loads it.
    binance_symbols: Symbols,
    /// The Deriv catalogue, filled when the symbol search loads it.
    deriv_symbols: Symbols,
    mt5_link: Arc<Mt5Link>,
}

impl CombinedFeed {
    /// A feed whose Deriv requests default to `deriv_symbol`.
    pub fn new(
        deriv_symbol: impl Into<String>,
        deriv_symbols: Symbols,
        binance_symbols: Symbols,
        mt5: Arc<Mt5Link>,
    ) -> Self {
        Self {
            deriv: DerivFeed::new(deriv_symbol, deriv_symbols.clone()),
            binance: BinanceFeed::new(binance_symbols.clone()),
            mt5: Mt5Feed::new(Arc::clone(&mt5)),
            binance_symbols,
            deriv_symbols,
            mt5_link: mt5,
        }
    }

    fn venue(&self, symbol: &str) -> Venue {
        venue(
            symbol,
            &self.binance_symbols,
            &self.deriv_symbols,
            &self.mt5_link,
        )
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
        match self.venue(symbol) {
            Venue::Binance => self.binance.resolve(symbol, reply),
            Venue::Mt5 => self.mt5.resolve(symbol, reply),
            Venue::Deriv => self.deriv.resolve(symbol, reply),
        }
    }

    fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
        let venue = self.venue(&request.symbol);
        eprintln!("History for {} via {venue:?}", request.symbol);
        match venue {
            Venue::Binance => self.binance.history(request, reply),
            Venue::Mt5 => self.mt5.history(request, reply),
            Venue::Deriv => self.deriv.history(request, reply),
        }
    }

    fn subscribe(&self, subscription: Subscription) {
        match self.venue(&subscription.symbol) {
            Venue::Binance => self.binance.subscribe(subscription),
            Venue::Mt5 => self.mt5.subscribe(subscription),
            Venue::Deriv => self.deriv.subscribe(subscription),
        }
    }
}

/// Watchlist quotes of every venue.
pub struct CombinedQuotes {
    deriv: DerivQuotes,
    binance: BinanceQuotes,
    mt5: Mt5Quotes,
    binance_symbols: Symbols,
    deriv_symbols: Symbols,
    mt5_link: Arc<Mt5Link>,
}

impl CombinedQuotes {
    pub fn new(binance_symbols: Symbols, deriv_symbols: Symbols, mt5: Arc<Mt5Link>) -> Self {
        Self {
            deriv: DerivQuotes,
            binance: BinanceQuotes,
            mt5: Mt5Quotes::new(Arc::clone(&mt5)),
            binance_symbols,
            deriv_symbols,
            mt5_link: mt5,
        }
    }

    fn venue(&self, symbol: &str) -> Venue {
        venue(
            symbol,
            &self.binance_symbols,
            &self.deriv_symbols,
            &self.mt5_link,
        )
    }
}

impl Quotes for CombinedQuotes {
    fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
        let mut mt5 = Vec::new();
        for symbol in symbols {
            match self.venue(symbol) {
                Venue::Binance => self
                    .binance
                    .subscribe(std::slice::from_ref(symbol), events.clone()),
                Venue::Mt5 => mt5.push(symbol.clone()),
                Venue::Deriv => self
                    .deriv
                    .subscribe(std::slice::from_ref(symbol), events.clone()),
            }
        }
        // One terminal connection serves every MT5 quote, so they go together.
        if !mt5.is_empty() {
            self.mt5.subscribe(&mt5, events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Venue, venue_for};

    /// A saved MT5 symbol must keep going to the terminal when it is off, so the chart says why
    /// instead of asking Deriv for a symbol it does not list.
    #[test]
    fn an_unknown_symbol_goes_to_the_terminal() {
        assert_eq!(venue_for(true, false, true, false), Venue::Binance);
        assert_eq!(venue_for(false, true, true, false), Venue::Deriv);
        assert_eq!(venue_for(false, true, true, true), Venue::Deriv);
        assert_eq!(venue_for(false, false, true, false), Venue::Mt5);
        assert_eq!(venue_for(false, false, true, true), Venue::Mt5);
        // Before the Deriv catalogue loads, a missing symbol says nothing about its venue.
        assert_eq!(venue_for(false, false, false, false), Venue::Deriv);
        assert_eq!(venue_for(false, false, false, true), Venue::Mt5);
    }
}
