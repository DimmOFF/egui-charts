//! Every venue catalogue, fetched once at startup so the symbol search lists every available
//! instrument, and shared with the feeds so a symbol is routed to the venue that served it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use egui_charts::data::Reply;
use egui_charts::model::{Instrument, SymbolType};
use egui_charts::services::{SymbolQuery, SymbolSearch};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::deriv_feed::DERIV_PUBLIC_WS;
use super::mt5_feed::{Connection, Mt5Link};
use super::providers::Providers;

const BINANCE_EXCHANGE_INFO: &str = "https://api.binance.com/api/v3/exchangeInfo";
const BINANCE_FUTURES_INFO: &str = "https://fapi.binance.com/fapi/v1/exchangeInfo";

/// The suffix that marks a perpetual futures symbol, which may share a spot pair's name.
pub const FUTURES_SUFFIX: &str = ".P";

/// A venue catalogue by upper-case symbol, empty until its fetch completes.
#[derive(Clone, Default)]
pub struct Symbols(Arc<RwLock<HashMap<String, Instrument>>>);

impl Symbols {
    pub(crate) fn insert(&self, instruments: Vec<Instrument>) {
        let mut map = self.0.write().unwrap_or_else(|error| error.into_inner());
        for instrument in instruments {
            map.insert(instrument.symbol.to_uppercase(), instrument);
        }
    }

    /// The instrument of `symbol`, when the catalogue holds it.
    pub fn get(&self, symbol: &str) -> Option<Instrument> {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&symbol.to_uppercase())
            .cloned()
    }

    /// Whether the catalogue holds `symbol`.
    pub fn contains(&self, symbol: &str) -> bool {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&symbol.to_uppercase())
    }

    /// Whether nothing has been loaded yet, so a missing symbol says nothing about the venue.
    pub fn is_empty(&self) -> bool {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    }

    /// Every instrument the catalogue holds.
    pub fn all(&self) -> Vec<Instrument> {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

struct Ready {
    /// Set once every catalogue has finished loading.
    loaded: Mutex<bool>,
    signalled: Condvar,
}

/// The symbol search over every catalogue, fetching them on worker threads at startup.
pub struct Catalog {
    ready: Arc<Ready>,
    deriv: Symbols,
    binance: Symbols,
    mt5: Arc<Mt5Link>,
    providers: Providers,
}

impl Catalog {
    /// Starts loading every catalogue and returns the search service at once.
    pub fn load(providers: Providers) -> Self {
        let deriv = Symbols::default();
        let binance = Symbols::default();
        // Local and quick, and the chart's first symbol may be an MT5 one, so it is ready before
        // the window opens rather than when the network fetches finish.
        let mt5 = Arc::new(Mt5Link::new(Symbols::default(), Connection::open()));
        let ready = Arc::new(Ready {
            loaded: Mutex::new(false),
            signalled: Condvar::new(),
        });
        let catalog = Self {
            ready: Arc::clone(&ready),
            deriv: deriv.clone(),
            binance: binance.clone(),
            mt5: Arc::clone(&mt5),
            providers,
        };
        let spawn = std::thread::Builder::new()
            .name("symbol-catalogue".into())
            .spawn(move || {
                // Concurrent, because a slow venue must not hold up the others: a symbol from the
                // finished catalogue is routed to its venue without waiting.
                let (deriv_pairs, spot, futures) = std::thread::scope(|scope| {
                    let deriv = scope.spawn(deriv_instruments);
                    let spot = scope.spawn(binance_instruments);
                    let futures = scope.spawn(binance_futures_instruments);
                    (
                        deriv.join().unwrap_or_default(),
                        spot.join().unwrap_or_else(|_| Err("catalogue panicked".into())),
                        futures
                            .join()
                            .unwrap_or_else(|_| Err("catalogue panicked".into())),
                    )
                });
                let deriv_count = deriv_pairs.len();
                deriv.insert(deriv_pairs);
                let mut spot_count = 0;
                match spot {
                    Ok(pairs) => {
                        spot_count = pairs.len();
                        binance.insert(pairs);
                    }
                    Err(error) => eprintln!("Binance spot catalogue error: {error}"),
                }
                let mut futures_count = 0;
                match futures {
                    Ok(pairs) => {
                        futures_count = pairs.len();
                        binance.insert(pairs);
                    }
                    Err(error) => eprintln!("Binance futures catalogue error: {error}"),
                }
                let mt5_count = mt5.symbols().all().len();
                eprintln!(
                    "Symbol catalogue: {deriv_count} Deriv + {spot_count} Binance spot + \
                     {futures_count} Binance futures + {mt5_count} MT5 symbols"
                );
                *ready
                    .loaded
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = true;
                ready.signalled.notify_all();
            });
        if let Err(error) = spawn {
            eprintln!("Could not start the symbol catalogue loader: {error}");
        }
        catalog
    }

    /// The Deriv catalogue, filled when the fetch completes.
    pub fn deriv(&self) -> Symbols {
        self.deriv.clone()
    }

    /// The Binance catalogue, filled when the fetch completes.
    pub fn binance(&self) -> Symbols {
        self.binance.clone()
    }

    /// The terminal this machine runs, shared by the feeds and quotes that read from it.
    pub fn mt5(&self) -> Arc<Mt5Link> {
        Arc::clone(&self.mt5)
    }

    /// Whether every catalogue has finished loading, including the network ones.
    pub fn is_ready(&self) -> bool {
        *self
            .ready
            .loaded
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Every instrument loaded so far from the providers the settings leave on.
    fn catalogue(&self) -> Vec<Instrument> {
        catalogue(&self.deriv, &self.binance, &self.mt5, &self.providers)
    }
}

/// The instruments of every loaded catalogue whose provider is on, MT5 included.
fn catalogue(
    deriv: &Symbols,
    binance: &Symbols,
    mt5: &Mt5Link,
    providers: &Providers,
) -> Vec<Instrument> {
    let providers = providers.get();
    let mut instruments = Vec::new();
    if providers.deriv {
        instruments.extend(deriv.all());
    }
    if providers.binance {
        instruments.extend(binance.all());
    }
    if providers.mt5 {
        instruments.extend(mt5.symbols().all());
    }
    instruments
}

impl SymbolSearch for Catalog {
    fn search(&self, query: SymbolQuery, reply: Reply<Vec<Instrument>>) {
        if self.is_ready() {
            self.catalogue().search(query, reply);
            return;
        }
        // The dialog searches off the UI thread, so waiting for the catalogue costs no frame.
        let ready = Arc::clone(&self.ready);
        let deriv = self.deriv.clone();
        let binance = self.binance.clone();
        let mt5 = Arc::clone(&self.mt5);
        let providers = self.providers.clone();
        std::thread::spawn(move || {
            let loaded = ready
                .loaded
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let (loaded, _) = ready
                .signalled
                .wait_timeout_while(loaded, Duration::from_secs(30), |loaded| !*loaded)
                .unwrap_or_else(|error| error.into_inner());
            drop(loaded);
            catalogue(&deriv, &binance, &mt5, &providers).search(query, reply);
        });
    }
}

fn deriv_instruments() -> Vec<Instrument> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Deriv catalogue runtime error: {error}");
            return Vec::new();
        }
    };
    // A silent venue must not hold the catalogue, and with it the symbol search, open.
    runtime.block_on(async {
        match tokio::time::timeout(Duration::from_secs(10), deriv_symbols()).await {
            Ok(instruments) => instruments,
            Err(_) => {
                eprintln!("Deriv catalogue timed out");
                Vec::new()
            }
        }
    })
}

async fn deriv_symbols() -> Vec<Instrument> {
    let (mut ws, _) = match connect_async(DERIV_PUBLIC_WS).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("Deriv catalogue connection error: {error}");
            return Vec::new();
        }
    };
    let request = serde_json::json!({ "active_symbols": "brief" });
    if ws
        .send(Message::Text(request.to_string().into()))
        .await
        .is_err()
    {
        return Vec::new();
    }
    while let Some(frame) = ws.next().await {
        match frame {
            Ok(Message::Text(text)) => {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                if let Some(error) = value.get("error") {
                    eprintln!("Deriv catalogue error: {error}");
                    return Vec::new();
                }
                let Some(rows) = value.get("active_symbols").and_then(|v| v.as_array()) else {
                    continue;
                };
                return rows.iter().filter_map(deriv_instrument).collect();
            }
            Ok(Message::Ping(payload)) => {
                if ws.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Vec::new()
}

fn deriv_instrument(row: &serde_json::Value) -> Option<Instrument> {
    let symbol = row.get("underlying_symbol")?.as_str()?;
    let name = row
        .get("underlying_symbol_name")
        .and_then(|value| value.as_str())
        .unwrap_or(symbol);
    let kind = match row.get("market").and_then(|value| value.as_str()) {
        Some("forex") => SymbolType::Forex,
        Some("cryptocurrency") => SymbolType::Crypto,
        Some("commodities") => SymbolType::Commodity,
        _ => SymbolType::Index,
    };
    Some(
        Instrument::new(symbol)
            .with_description(name)
            .with_exchange("Deriv")
            .with_symbol_type(kind),
    )
}

/// The spot pairs `exchangeInfo` lists as trading; its full reply carries every filter, so only
/// the fields the catalogue needs are parsed.
#[derive(Deserialize)]
struct ExchangeInfo {
    symbols: Vec<AssetPair>,
}

#[derive(Deserialize)]
struct AssetPair {
    symbol: String,
    status: String,
    #[serde(rename = "baseAsset")]
    base: String,
    #[serde(rename = "quoteAsset")]
    quote: String,
    #[serde(rename = "isSpotTradingAllowed")]
    spot: bool,
}

fn binance_instruments() -> Result<Vec<Instrument>, String> {
    let response = ureq::get(BINANCE_EXCHANGE_INFO)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|error| error.to_string())?;
    let info: ExchangeInfo =
        serde_json::from_reader(response.into_reader()).map_err(|error| error.to_string())?;
    Ok(info
        .symbols
        .into_iter()
        .filter(|pair| pair.status == "TRADING" && pair.spot)
        .map(|pair| {
            Instrument::new(pair.symbol)
                .with_description(format!("{} / {}", pair.base, pair.quote))
                .with_exchange("Binance")
                .with_symbol_type(SymbolType::Crypto)
        })
        .collect())
}

/// The perpetual contracts `fapi` lists as trading, margined in USDT or USDC.
#[derive(Deserialize)]
struct FuturesInfo {
    symbols: Vec<Contract>,
}

#[derive(Deserialize)]
struct Contract {
    symbol: String,
    status: String,
    #[serde(rename = "baseAsset")]
    base: String,
    #[serde(rename = "quoteAsset")]
    quote: String,
    #[serde(rename = "contractType")]
    contract: String,
}

fn binance_futures_instruments() -> Result<Vec<Instrument>, String> {
    let response = ureq::get(BINANCE_FUTURES_INFO)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|error| error.to_string())?;
    let info: FuturesInfo =
        serde_json::from_reader(response.into_reader()).map_err(|error| error.to_string())?;
    Ok(info
        .symbols
        .into_iter()
        .filter(|pair| pair.status == "TRADING" && pair.contract == "PERPETUAL")
        .map(|pair| {
            Instrument::new(format!("{}{FUTURES_SUFFIX}", pair.symbol))
                .with_description(format!("{} / {} Perpetual", pair.base, pair.quote))
                .with_exchange("Binance Futures")
                .with_symbol_type(SymbolType::Crypto)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{Mt5Link, Providers, Symbols, catalogue};
    use egui_charts::model::Instrument;
    use egui_charts::services::DataProviders;

    /// A turned-off provider's symbols leave the search; the ones left on stay.
    #[test]
    fn a_turned_off_provider_is_left_out_of_the_search() {
        let deriv = Symbols::default();
        deriv.insert(vec![Instrument::new("R_100").with_exchange("Deriv")]);
        let binance = Symbols::default();
        binance.insert(vec![Instrument::new("BTCUSDT").with_exchange("Binance")]);
        let mt5 = Mt5Link::new(Symbols::default(), None);
        let providers = Providers::default();
        assert_eq!(catalogue(&deriv, &binance, &mt5, &providers).len(), 2);
        providers.set(&DataProviders {
            binance: false,
            ..DataProviders::default()
        });
        let found = catalogue(&deriv, &binance, &mt5, &providers);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].symbol, "R_100");
    }
}
