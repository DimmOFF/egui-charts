//! MetaTrader 5 through the terminal's local pipe: the symbol catalogue, bar history, live ticks
//! and watchlist quotes.
//!
//! MT5 has no public network API, so nothing here is remote: the `mt5-rs` crate speaks the same
//! local named-pipe protocol the official Python package uses, which needs the terminal running
//! and logged in on this machine and nothing else.

use chrono::{TimeZone, Utc};
use egui_charts::data::{
    Datafeed, FeedCapabilities, FeedError, FeedStatus, HistoryPage, HistoryRequest, Reply,
    Subscription,
};
use egui_charts::model::{Bar, Instrument, Quote, SymbolType, Tick, Timeframe};
use egui_charts::services::{Events, Quotes};
use mt5_rs::Mt5Client;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::catalog::Symbols;

/// How often a live stream asks the terminal for the ticks it has not seen yet.
const TICK_POLL: Duration = Duration::from_millis(250);
/// How often watchlist quotes are refreshed.
const QUOTE_POLL: Duration = Duration::from_secs(1);
/// Ticks fetched per poll; the terminal keeps a bounded tick history.
const TICK_BATCH: i32 = 1000;
/// Bars the terminal serves at most per request.
const HISTORY_COUNT: i32 = 5000;
/// Every tick its database holds, both quote and trade changes.
const TICKS_ALL: i32 = 3;
/// The reference price of a watchlist quote changes once a session.
const CLOSE_TTL: Duration = Duration::from_secs(60);
/// A terminal whose newest quote is older than this has its clock offset left alone.
const LIVE_WINDOW: i64 = 300;

/// MT5's own timeframe codes, which its protocol uses instead of names.
const TIMEFRAME_D1: i32 = 16408;

/// MT5's own timeframe codes, which its protocol uses instead of names: minute codes are plain
/// minutes, hourly ones are `16384 + hours`. The 2m, 3m, 10m and 2h presets are native too; a
/// one-minute code would make the chart keep only the last minute of each slot.
fn timeframe_code(timeframe: Timeframe) -> i32 {
    match timeframe {
        Timeframe::Min1 => 1,
        Timeframe::Min5 => 5,
        Timeframe::Min15 => 15,
        Timeframe::Min30 => 30,
        Timeframe::Hour1 => 16385,
        Timeframe::Hour4 => 16388,
        Timeframe::Day1 => TIMEFRAME_D1,
        Timeframe::Custom(seconds) => match seconds.get() {
            120 => 2,
            180 => 3,
            600 => 10,
            7200 => 16386,
            _ => 1,
        },
        _ => 1,
    }
}

fn timeframe_seconds(timeframe: Timeframe) -> i64 {
    timeframe.duration_ms() / 1000
}

/// The class a symbol belongs to, as the terminal files it in its own tree.
fn symbol_type(path: &str) -> SymbolType {
    let root = path.split('\\').next().unwrap_or_default();
    match root.to_ascii_lowercase().as_str() {
        "fx" | "forex" => SymbolType::Forex,
        "crypto" => SymbolType::Crypto,
        "metals" | "energy" | "commodities" => SymbolType::Commodity,
        "indices" | "indexes" => SymbolType::Index,
        "stocks" | "shares" => SymbolType::Stock,
        _ => SymbolType::Cfd,
    }
}

/// Where the terminal listens and how far its clock runs from UTC.
#[derive(Clone)]
pub struct Connection {
    pipe: Arc<str>,
    /// Broker server time minus UTC, in seconds: MT5 stamps bars and ticks in server time.
    offset: i64,
}

impl Connection {
    /// The terminal running on this machine, or `None` when none answers.
    pub fn open() -> Option<Self> {
        // A diagnostic switch: `TV_NO_MT5=1` runs the app as if no terminal were open.
        if std::env::var_os("TV_NO_MT5").is_some() {
            return None;
        }
        let pipe = discover()?;
        let offset = clock_offset(&pipe);
        if offset == 0 {
            eprintln!("MT5 clock offset unknown; bar and tick times may be shifted");
        }
        Some(Self {
            pipe: pipe.into(),
            offset,
        })
    }

    /// Every symbol the terminal offers, with the class and description it gives it.
    pub fn instruments(&self) -> Vec<Instrument> {
        let Ok(client) = self.client() else {
            return Vec::new();
        };
        match client.symbols_get() {
            Ok(symbols) => symbols
                .into_iter()
                .map(|symbol| {
                    Instrument::new(&symbol.name)
                        .with_description(&symbol.description)
                        .with_exchange("MT5")
                        .with_symbol_type(symbol_type(&symbol.path))
                })
                .collect(),
            Err(error) => {
                eprintln!("MT5 catalogue error: {error}");
                Vec::new()
            }
        }
    }

    /// A client of its own: the terminal accepts one pipe connection per thread.
    fn client(&self) -> Result<Mt5Client, FeedError> {
        connect(&self.pipe)
    }

    /// A server-time timestamp in milliseconds, as UTC milliseconds.
    fn utc_ms(&self, server_ms: i64) -> i64 {
        server_ms - self.offset * 1000
    }
}

fn connect(pipe: &str) -> Result<Mt5Client, FeedError> {
    let mut client = Mt5Client::new();
    client
        .initialize(Some(pipe))
        .map_err(|error| FeedError::Network(format!("MT5 terminal: {error}")))?;
    Ok(client)
}

/// The pipe of the terminal running on this machine, or `None` when none answers.
fn discover() -> Option<String> {
    // The crate panics instead of answering nothing; keep that quiet, it is the normal no-terminal
    // case.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let found = std::panic::catch_unwind(mt5_rs::discover_mt5_pipe).ok();
    std::panic::set_hook(previous);
    found
}

/// The terminal's clock offset from UTC, measured from its newest live quote, or 0 while the
/// market is closed and nothing live is there to compare.
fn clock_offset(pipe: &str) -> i64 {
    let Ok(client) = connect(pipe) else {
        return 0;
    };
    let Ok(symbols) = client.symbols_get() else {
        return 0;
    };
    let now = Utc::now().timestamp();
    let newest = symbols.iter().map(|symbol| symbol.time).max().unwrap_or(0);
    if newest == 0 || now - newest > LIVE_WINDOW {
        return 0;
    }
    // Broker clocks sit on whole, and sometimes half, hours from UTC.
    ((newest - now) as f64 / 900.0).round() as i64 * 900
}

/// The terminal this session talks to. A terminal started after the app is used from the next
/// request on, so starting MetaTrader 5 does not need a restart.
pub struct Mt5Link {
    connection: Mutex<Option<Connection>>,
    /// The symbols the terminal lists, filled whenever a connection is made.
    symbols: Symbols,
    /// When the terminal was last looked for, so a missing one is looked for now and then
    /// rather than on every request.
    attempt: Mutex<Option<Instant>>,
}

/// How often a missing terminal is looked for again.
const RECONNECT: Duration = Duration::from_secs(5);

impl Mt5Link {
    /// A link over `symbols` that uses `connection` when one was found at startup.
    pub fn new(symbols: Symbols, connection: Option<Connection>) -> Self {
        // The chart's first symbol may be an MT5 one, so its catalogue is ready at once.
        if let Some(connection) = &connection {
            symbols.insert(connection.instruments());
        }
        Self {
            connection: Mutex::new(connection),
            symbols,
            attempt: Mutex::new(None),
        }
    }

    /// The terminal, looking for one that started since the last look.
    pub fn connection(&self) -> Option<Connection> {
        let mut held = self
            .connection
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if held.is_none() {
            let mut attempt = self
                .attempt
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if attempt.is_some_and(|at: Instant| at.elapsed() < RECONNECT) {
                return None;
            }
            *attempt = Some(Instant::now());
            let connection = Connection::open()?;
            self.symbols.insert(connection.instruments());
            *held = Some(connection);
        }
        held.clone()
    }

    /// The terminal's symbols, after a look for a terminal that started meanwhile.
    pub fn holds(&self, symbol: &str) -> bool {
        self.connection();
        self.symbols.contains(symbol)
    }

    /// The symbols this link has seen.
    pub fn symbols(&self) -> Symbols {
        self.symbols.clone()
    }
}

/// History and live ticks of the terminal's symbols.
pub struct Mt5Feed {
    mt5: Arc<Mt5Link>,
}

impl Mt5Feed {
    /// A feed of the terminal behind `mt5`.
    pub fn new(mt5: Arc<Mt5Link>) -> Self {
        Self { mt5 }
    }

    /// The terminal, or the reason there is none.
    fn connection(&self) -> Result<Connection, FeedError> {
        self.mt5.connection().ok_or_else(|| {
            FeedError::Network("MetaTrader 5 is not running on this machine".into())
        })
    }

    fn bars(rows: &[mt5_rs::Rate], connection: &Connection) -> Vec<Bar> {
        rows.iter()
            .filter_map(|rate| {
                let time = Utc
                    .timestamp_millis_opt(connection.utc_ms(rate.time * 1000))
                    .single()?;
                Some(Bar::new(
                    time,
                    rate.open,
                    rate.high,
                    rate.low,
                    rate.close,
                    rate.tick_volume as f64,
                ))
            })
            .collect()
    }
}

impl Datafeed for Mt5Feed {
    fn capabilities(&self, reply: Reply<FeedCapabilities>) {
        reply.send(Ok(
            FeedCapabilities::new().with_timeframes(super::deriv_feed::TIMEFRAMES.iter().copied())
        ));
    }

    fn resolve(&self, symbol: &str, reply: Reply<Instrument>) {
        let instrument = self.mt5.symbols().get(symbol).unwrap_or_else(|| {
            Instrument::new(symbol)
                .with_exchange("MT5")
                .with_symbol_type(SymbolType::Forex)
        });
        reply.send(Ok(instrument));
    }

    fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
        let connection = match self.connection() {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("MT5 history error: {error}");
                let _ = reply.send(Err(error));
                return;
            }
        };
        let symbol = request.symbol;
        let timeframe = request.timeframe;
        let code = timeframe_code(timeframe);
        let count = request.count.clamp(1, HISTORY_COUNT as usize) as i32;
        let before = request.before.map(|before| before.timestamp_millis());
        let spawn = std::thread::Builder::new()
            .name(format!("mt5-history-{symbol}"))
            .spawn(move || {
                let bars = connection.client().and_then(|client| {
                    let rows = match before {
                        // `before` is exclusive and in UTC; the terminal's bars are in server time.
                        Some(before) => {
                            let span =
                                timeframe_seconds(timeframe) * count as i64 * 1500 + 86_400_000;
                            let start = before + connection.offset * 1000 - span;
                            let rows = client
                                .copy_rates_from(&symbol, code, start, count + 50)
                                .map_err(|error| FeedError::Network(error.to_string()))?;
                            rows.into_iter()
                                .filter(|rate| connection.utc_ms(rate.time * 1000) < before)
                                .collect::<Vec<_>>()
                        }
                        None => client
                            .copy_rates_from_pos(&symbol, code, 0, count)
                            .map_err(|error| FeedError::Network(error.to_string()))?,
                    };
                    Ok(Self::bars(&rows, &connection))
                });
                match bars {
                    Ok(bars) => {
                        eprintln!("MT5 history returned {} candles", bars.len());
                        let end_of_history = bars.len() < count as usize;
                        let _ = reply.send(Ok(HistoryPage::new(bars, end_of_history)));
                    }
                    Err(error) => {
                        eprintln!("MT5 history error: {error}");
                        let _ = reply.send(Err(error));
                    }
                }
            });
        if let Err(error) = spawn {
            eprintln!("Could not start MT5 history worker: {error}");
        }
    }

    fn subscribe(&self, subscription: Subscription) {
        let mt5 = Arc::clone(&self.mt5);
        let symbol = subscription.symbol;
        let sink = subscription.sink;
        let spawn = std::thread::Builder::new()
            .name(format!("mt5-stream-{symbol}"))
            .spawn(move || {
                // The chart builds the current bar from ticks, so a minute of them is plenty.
                let mut since = Utc::now().timestamp_millis() - 60_000;
                while sink.is_active() {
                    // A terminal started after the chart subscribed is picked up here.
                    let Some(connection) = mt5.connection() else {
                        sink.set_status(FeedStatus::Disconnected);
                        std::thread::sleep(RECONNECT);
                        continue;
                    };
                    sink.set_status(FeedStatus::Connecting);
                    let Ok(client) = connection.client() else {
                        sink.set_status(FeedStatus::Disconnected);
                        std::thread::sleep(RECONNECT);
                        continue;
                    };
                    eprintln!("MT5 live tick stream subscribed: {symbol}");
                    sink.set_status(FeedStatus::Live);
                    let mut failures = 0;
                    while sink.is_active() && failures < 3 {
                        let server_since = since + connection.offset * 1000;
                        match client.copy_ticks_from(&symbol, server_since, TICK_BATCH, TICKS_ALL) {
                            Ok(ticks) => {
                                failures = 0;
                                for tick in ticks {
                                    let price = if tick.last > 0.0 {
                                        tick.last
                                    } else {
                                        (tick.bid + tick.ask) / 2.0
                                    };
                                    let time = connection.utc_ms(if tick.time_msc > 0 {
                                        tick.time_msc
                                    } else {
                                        tick.time * 1000
                                    });
                                    let Some(time) = Utc.timestamp_millis_opt(time).single() else {
                                        continue;
                                    };
                                    if !price.is_finite()
                                        || price <= 0.0
                                        || time.timestamp_millis() <= since
                                    {
                                        continue;
                                    }
                                    since = time.timestamp_millis();
                                    let size = if tick.volume_real > 0.0 {
                                        tick.volume_real
                                    } else {
                                        tick.volume as f64
                                    };
                                    sink.push_tick(Tick::new(time, price, size, None));
                                }
                            }
                            Err(error) => {
                                failures += 1;
                                eprintln!("MT5 live tick error: {error}");
                            }
                        }
                        std::thread::sleep(TICK_POLL);
                    }
                    sink.set_status(FeedStatus::Disconnected);
                    eprintln!("MT5 live tick stream ended: {symbol}");
                }
            });
        if let Err(error) = spawn {
            eprintln!("Could not start MT5 stream worker: {error}");
        }
    }
}

/// Watchlist quotes of the terminal's symbols. The change is measured from the previous session's
/// close, which is what the terminal's own watchlist shows.
pub struct Mt5Quotes {
    mt5: Arc<Mt5Link>,
}

impl Mt5Quotes {
    pub fn new(mt5: Arc<Mt5Link>) -> Self {
        Self { mt5 }
    }
}

impl Quotes for Mt5Quotes {
    fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
        let mt5 = Arc::clone(&self.mt5);
        let symbols = symbols.to_vec();
        std::thread::spawn(move || {
            let mut closes: HashMap<String, (f64, Instant)> = HashMap::new();
            while events.is_active() {
                // A terminal started after this subscription is picked up on the next look.
                let Some(connection) = mt5.connection() else {
                    std::thread::sleep(RECONNECT);
                    continue;
                };
                let Ok(client) = connection.client() else {
                    std::thread::sleep(RECONNECT);
                    continue;
                };
                eprintln!("MT5 quotes subscribed: {} symbols", symbols.len());
                let mut failures = 0;
                while events.is_active() && failures < 3 {
                    for symbol in &symbols {
                        let tick = match client.symbol_info_tick(symbol) {
                            Ok(Some(tick)) => tick,
                            Ok(None) => continue,
                            Err(error) => {
                                failures += 1;
                                eprintln!("MT5 quote error: {error}");
                                break;
                            }
                        };
                        let price = if tick.last > 0.0 {
                            tick.last
                        } else {
                            (tick.bid + tick.ask) / 2.0
                        };
                        if !price.is_finite() || price <= 0.0 {
                            continue;
                        }
                        let reference = match closes.get(symbol) {
                            Some((close, at)) if at.elapsed() < CLOSE_TTL => *close,
                            _ => {
                                let close = client
                                    .copy_rates_from_pos(symbol, TIMEFRAME_D1, 0, 2)
                                    .ok()
                                    .and_then(|rows| match rows.len() {
                                        0 => None,
                                        1 => Some(rows[0].open),
                                        _ => Some(rows[rows.len() - 2].close),
                                    })
                                    .unwrap_or(price);
                                closes.insert(symbol.clone(), (close, Instant::now()));
                                close
                            }
                        };
                        let time = connection.utc_ms(if tick.time_msc > 0 {
                            tick.time_msc
                        } else {
                            tick.time * 1000
                        });
                        let time = Utc.timestamp_millis_opt(time).single().unwrap_or_default();
                        events.push((symbol.clone(), Quote::new(price, reference, time)));
                    }
                    std::thread::sleep(QUOTE_POLL);
                }
            }
            eprintln!("MT5 quotes stream ended");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{timeframe_code, timeframe_seconds};
    use egui_charts::model::Timeframe;
    use std::num::NonZeroU64;

    fn custom(seconds: u64) -> Timeframe {
        Timeframe::Custom(NonZeroU64::new(seconds).unwrap())
    }

    /// The 3m preset has to be requested as 3m candles: one-minute candles leave the chart's
    /// three-minute slots empty for two of every three minutes, which draws ragged bars.
    #[test]
    fn native_intervals_are_requested_as_they_are() {
        assert_eq!(timeframe_code(custom(180)), 3);
        assert_eq!(timeframe_code(custom(120)), 2);
        assert_eq!(timeframe_code(custom(600)), 10);
        assert_eq!(timeframe_code(custom(7200)), 16386);
        assert_eq!(timeframe_code(Timeframe::Min1), 1);
        assert_eq!(timeframe_code(Timeframe::Min5), 5);
        assert_eq!(timeframe_code(Timeframe::Hour1), 16385);
        assert_eq!(timeframe_code(Timeframe::Day1), 16408);
        assert_eq!(timeframe_seconds(custom(180)), 180);
        assert_eq!(timeframe_seconds(Timeframe::Hour4), 14400);
    }
}
