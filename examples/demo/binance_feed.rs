//! Binance's public market data as an egui-charts feed: REST klines, a live trade stream and
//! watchlist quotes.

use chrono::{TimeZone, Utc};
use egui_charts::data::{
    Datafeed, FeedCapabilities, FeedError, FeedStatus, HistoryPage, HistoryRequest, Reply,
    Subscription,
};
use egui_charts::model::{Bar, Instrument, OrderSide, Quote, SymbolType, Tick, Timeframe};
use egui_charts::services::{Events, Quotes};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::catalog::{FUTURES_SUFFIX, Symbols};

const SPOT_KLINES: &str = "https://api.binance.com/api/v3/klines";
const FUTURES_KLINES: &str = "https://fapi.binance.com/fapi/v1/klines";
const SPOT_WS: &str = "wss://stream.binance.com:9443/ws";
/// Futures market data is served on the combined-stream route; the plain one stays silent.
const FUTURES_WS: &str = "wss://fstream.binance.com/market/stream?streams=";

/// Binance serves at most this many klines per history request.
const HISTORY_COUNT: usize = 1000;

/// Whether `symbol` is a perpetual futures pair rather than a spot one.
pub fn is_futures(symbol: &str) -> bool {
    symbol.to_uppercase().ends_with(FUTURES_SUFFIX)
}

/// The exchange's own spelling of `symbol`, which carries no futures suffix.
fn api_symbol(symbol: &str) -> &str {
    symbol.strip_suffix(FUTURES_SUFFIX).unwrap_or(symbol)
}

fn stream_url(symbol: &str, stream: &str) -> String {
    let pair = api_symbol(symbol).to_lowercase();
    if is_futures(symbol) {
        format!("{FUTURES_WS}{pair}@{stream}")
    } else {
        format!("{SPOT_WS}/{pair}@{stream}")
    }
}

/// Futures streams wrap each payload in `{"stream": .., "data": ..}`.
fn payload(value: &serde_json::Value) -> &serde_json::Value {
    value.get("data").unwrap_or(value)
}

/// Candle history and live trades of Binance's public endpoints.
pub struct BinanceFeed {
    symbols: Symbols,
}

impl BinanceFeed {
    /// A feed whose symbols resolve through `symbols`, the catalogue the search loaded.
    pub fn new(symbols: Symbols) -> Self {
        Self { symbols }
    }

    /// The interval Binance serves: its own, so bars come already bucketed instead of one-minute
    /// bars that the chart's clock would drop to the last minute of every slot.
    fn interval(timeframe: Timeframe) -> &'static str {
        match timeframe {
            Timeframe::Min5 => "5m",
            Timeframe::Min15 => "15m",
            Timeframe::Min30 => "30m",
            Timeframe::Hour1 => "1h",
            Timeframe::Hour4 => "4h",
            Timeframe::Day1 => "1d",
            Timeframe::Custom(seconds) => match seconds.get() {
                120 => "2m",
                180 => "3m",
                600 => "10m",
                7200 => "2h",
                _ => "1m",
            },
            _ => "1m",
        }
    }

    /// A number Binance sends as a string in klines and as a JSON number elsewhere.
    fn number(value: &serde_json::Value) -> Option<f64> {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    }

    fn parse_klines(body: &str) -> Option<Vec<Bar>> {
        let rows: Vec<Vec<serde_json::Value>> = serde_json::from_str(body).ok()?;
        Some(
            rows.iter()
                .filter_map(|row| {
                    let open = Self::number(row.get(1)?)?;
                    let high = Self::number(row.get(2)?)?;
                    let low = Self::number(row.get(3)?)?;
                    let close = Self::number(row.get(4)?)?;
                    if ![open, high, low, close]
                        .iter()
                        .all(|price| price.is_finite())
                    {
                        return None;
                    }
                    Some(Bar::new(
                        Utc.timestamp_millis_opt(row.first()?.as_i64()?).single()?,
                        open,
                        high,
                        low,
                        close,
                        row.get(5).and_then(Self::number).unwrap_or(0.0),
                    ))
                })
                .collect(),
        )
    }
}

impl Datafeed for BinanceFeed {
    fn capabilities(&self, reply: Reply<FeedCapabilities>) {
        reply.send(Ok(
            FeedCapabilities::new().with_timeframes(super::deriv_feed::TIMEFRAMES.iter().copied())
        ));
    }

    fn resolve(&self, symbol: &str, reply: Reply<Instrument>) {
        let instrument = self.symbols.get(symbol).unwrap_or_else(|| {
            Instrument::new(symbol)
                .with_exchange(if is_futures(symbol) {
                    "Binance Futures"
                } else {
                    "Binance"
                })
                .with_symbol_type(SymbolType::Crypto)
        });
        reply.send(Ok(instrument));
    }

    fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
        let symbol = request.symbol.to_uppercase();
        let interval = Self::interval(request.timeframe);
        let count = request.count.clamp(1, HISTORY_COUNT);
        let base = if is_futures(&symbol) {
            FUTURES_KLINES
        } else {
            SPOT_KLINES
        };
        let mut url = format!(
            "{base}?symbol={}&interval={interval}&limit={count}",
            api_symbol(&symbol)
        );
        if let Some(before) = request.before {
            // `endTime` is inclusive, so stop a millisecond before the bar the chart holds.
            url.push_str(&format!("&endTime={}", before.timestamp_millis() - 1));
        }

        let spawn = std::thread::Builder::new()
            .name(format!("binance-history-{symbol}"))
            .spawn(move || {
                let bars = ureq::get(&url)
                    .timeout(Duration::from_secs(20))
                    .call()
                    .map_err(|error| FeedError::Network(error.to_string()))
                    .and_then(|response| {
                        response
                            .into_string()
                            .map_err(|error| FeedError::Network(error.to_string()))
                    })
                    .and_then(|body| {
                        Self::parse_klines(&body)
                            .ok_or_else(|| FeedError::InvalidData("Unexpected klines".into()))
                    });
                match bars {
                    Ok(bars) => {
                        eprintln!("Binance history returned {} candles", bars.len());
                        let end_of_history = bars.len() < count;
                        let _ = reply.send(Ok(HistoryPage::new(bars, end_of_history)));
                    }
                    Err(error) => {
                        eprintln!("Binance history error: {error}");
                        let _ = reply.send(Err(error));
                    }
                }
            });
        if let Err(error) = spawn {
            eprintln!("Could not start Binance history worker: {error}");
        }
    }

    fn subscribe(&self, subscription: Subscription) {
        let symbol = subscription.symbol.to_uppercase();
        let stream = stream_url(&symbol, "aggTrade");
        let sink = subscription.sink;
        let spawn = std::thread::Builder::new()
            .name(format!("binance-stream-{symbol}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("Binance stream runtime error: {error}");
                        sink.set_status(FeedStatus::Disconnected);
                        return;
                    }
                };
                sink.set_status(FeedStatus::Connecting);
                runtime.block_on(async move {
                    let (mut ws, _) = match connect_async(&stream).await {
                        Ok(pair) => pair,
                        Err(error) => {
                            eprintln!("Binance live connection error: {error}");
                            sink.set_status(FeedStatus::Disconnected);
                            return;
                        }
                    };
                    eprintln!("Binance live trade stream subscribed: {symbol}");
                    sink.set_status(FeedStatus::Live);
                    while sink.is_active() {
                        match ws.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                                else {
                                    continue;
                                };
                                let value = payload(&value);
                                if value.get("e").and_then(|v| v.as_str()) != Some("aggTrade") {
                                    continue;
                                }
                                let price = value.get("p").and_then(Self::number);
                                let size = value.get("q").and_then(Self::number);
                                let time = value
                                    .get("T")
                                    .and_then(|v| v.as_i64())
                                    .and_then(|ms| Utc.timestamp_millis_opt(ms).single());
                                let (Some(price), Some(size), Some(time)) = (price, size, time)
                                else {
                                    continue;
                                };
                                if !price.is_finite() || price <= 0.0 {
                                    continue;
                                }
                                // `m` names the buyer as the maker, so the aggressor sold.
                                let side = if value.get("m").and_then(|v| v.as_bool()) == Some(true)
                                {
                                    OrderSide::Sell
                                } else {
                                    OrderSide::Buy
                                };
                                let mut tick = Tick::new(time, price, size, Some(side));
                                if let Some(id) = value.get("a").and_then(|v| v.as_u64()) {
                                    tick = tick.with_id(id);
                                }
                                sink.push_tick(tick);
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if ws.send(Message::Pong(payload)).await.is_err() {
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                eprintln!("Binance live WebSocket error: {error}");
                                break;
                            }
                        }
                    }
                    sink.set_status(FeedStatus::Disconnected);
                    eprintln!("Binance live trade stream ended: {symbol}");
                });
            });
        if let Err(error) = spawn {
            eprintln!("Could not start Binance stream worker: {error}");
        }
    }
}

/// Watchlist quotes from Binance's 24-hour mini ticker. The change is measured from the open of
/// the 24-hour window, which is the window's own reference price.
#[derive(Default)]
pub struct BinanceQuotes;

impl Quotes for BinanceQuotes {
    fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
        for symbol in symbols {
            let symbol = symbol.to_uppercase();
            let events = events.clone();
            std::thread::spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("Binance quotes runtime error: {error}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let stream = stream_url(&symbol, "miniTicker");
                    let (mut ws, _) = match connect_async(&stream).await {
                        Ok(pair) => pair,
                        Err(error) => {
                            eprintln!("Binance quotes connection error: {error}");
                            return;
                        }
                    };
                    eprintln!("Binance quotes subscribed: {symbol}");
                    let mut fallback = None;
                    while events.is_active() {
                        match ws.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                                else {
                                    continue;
                                };
                                let value = payload(&value);
                                let price = value.get("c").and_then(BinanceFeed::number);
                                let open = value.get("o").and_then(BinanceFeed::number);
                                let time = value
                                    .get("E")
                                    .and_then(|v| v.as_i64())
                                    .and_then(|ms| Utc.timestamp_millis_opt(ms).single());
                                let (Some(price), Some(time)) = (price, time) else {
                                    continue;
                                };
                                if !price.is_finite() || price <= 0.0 {
                                    continue;
                                }
                                let reference = open
                                    .filter(|open| open.is_finite() && *open > 0.0)
                                    .unwrap_or_else(|| *fallback.get_or_insert(price));
                                events.push((symbol.clone(), Quote::new(price, reference, time)));
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if ws.send(Message::Pong(payload)).await.is_err() {
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                eprintln!("Binance quotes WebSocket error: {error}");
                                break;
                            }
                        }
                    }
                    eprintln!("Binance quotes stream ended: {symbol}");
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BinanceFeed;
    use egui_charts::model::Timeframe;
    use std::num::NonZeroU64;

    fn custom(seconds: u64) -> Timeframe {
        Timeframe::Custom(NonZeroU64::new(seconds).unwrap())
    }

    /// The 3m preset has to be requested as 3m candles: one-minute candles leave the chart's
    /// three-minute slots empty for two of every three minutes, which draws ragged bars.
    #[test]
    fn native_intervals_are_requested_as_they_are() {
        assert_eq!(BinanceFeed::interval(custom(180)), "3m");
        assert_eq!(BinanceFeed::interval(custom(120)), "2m");
        assert_eq!(BinanceFeed::interval(custom(600)), "10m");
        assert_eq!(BinanceFeed::interval(custom(7200)), "2h");
        assert_eq!(BinanceFeed::interval(Timeframe::Min5), "5m");
        assert_eq!(BinanceFeed::interval(Timeframe::Min1), "1m");
    }
}
