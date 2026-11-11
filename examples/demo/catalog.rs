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
use super::mt5_feed::Connection;

const BINANCE_EXCHANGE_INFO: &str = "https://api.binance.com/api/v3/exchangeInfo";
const BINANCE_FUTURES_INFO: &str = "https://fapi.binance.com/fapi/v1/exchangeInfo";

/// The suffix that marks a perpetual futures symbol, which may share a spot pair's name.
pub const FUTURES_SUFFIX: &str = ".P";

/// A venue catalogue by upper-case symbol, empty until its fetch completes.
#[derive(Clone, Default)]
pub struct Symbols(Arc<RwLock<HashMap<String, Instrument>>>);

impl Symbols {
    fn insert(&self, instruments: Vec<Instrument>) {
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
}

struct Ready {
    /// Every instrument of both venues, once loaded.
    list: Mutex<Option<Vec<Instrument>>>,
    loaded: Condvar,
}

/// The symbol search over every catalogue, fetching them on a worker thread at startup.
pub struct Catalog {
    ready: Arc<Ready>,
    deriv: Symbols,
    binance: Symbols,
    mt5: Symbols,
    connection: Option<Connection>,
}

impl Catalog {
    /// Starts loading every catalogue and returns the search service at once.
    pub fn load() -> Self {
        let deriv = Symbols::default();
        let binance = Symbols::default();
        let mt5 = Symbols::default();
        let connection = Connection::open();
        // Local and quick, and the chart's first symbol may be an MT5 one, so it is ready before
        // the window opens rather than when the network fetches finish.
        let mt5_pairs = connection
            .as_ref()
            .map(Connection::instruments)
            .unwrap_or_default();
        let mt5_count = mt5_pairs.len();
        mt5.insert(mt5_pairs.clone());
        let ready = Arc::new(Ready {
            list: Mutex::new(None),
            loaded: Condvar::new(),
        });
        let catalog = Self {
            ready: ready.clone(),
            deriv: deriv.clone(),
            binance: binance.clone(),
            mt5,
            connection: connection.clone(),
        };
        let spawn = std::thread::Builder::new()
            .name("symbol-catalogue".into())
            .spawn(move || {
                let mut instruments = deriv_instruments();
                let deriv_count = instruments.len();
                deriv.insert(instruments.clone());
                let mut spot_count = 0;
                match binance_instruments() {
                    Ok(pairs) => {
                        spot_count = pairs.len();
                        binance.insert(pairs.clone());
                        instruments.extend(pairs);
                    }
                    Err(error) => eprintln!("Binance spot catalogue error: {error}"),
                }
                let mut futures_count = 0;
                match binance_futures_instruments() {
                    Ok(pairs) => {
                        futures_count = pairs.len();
                        binance.insert(pairs.clone());
                        instruments.extend(pairs);
                    }
                    Err(error) => eprintln!("Binance futures catalogue error: {error}"),
                }
                instruments.extend(mt5_pairs);
                eprintln!(
                    "Symbol catalogue: {deriv_count} Deriv + {spot_count} Binance spot + \
                     {futures_count} Binance futures + {mt5_count} MT5 symbols"
                );
                *ready.list.lock().unwrap_or_else(|error| error.into_inner()) = Some(instruments);
                ready.loaded.notify_all();
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

    /// The MetaTrader 5 catalogue, empty when no terminal is running.
    pub fn mt5(&self) -> Symbols {
        self.mt5.clone()
    }

    /// The terminal this machine runs, shared by the feeds that read from it.
    pub fn connection(&self) -> Option<Connection> {
        self.connection.clone()
    }

    /// Whether every catalogue has finished loading, including the network ones.
    pub fn is_ready(&self) -> bool {
        self.ready
            .list
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }
}

impl SymbolSearch for Catalog {
    fn search(&self, query: SymbolQuery, reply: Reply<Vec<Instrument>>) {
        {
            let list = self
                .ready
                .list
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(instruments) = &*list {
                instruments.search(query, reply);
                return;
            }
        }
        // The dialog searches off the UI thread, so waiting for the catalogue costs no frame.
        let ready = self.ready.clone();
        std::thread::spawn(move || {
            let list = ready.list.lock().unwrap_or_else(|error| error.into_inner());
            let (list, _) = ready
                .loaded
                .wait_timeout_while(list, Duration::from_secs(60), |list| list.is_none())
                .unwrap_or_else(|error| error.into_inner());
            match &*list {
                Some(instruments) => instruments.search(query, reply),
                None => reply.send(Ok(Vec::new())),
            }
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
    runtime.block_on(async {
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
    })
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
        .timeout(Duration::from_secs(60))
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
        .timeout(Duration::from_secs(60))
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
