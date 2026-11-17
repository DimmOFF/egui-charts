//! Deriv's public WebSocket as an egui-charts `Datafeed`: candle history plus a live tick stream.

use chrono::{TimeZone, Utc};
use egui_charts::data::{
    Datafeed, FeedCapabilities, FeedStatus, HistoryPage, HistoryRequest, Reply, Subscription,
};
use egui_charts::model::{Bar, Instrument, Tick, Timeframe};
use futures_util::{SinkExt, StreamExt};
use std::num::NonZeroU64;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::catalog::Symbols;

pub const DERIV_PUBLIC_WS: &str = "wss://api.derivws.com/trading/v1/options/ws/public";

/// Deriv symbols offered by the symbol search and the terminal's symbol picker.
pub const SYMBOLS: &[(&str, &str)] = &[
    ("R_10", "Volatility 10"),
    ("R_25", "Volatility 25"),
    ("R_50", "Volatility 50"),
    ("R_75", "Volatility 75"),
    ("R_100", "Volatility 100"),
    ("1HZ10V", "Volatility 10 (1s)"),
    ("1HZ100V", "Volatility 100 (1s)"),
    ("frxEURUSD", "EUR/USD"),
    ("frxGBPUSD", "GBP/USD"),
    ("frxUSDJPY", "USD/JPY"),
    ("frxXAUUSD", "Gold/USD"),
    ("cryBTCUSD", "BTC/USD"),
    ("cryETHUSD", "ETH/USD"),
];

/// The intervals Deriv serves as native candles.
pub const TIMEFRAMES: &[Timeframe] = &[
    Timeframe::Min1,
    Timeframe::Custom(NonZeroU64::new(120).unwrap()),
    Timeframe::Custom(NonZeroU64::new(180).unwrap()),
    Timeframe::Min5,
    Timeframe::Custom(NonZeroU64::new(600).unwrap()),
    Timeframe::Min15,
    Timeframe::Min30,
    Timeframe::Hour1,
    Timeframe::Custom(NonZeroU64::new(7200).unwrap()),
    Timeframe::Hour4,
    Timeframe::Day1,
];

/// Deriv serves at most this many candles per history request.
const HISTORY_COUNT: u32 = 5000;

pub struct DerivFeed {
    symbol: String,
    symbols: Symbols,
}

impl DerivFeed {
    pub fn new(symbol: impl Into<String>, symbols: Symbols) -> Self {
        Self {
            symbol: symbol.into(),
            symbols,
        }
    }

    fn requested_symbol(configured: &str, requested: &str) -> String {
        if requested.trim().is_empty() {
            configured.to_owned()
        } else {
            requested.to_owned()
        }
    }

    /// Deriv candle history accepts these fixed granularities; other chart intervals use
    /// one-minute source candles until tick aggregation is added.
    fn granularity(timeframe: Timeframe) -> u32 {
        match timeframe {
            Timeframe::Min1 => 60,
            Timeframe::Min5 => 300,
            Timeframe::Min15 => 900,
            Timeframe::Min30 => 1800,
            Timeframe::Hour1 => 3600,
            Timeframe::Hour4 => 14400,
            Timeframe::Day1 => 86400,
            // The 2m, 3m, 10m and 2h presets are native too; one-minute candles would make the
            // chart keep only the last minute of each slot.
            Timeframe::Custom(seconds) => match seconds.get() {
                120 | 180 | 600 | 7200 => seconds.get() as u32,
                _ => 60,
            },
            _ => 60,
        }
    }

    fn parse_candles(value: &serde_json::Value) -> Vec<Bar> {
        value
            .get("candles")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|candle| {
                let epoch = candle.get("epoch")?.as_i64()?;
                let open = candle.get("open")?.as_f64()?;
                let high = candle.get("high")?.as_f64()?;
                let low = candle.get("low")?.as_f64()?;
                let close = candle.get("close")?.as_f64()?;
                if ![open, high, low, close].iter().all(|p| p.is_finite()) {
                    return None;
                }
                Some(Bar::new(
                    Utc.timestamp_opt(epoch, 0).single()?,
                    open,
                    high,
                    low,
                    close,
                    // Deriv's public candle response does not include volume.
                    0.0,
                ))
            })
            .collect()
    }
}

impl Datafeed for DerivFeed {
    fn capabilities(&self, reply: Reply<FeedCapabilities>) {
        reply.send(Ok(
            FeedCapabilities::new().with_timeframes(TIMEFRAMES.iter().copied())
        ));
    }

    fn resolve(&self, symbol: &str, reply: Reply<Instrument>) {
        let instrument = self.symbols.get(symbol).unwrap_or_else(|| {
            let name = SYMBOLS
                .iter()
                .find(|(value, _)| value.eq_ignore_ascii_case(symbol))
                .map_or(symbol, |(_, name)| *name);
            Instrument::new(symbol)
                .with_description(name)
                .with_exchange("Deriv")
        });
        reply.send(Ok(instrument));
    }

    fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
        let symbol = Self::requested_symbol(&self.symbol, &request.symbol);
        let granularity = Self::granularity(request.timeframe);
        let count = u32::try_from(request.count.min(HISTORY_COUNT as usize))
            .unwrap_or(HISTORY_COUNT)
            .max(1);
        // `end` is inclusive, so stop one second before `before` to skip the bar the chart holds.
        let end = request.before.map_or_else(
            || "latest".to_owned(),
            |before| (before.timestamp() - 1).to_string(),
        );

        let spawn = std::thread::Builder::new()
            .name(format!("deriv-history-{symbol}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("Deriv history runtime error: {error}");
                        let _ =
                            reply.send(Err(egui_charts::data::FeedError::Other(error.to_string())));
                        return;
                    }
                };
                let result = runtime.block_on(async move {
                    let (mut ws, _) = match connect_async(DERIV_PUBLIC_WS).await {
                        Ok(pair) => pair,
                        Err(error) => {
                            eprintln!("Deriv history connection error: {error}");
                            return Err(egui_charts::data::FeedError::Network(error.to_string()));
                        }
                    };
                    let request = serde_json::json!({
                        "ticks_history": symbol,
                        "adjust_start_time": 1,
                        "count": count,
                        "end": end,
                        "style": "candles",
                        "granularity": granularity
                    });
                    if let Err(error) = ws.send(Message::Text(request.to_string().into())).await {
                        eprintln!("Deriv history request send error: {error}");
                        return Err(egui_charts::data::FeedError::Network(error.to_string()));
                    }
                    while let Some(frame) = ws.next().await {
                        match frame {
                            Ok(Message::Text(text)) => {
                                let value: serde_json::Value = match serde_json::from_str(&text) {
                                    Ok(value) => value,
                                    Err(error) => {
                                        eprintln!("Invalid Deriv history JSON: {error}");
                                        continue;
                                    }
                                };
                                if let Some(error) = value.get("error") {
                                    eprintln!("Deriv history API error: {error}");
                                    return Err(egui_charts::data::FeedError::Other(
                                        error.to_string(),
                                    ));
                                }
                                if value.get("msg_type").and_then(|v| v.as_str()) == Some("candles")
                                    || value.get("candles").is_some()
                                {
                                    return Ok(Self::parse_candles(&value));
                                }
                            }
                            Ok(Message::Ping(payload)) => {
                                if let Err(error) = ws.send(Message::Pong(payload)).await {
                                    eprintln!("Deriv history pong error: {error}");
                                    break;
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(error) => {
                                eprintln!("Deriv history WebSocket error: {error}");
                                break;
                            }
                        }
                    }
                    Ok(Vec::new())
                });
                match result {
                    Ok(bars) => {
                        eprintln!("Deriv history returned {} candles", bars.len());
                        // A short page means there is nothing older to page back to.
                        let end_of_history = bars.len() < count as usize;
                        let _ = reply.send(Ok(HistoryPage::new(bars, end_of_history)));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            });
        if let Err(error) = spawn {
            eprintln!("Could not start Deriv history worker: {error}");
        }
    }

    fn subscribe(&self, subscription: Subscription) {
        let symbol = Self::requested_symbol(&self.symbol, &subscription.symbol);
        let sink = subscription.sink;
        let spawn = std::thread::Builder::new()
            .name(format!("deriv-stream-{symbol}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("Deriv stream runtime error: {error}");
                        sink.set_status(FeedStatus::Disconnected);
                        return;
                    }
                };
                sink.set_status(FeedStatus::Connecting);
                runtime.block_on(async move {
                    let (mut ws, _) = match connect_async(DERIV_PUBLIC_WS).await {
                        Ok(pair) => pair,
                        Err(error) => {
                            eprintln!("Deriv live connection error: {error}");
                            sink.set_status(FeedStatus::Disconnected);
                            return;
                        }
                    };
                    let request = serde_json::json!({ "ticks": symbol, "subscribe": 1 });
                    if let Err(error) = ws.send(Message::Text(request.to_string().into())).await {
                        eprintln!("Deriv live subscribe send error: {error}");
                        sink.set_status(FeedStatus::Disconnected);
                        return;
                    }
                    eprintln!("Deriv live tick stream subscribed: {symbol}");
                    sink.set_status(FeedStatus::Live);
                    while sink.is_active() {
                        match ws.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let value: serde_json::Value = match serde_json::from_str(&text) {
                                    Ok(value) => value,
                                    Err(error) => {
                                        eprintln!("Invalid Deriv tick JSON: {error}");
                                        continue;
                                    }
                                };
                                if let Some(error) = value.get("error") {
                                    eprintln!("Deriv tick API error: {error}");
                                    sink.report_error(egui_charts::data::FeedError::Other(
                                        error.to_string(),
                                    ));
                                    break;
                                }
                                if value.get("msg_type").and_then(|v| v.as_str()) != Some("tick") {
                                    continue;
                                }
                                let Some(tick) = value.get("tick") else {
                                    continue;
                                };
                                let Some(epoch) = tick.get("epoch").and_then(|v| v.as_i64()) else {
                                    continue;
                                };
                                let Some(quote) = tick.get("quote").and_then(|v| v.as_f64()) else {
                                    continue;
                                };
                                if epoch <= 0 || !quote.is_finite() || quote <= 0.0 {
                                    continue;
                                }
                                let Some(time) = Utc.timestamp_opt(epoch, 0).single() else {
                                    continue;
                                };
                                // The public ticks endpoint supplies a quote, not trade side or volume.
                                sink.push_tick(Tick::new(time, quote, 0.0, None));
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(error) = ws.send(Message::Pong(payload)).await {
                                    eprintln!("Deriv live pong error: {error}");
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                eprintln!("Deriv live WebSocket error: {error}");
                                break;
                            }
                        }
                    }
                    sink.set_status(FeedStatus::Disconnected);
                    eprintln!("Deriv live tick stream ended: {symbol}");
                });
            });
        if let Err(error) = spawn {
            eprintln!("Could not start Deriv stream worker: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DerivFeed;
    use egui_charts::model::Timeframe;
    use std::num::NonZeroU64;

    fn custom(seconds: u64) -> Timeframe {
        Timeframe::Custom(NonZeroU64::new(seconds).unwrap())
    }

    /// Native granularities keep every slot filled; 1m source candles would leave two of every
    /// three 3m slots empty.
    #[test]
    fn native_intervals_are_requested_as_they_are() {
        assert_eq!(DerivFeed::granularity(custom(180)), 180);
        assert_eq!(DerivFeed::granularity(custom(120)), 120);
        assert_eq!(DerivFeed::granularity(custom(600)), 600);
        assert_eq!(DerivFeed::granularity(custom(7200)), 7200);
        assert_eq!(DerivFeed::granularity(Timeframe::Min5), 300);
        assert_eq!(DerivFeed::granularity(Timeframe::Min1), 60);
    }
}
