use chrono::{TimeZone, Utc};
use egui_charts::data::{Datafeed, HistoryPage, HistoryRequest, Reply, Subscription};
use egui_charts::model::{Bar, Tick, Timeframe};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const DERIV_PUBLIC_WS: &str = "wss://api.derivws.com/trading/v1/options/ws/public";
const HISTORY_COUNT: u32 = 5000;

pub struct DerivFeed {
    symbol: String,
}

impl DerivFeed {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self { symbol: symbol.into() }
    }

    fn requested_symbol(configured: &str, requested: &str) -> String {
        if requested.trim().is_empty() { configured.to_owned() } else { requested.to_owned() }
    }

    // Deriv candle history accepts fixed granularities. Non-standard chart
    // intervals currently use one-minute source candles until tick aggregation
    // is added.
    fn granularity(timeframe: Timeframe) -> u32 {
        match timeframe {
            Timeframe::Min1 => 60,
            Timeframe::Min5 => 300,
            Timeframe::Min15 => 900,
            Timeframe::Hour1 => 3600,
            Timeframe::Hour4 => 14400,
            Timeframe::Day1 => 86400,
            _ => 60,
        }
    }

    fn parse_candles(value: &serde_json::Value) -> Vec<Bar> {
        value.get("candles").and_then(serde_json::Value::as_array)
            .into_iter().flatten().filter_map(|candle| {
                let epoch = candle.get("epoch")?.as_i64()?;
                let open = candle.get("open")?.as_f64()?;
                let high = candle.get("high")?.as_f64()?;
                let low = candle.get("low")?.as_f64()?;
                let close = candle.get("close")?.as_f64()?;
                if ![open, high, low, close].iter().all(|p| p.is_finite()) { return None; }
                Some(Bar {
                    time: Utc.timestamp_opt(epoch, 0).single()?,
                    open, high, low, close,
                    // Deriv's public candle response does not include volume.
                    volume: 0.0, colors: None, payload: None,
                })
            }).collect()
    }
}

impl Datafeed for DerivFeed {
    fn history(&self, request: HistoryRequest, reply: Reply<HistoryPage>) {
        let symbol = Self::requested_symbol(&self.symbol, &request.symbol);
        let granularity = Self::granularity(request.timeframe);

        let spawn = std::thread::Builder::new().name(format!("deriv-history-{symbol}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => { eprintln!("Deriv history runtime error: {error}"); return; }
                };
                let bars = runtime.block_on(async move {
                    let (mut ws, _) = match connect_async(DERIV_PUBLIC_WS).await {
                        Ok(pair) => pair,
                        Err(error) => { eprintln!("Deriv history connection error: {error}"); return Vec::new(); }
                    };
                    let request = serde_json::json!({
                        "ticks_history": symbol,
                        "adjust_start_time": 1,
                        "count": HISTORY_COUNT,
                        "end": "latest",
                        "style": "candles",
                        "granularity": granularity
                    });
                    if let Err(error) = ws.send(Message::Text(request.to_string().into())).await {
                        eprintln!("Deriv history request send error: {error}");
                        return Vec::new();
                    }
                    while let Some(frame) = ws.next().await {
                        match frame {
                            Ok(Message::Text(text)) => {
                                let value: serde_json::Value = match serde_json::from_str(&text) {
                                    Ok(value) => value,
                                    Err(error) => { eprintln!("Invalid Deriv history JSON: {error}"); continue; }
                                };
                                if let Some(error) = value.get("error") {
                                    eprintln!("Deriv history API error: {error}");
                                    return Vec::new();
                                }
                                if value.get("msg_type").and_then(|v| v.as_str()) == Some("candles")
                                    || value.get("candles").is_some() {
                                    return Self::parse_candles(&value);
                                }
                            }
                            Ok(Message::Ping(payload)) => {
                                if let Err(error) = ws.send(Message::Pong(payload)).await {
                                    eprintln!("Deriv history pong error: {error}"); break;
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(error) => { eprintln!("Deriv history WebSocket error: {error}"); break; }
                        }
                    }
                    Vec::new()
                });
                eprintln!("Deriv history returned {} candles", bars.len());
                let _ = reply.send(Ok(HistoryPage::new(bars, true)));
            });
        if let Err(error) = spawn { eprintln!("Could not start Deriv history worker: {error}"); }
    }

    fn subscribe(&self, subscription: Subscription) {
        let symbol = Self::requested_symbol(&self.symbol, &subscription.symbol);
        let sink = subscription.sink;
        let spawn = std::thread::Builder::new().name(format!("deriv-stream-{symbol}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => { eprintln!("Deriv stream runtime error: {error}"); return; }
                };
                runtime.block_on(async move {
                    let (mut ws, _) = match connect_async(DERIV_PUBLIC_WS).await {
                        Ok(pair) => pair,
                        Err(error) => { eprintln!("Deriv live connection error: {error}"); return; }
                    };
                    let request = serde_json::json!({ "ticks": symbol, "subscribe": 1 });
                    if let Err(error) = ws.send(Message::Text(request.to_string().into())).await {
                        eprintln!("Deriv live subscribe send error: {error}");
                        return;
                    }
                    eprintln!("Deriv live tick stream subscribed: {symbol}");
                    while sink.is_active() {
                        match ws.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let value: serde_json::Value = match serde_json::from_str(&text) {
                                    Ok(value) => value,
                                    Err(error) => { eprintln!("Invalid Deriv tick JSON: {error}"); continue; }
                                };
                                if let Some(error) = value.get("error") {
                                    eprintln!("Deriv tick API error: {error}"); break;
                                }
                                if value.get("msg_type").and_then(|v| v.as_str()) != Some("tick") { continue; }
                                let Some(tick) = value.get("tick") else { continue; };
                                let Some(epoch) = tick.get("epoch").and_then(|v| v.as_i64()) else { continue; };
                                let Some(quote) = tick.get("quote").and_then(|v| v.as_f64()) else { continue; };
                                if epoch <= 0 || !quote.is_finite() || quote <= 0.0 { continue; }
                                let Some(time) = Utc.timestamp_opt(epoch, 0).single() else { continue; };
                                // The public ticks endpoint supplies a quote, not trade side or volume.
                                sink.push_tick(Tick::new(time, quote, 0.0, None));
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if let Err(error) = ws.send(Message::Pong(payload)).await {
                                    eprintln!("Deriv live pong error: {error}"); break;
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => {}
                            Some(Err(error)) => { eprintln!("Deriv live WebSocket error: {error}"); break; }
                        }
                    }
                    eprintln!("Deriv live tick stream ended: {symbol}");
                });
            });
        if let Err(error) = spawn { eprintln!("Could not start Deriv stream worker: {error}"); }
    }
}
