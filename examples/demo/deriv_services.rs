//! Host services for the terminal: Deriv watchlist quotes and in-memory storage.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use egui_charts::data::{FeedError, Reply};
use egui_charts::model::Quote;
use egui_charts::services::{Events, Quotes, Storage, StoreKind};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::deriv_feed::DERIV_PUBLIC_WS;

/// Watchlist quotes from Deriv's public tick stream. Each symbol also fetches the previous
/// daily close, so the watchlist shows a real change instead of zero.
#[derive(Default)]
pub struct DerivQuotes;

impl Quotes for DerivQuotes {
    fn subscribe(&self, symbols: &[String], events: Events<(String, Quote)>) {
        for symbol in symbols {
            let symbol = symbol.clone();
            let events = events.clone();
            std::thread::spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("Deriv quotes runtime error: {error}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    let (mut ws, _) = match connect_async(DERIV_PUBLIC_WS).await {
                        Ok(pair) => pair,
                        Err(error) => {
                            eprintln!("Deriv quotes connection error: {error}");
                            return;
                        }
                    };
                    let history = serde_json::json!({
                        "ticks_history": symbol,
                        "count": 2,
                        "end": "latest",
                        "style": "candles",
                        "granularity": 86400
                    });
                    if ws
                        .send(Message::Text(history.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let mut prev_close = 0.0;
                    while let Some(frame) = ws.next().await {
                        match frame {
                            Ok(Message::Text(text)) => {
                                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                                else {
                                    continue;
                                };
                                if value.get("error").is_some() {
                                    break;
                                }
                                let Some(candles) = value.get("candles").and_then(|v| v.as_array())
                                else {
                                    continue;
                                };
                                prev_close = candles
                                    .first()
                                    .and_then(|candle| candle.get("close")?.as_f64())
                                    .unwrap_or(0.0);
                                break;
                            }
                            Ok(Message::Ping(payload)) => {
                                if ws.send(Message::Pong(payload)).await.is_err() {
                                    return;
                                }
                            }
                            Ok(Message::Close(_)) | Err(_) => return,
                            Ok(_) => {}
                        }
                    }
                    let request = serde_json::json!({ "ticks": symbol, "subscribe": 1 });
                    if ws
                        .send(Message::Text(request.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    eprintln!("Deriv quotes subscribed: {symbol}");
                    let mut fallback = None;
                    while events.is_active() {
                        match ws.next().await {
                            Some(Ok(Message::Text(text))) => {
                                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                                else {
                                    continue;
                                };
                                if value.get("error").is_some() {
                                    break;
                                }
                                let Some(tick) = value.get("tick") else {
                                    continue;
                                };
                                let Some(epoch) = tick.get("epoch").and_then(|v| v.as_i64()) else {
                                    continue;
                                };
                                let Some(price) = tick.get("quote").and_then(|v| v.as_f64()) else {
                                    continue;
                                };
                                if !price.is_finite() || price <= 0.0 {
                                    continue;
                                }
                                let Some(time) = Utc.timestamp_opt(epoch, 0).single() else {
                                    continue;
                                };
                                let reference = if prev_close > 0.0 {
                                    prev_close
                                } else {
                                    *fallback.get_or_insert(price)
                                };
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
                                eprintln!("Deriv quotes WebSocket error: {error}");
                                break;
                            }
                        }
                    }
                    eprintln!("Deriv quotes stream ended: {symbol}");
                });
            });
        }
    }
}

/// Every document in one JSON file, so settings, drawings, layouts, notes and templates survive a
/// restart. The file lives in the user's data directory.
pub struct FileStorage(Arc<Inner>);

struct Inner {
    path: PathBuf,
    documents: Mutex<HashMap<(StoreKind, String), String>>,
}

/// The store as written: each kind, then each document of it.
type Persisted = BTreeMap<String, BTreeMap<String, String>>;

impl FileStorage {
    /// Opens the store beside the executable, reading the documents kept there. A store left in
    /// the old per-user directory seeds the new one once, so the move keeps the settings.
    pub fn open() -> Self {
        let path = default_path();
        if !path.exists()
            && let Some(legacy) = legacy_path()
            && legacy.exists()
        {
            let _ = std::fs::copy(&legacy, &path);
        }
        Self::open_at(path)
    }

    /// Opens or creates the store at `path`, reading the documents it already holds.
    pub fn open_at(path: PathBuf) -> Self {
        let documents = std::fs::read(&path).ok().map(|bytes| {
            // A text editor may add a byte-order mark, which JSON does not allow.
            let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
            match serde_json::from_slice::<Persisted>(bytes) {
                Ok(tree) => flatten(tree),
                Err(error) => {
                    eprintln!("Storage read error in {}: {error}", path.display());
                    HashMap::new()
                }
            }
        });
        Self(Arc::new(Inner {
            path,
            documents: Mutex::new(documents.unwrap_or_default()),
        }))
    }
}

impl Inner {
    fn write(&self) -> Result<(), String> {
        let mut tree = Persisted::new();
        let documents = self
            .documents
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for ((kind, name), document) in documents.iter() {
            tree.entry(kind_key(*kind).to_owned())
                .or_default()
                .insert(name.clone(), document.clone());
        }
        drop(documents);
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(&tree)
            .map_err(|error| format!("Could not encode the documents: {error}"))?;
        // shortcut: a crash during the write can truncate the store; an atomic replace needs
        // ReplaceFileW on Windows.
        std::fs::write(&self.path, json)
            .map_err(|error| format!("Could not write {}: {error}", self.path.display()))
    }
}

impl Storage for FileStorage {
    fn list(&self, kind: StoreKind, reply: Reply<Vec<String>>) {
        let documents = self
            .0
            .documents
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let names = documents
            .keys()
            .filter(|(stored, _)| *stored == kind)
            .map(|(_, name)| name.clone())
            .collect();
        drop(documents);
        reply.send(Ok(names));
    }

    fn load(&self, kind: StoreKind, name: &str, reply: Reply<Option<String>>) {
        let document = self
            .0
            .documents
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(kind, name.to_owned()))
            .cloned();
        reply.send(Ok(document));
    }

    fn save(&self, kind: StoreKind, name: &str, document: String, reply: Reply<()>) {
        // The file write runs off the UI thread, as the trait requires.
        let store = self.0.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            store
                .documents
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert((kind, name), document);
            match store.write() {
                Ok(()) => reply.send(Ok(())),
                Err(error) => {
                    eprintln!("Storage save error: {error}");
                    reply.send(Err(FeedError::Other(error)));
                }
            }
        });
    }

    fn delete(&self, kind: StoreKind, name: &str, reply: Reply<()>) {
        let store = self.0.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            store
                .documents
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(kind, name));
            match store.write() {
                Ok(()) => reply.send(Ok(())),
                Err(error) => {
                    eprintln!("Storage delete error: {error}");
                    reply.send(Err(FeedError::Other(error)));
                }
            }
        });
    }
}

/// Where the store lives: beside the executable, so a development build does not overwrite the
/// settings of another one.
pub(crate) fn default_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("storage.json")))
        .unwrap_or_else(|| PathBuf::from("storage.json"))
}

/// The per-user location the store used before it moved beside the executable.
fn legacy_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .map(|dir| dir.join("egui-charts-deriv").join("storage.json"))
}

fn kind_key(kind: StoreKind) -> &'static str {
    match kind {
        StoreKind::Settings => "settings",
        StoreKind::Drawings => "drawings",
        StoreKind::AlertPresets => "alert_presets",
        StoreKind::ChartTemplates => "chart_templates",
        StoreKind::StudyTemplates => "study_templates",
        StoreKind::DrawingTemplates => "drawing_templates",
        StoreKind::SymbolNotes => "symbol_notes",
        StoreKind::Layouts => "layouts",
        StoreKind::RecentSymbols => "recent_symbols",
        StoreKind::Screens => "screens",
        _ => "other",
    }
}

fn kind_from_key(key: &str) -> Option<StoreKind> {
    Some(match key {
        "settings" => StoreKind::Settings,
        "drawings" => StoreKind::Drawings,
        "alert_presets" => StoreKind::AlertPresets,
        "chart_templates" => StoreKind::ChartTemplates,
        "study_templates" => StoreKind::StudyTemplates,
        "drawing_templates" => StoreKind::DrawingTemplates,
        "symbol_notes" => StoreKind::SymbolNotes,
        "layouts" => StoreKind::Layouts,
        "recent_symbols" => StoreKind::RecentSymbols,
        "screens" => StoreKind::Screens,
        _ => return None,
    })
}

fn flatten(tree: Persisted) -> HashMap<(StoreKind, String), String> {
    let mut documents = HashMap::new();
    for (kind, docs) in tree {
        let Some(kind) = kind_from_key(&kind) else {
            continue;
        };
        for (name, document) in docs {
            documents.insert((kind, name), document);
        }
    }
    documents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_lives_beside_the_executable() {
        let exe = std::env::current_exe().expect("the test binary has a path");
        assert_eq!(default_path().parent(), exe.parent());
        assert_eq!(default_path().file_name().unwrap(), "storage.json");
    }

    #[test]
    fn documents_survive_a_reopen() {
        let dir = std::env::temp_dir().join(format!("egui-charts-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("storage.json");
        let storage = FileStorage::open_at(path.clone());
        storage.0.documents.lock().unwrap().insert(
            (StoreKind::Settings, "settings".to_owned()),
            "{\"v\":1}".to_owned(),
        );
        storage.0.write().expect("write");
        // An editor's byte-order mark must not lose the documents.
        let mut marked = vec![0xEF, 0xBB, 0xBF];
        marked.extend(std::fs::read(&path).expect("read"));
        std::fs::write(&path, marked).expect("mark");
        let reopened = FileStorage::open_at(path);
        let documents = reopened.0.documents.lock().unwrap();
        assert_eq!(
            documents
                .get(&(StoreKind::Settings, "settings".to_owned()))
                .map(String::as_str),
            Some("{\"v\":1}")
        );
        drop(documents);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
