//! Files and formats of the Pine script store, as described in the storage document.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A script's identity, generated at creation and never changed.
pub type ScriptId = Uuid;
/// A chart layout's identity, assigned by the host application.
pub type LayoutId = String;
/// A history version's number; grows monotonically and is never reused.
pub type VersionId = u64;

/// The schema version written into every service file.
pub const SCHEMA_VERSION: u32 = 1;

/// A source with the line endings and byte-order mark a `.pine` file never keeps.
pub(crate) fn normalize(source: &str) -> String {
    let source = source.trim_start_matches('\u{feff}');
    match source.contains('\r') {
        false => source.to_owned(),
        true => source.replace("\r\n", "\n").replace('\r', "\n"),
    }
}

/// The `sha256:` hash of a source, as `meta` and the history record it.
pub(crate) fn hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(source.as_bytes());
    let mut hash = String::with_capacity(71);
    hash.push_str("sha256:");
    for byte in digest {
        hash.push_str(&format!("{byte:02x}"));
    }
    hash
}

/// Now, to the second, as the files record it.
pub(crate) fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A timestamp as a file name takes it: `2026-11-17T10-22-31`.
pub(crate) fn file_stamp(at: &str) -> String {
    at.trim_end_matches('Z').replace(':', "-")
}

/// The `//@version=N` the source declares.
pub(crate) fn pine_version(source: &str) -> Option<u32> {
    let line = source
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("//@version="))?;
    line["//@version=".len()..].trim().parse().ok()
}

/// What a script declares itself to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptKind {
    Indicator,
    Strategy,
    Library,
}

impl ScriptKind {
    /// The `library/` folder this kind lives in.
    pub fn folder(self) -> &'static str {
        match self {
            Self::Indicator => "indicators",
            Self::Strategy => "strategies",
            Self::Library => "libraries",
        }
    }

    /// The kind a source declares through its `indicator()`, `strategy()` or `library()` call.
    pub fn from_source(source: &str) -> Self {
        match (
            find_call(source, "strategy"),
            find_call(source, "library"),
            find_call(source, "indicator"),
        ) {
            (Some(_), None, _) => Self::Strategy,
            (None, Some(_), None) => Self::Library,
            _ => Self::Indicator,
        }
    }
}

/// The offset of the first `name(` call that starts a statement, skipping comments and strings.
fn find_call(source: &str, name: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut line_start = true;
    while at < bytes.len() {
        let byte = bytes[at];
        match byte {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            b'"' | b'\'' => {
                let quote = byte;
                at += 1;
                while at < bytes.len() && bytes[at] != quote {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            b'\n' => {
                line_start = true;
                at += 1;
                continue;
            }
            _ => {
                if line_start && bytes[at..].starts_with(name.as_bytes()) {
                    let rest = &source[at + name.len()..];
                    if rest.trim_start().starts_with('(') {
                        return Some(at);
                    }
                }
                line_start = matches!(byte, b' ' | b'\t' | b'\r');
            }
        }
        at += 1;
    }
    None
}

/// A `.pine` source with the metadata that points at it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptMeta {
    pub schema_version: u32,
    pub id: ScriptId,
    pub kind: ScriptKind,
    /// The display name, which is also the file stem.
    pub title: String,
    /// Path under `library/`, with `/` separators.
    pub file: String,
    #[serde(default)]
    pub pine_version: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub content_hash: String,
    /// The `.pine` file was removed outside the application.
    #[serde(default)]
    pub missing: bool,
}

/// A script and its source.
#[derive(Clone, Debug)]
pub struct ScriptSource {
    pub meta: ScriptMeta,
    pub source: String,
}

/// One history file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionMeta {
    pub n: VersionId,
    /// File name inside `history/<script_id>/`.
    pub file: String,
    pub created_at: String,
    pub hash: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

/// A saved settings template of one script.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub style: BTreeMap<String, serde_json::Value>,
}

impl Preset {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: name.into(),
            inputs: BTreeMap::new(),
            style: BTreeMap::new(),
        }
    }
}

/// What `list_presets` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetMeta {
    pub name: String,
    pub schema_version: u32,
}

/// One script placed on a chart.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptInstance {
    pub instance_id: Uuid,
    pub script_id: ScriptId,
    /// The script content the settings were made against.
    #[serde(default)]
    pub script_hash: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    /// Values by [`InputDecl::key`](crate::ast::InputDecl::key), never by position.
    #[serde(default)]
    pub inputs: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub style: BTreeMap<String, serde_json::Value>,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub pane: usize,
}

fn default_true() -> bool {
    true
}

impl ScriptInstance {
    /// Drops the values of inputs the script no longer declares, as the document's soft migration
    /// asks; the caller adds defaults for the new keys.
    pub fn retain_inputs<'a>(&mut self, keys: impl IntoIterator<Item = &'a str>) {
        let keys: Vec<&str> = keys.into_iter().collect();
        self.inputs.retain(|key, _| {
            let known = keys.iter().any(|known| known == key);
            if !known {
                log::info!("dropping the saved value of the input {key:?}, which the script dropped");
            }
            known
        });
    }
}

/// The Pine instances of one chart layout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayoutInstances {
    pub schema_version: u32,
    pub layout_id: LayoutId,
    #[serde(default)]
    pub instances: Vec<ScriptInstance>,
}

impl LayoutInstances {
    pub fn new(layout_id: impl Into<LayoutId>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            layout_id: layout_id.into(),
            instances: Vec::new(),
        }
    }
}

/// Where the store keeps its files.
#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// `<config_root>/pinescript`.
    pub dir: PathBuf,
    /// How many unpinned versions to keep.
    pub history_limit: usize,
}

impl StoreConfig {
    /// The default history limit.
    pub const HISTORY_LIMIT: usize = 100;

    /// `dir` is the `pinescript/` folder itself.
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            history_limit: Self::HISTORY_LIMIT,
        }
    }

    /// Resolves `config/pinescript/`: next to the executable when the folder is portable, in the
    /// per-user config folder otherwise.
    ///
    /// A `portable` marker file beside the executable, or an existing writable `config/` folder
    /// there, makes the store portable.
    pub fn resolve(app: &str) -> Result<Self, crate::store::StoreError> {
        use crate::store::StoreError;
        if let Some(exe) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        {
            let config = exe.join("config");
            if exe.join("portable").exists() || config.is_dir() {
                if writable(&config) {
                    return Ok(Self::in_dir(config.join("pinescript")));
                }
                log::warn!(
                    "the portable config folder {} is not writable",
                    config.display()
                );
            }
        }
        let per_user = directories::ProjectDirs::from("", "", app)
            .ok_or(StoreError::NoConfigRoot)?
            .config_dir()
            .to_path_buf();
        if writable(&per_user) {
            return Ok(Self::in_dir(per_user.join("pinescript")));
        }
        log::warn!(
            "the per-user config folder {} is not writable",
            per_user.display()
        );
        Err(StoreError::NoConfigRoot)
    }
}

/// Checks that `dir` can be created and written to, leaving no file behind.
fn writable(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".writable-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_comes_from_the_declaration_not_from_prose() {
        assert_eq!(
            ScriptKind::from_source("// a strategy() note\nindicator(\"X\")\nplot(close)"),
            ScriptKind::Indicator
        );
        assert_eq!(
            ScriptKind::from_source("strategy(\"X\")\nplot(close)"),
            ScriptKind::Strategy
        );
        assert_eq!(
            ScriptKind::from_source("//@version=5\nlibrary(\"X\")"),
            ScriptKind::Library
        );
        assert_eq!(ScriptKind::from_source("plot(close)"), ScriptKind::Indicator);
    }

    #[test]
    fn input_migration_drops_removed_keys_and_keeps_the_rest() {
        let mut instance = ScriptInstance {
            instance_id: Uuid::nil(),
            script_id: Uuid::nil(),
            script_hash: None,
            preset: None,
            inputs: BTreeMap::from([
                ("Length".to_owned(), serde_json::json!(14)),
                ("Source".to_owned(), serde_json::json!("close")),
            ]),
            style: BTreeMap::new(),
            visible: true,
            pane: 0,
        };
        instance.retain_inputs(["Length"]);
        assert_eq!(
            instance.inputs,
            BTreeMap::from([("Length".to_owned(), serde_json::json!(14))])
        );
    }
}
