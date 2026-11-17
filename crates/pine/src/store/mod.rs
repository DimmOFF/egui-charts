//! The Pine script store.
//!
//! Three entities live apart, because they change on different occasions:
//!
//! * a **script** is a `.pine` source in `library/` plus its [`ScriptMeta`] in `meta/`;
//! * **instances** are the settings of the scripts placed on a chart layout, in `instances/`;
//! * **history** is a version snapshot per explicit save, in `history/<script_id>/`.
//!
//! ```text
//! pinescript/
//! ├── manifest.json
//! ├── library/{indicators,strategies,libraries}/*.pine
//! ├── meta/<script_id>.json
//! ├── presets/<script_id>/<preset>.json
//! ├── instances/<layout_id>.json
//! ├── history/<script_id>/{index.json,0001_<timestamp>.pine}
//! ├── cache/compiled/
//! └── trash/<script_id>/
//! ```
//!
//! Sources are plain UTF-8 files with LF line endings, so they open in any editor. A script is
//! referenced by its [`ScriptId`], never by its file name: renaming a script keeps every
//! reference. Every write goes to a temporary file in the target's folder, is synced and then
//! renamed over the target, so a crash leaves either the old or the new file.
//!
//! The rest of the application uses only [`ScriptStore`]; [`FsScriptStore`] keeps the files under
//! [`StoreConfig::dir`], and [`MemoryScriptStore`] does the same in memory for tests and hosts
//! that keep their own files.

mod fs_store;
mod memory;
mod names;
mod types;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

pub use fs_store::FsScriptStore;
pub use memory::MemoryScriptStore;
pub use types::{
    LayoutId, LayoutInstances, Preset, PresetMeta, SCHEMA_VERSION, ScriptId, ScriptInstance,
    ScriptKind, ScriptMeta, ScriptSource, StoreConfig, VersionId, VersionMeta,
};

/// Everything the store can refuse.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("script {0} was not found")]
    NotFound(ScriptId),
    #[error("history version {0} was not found")]
    VersionNotFound(VersionId),
    #[error("preset {0:?} was not found")]
    PresetNotFound(String),
    #[error("the stored path {0:?} leads outside the store")]
    UnsafePath(String),
    #[error("the folder is schema version {found}, this build knows {supported}")]
    VersionTooNew { found: u32, supported: u32 },
    #[error("no writable config folder")]
    NoConfigRoot,
}

impl StoreError {
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// The storage API. A host keeps everything script-related behind this trait.
pub trait ScriptStore: Send + Sync {
    /// The scripts of one kind, or all of them, sorted by title.
    fn list(&self, kind: Option<ScriptKind>) -> Result<Vec<ScriptMeta>, StoreError>;
    fn load(&self, id: ScriptId) -> Result<ScriptSource, StoreError>;
    fn create(&self, kind: ScriptKind, title: &str, src: &str) -> Result<ScriptId, StoreError>;
    /// Writes `src` and snapshots it when it differs from the last version.
    fn save(
        &self,
        id: ScriptId,
        src: &str,
        note: Option<&str>,
    ) -> Result<Option<VersionId>, StoreError>;
    /// Renames the script and its `.pine` file; the id does not change.
    fn rename(&self, id: ScriptId, new_title: &str) -> Result<(), StoreError>;
    /// Moves the script to `trash/`, keeping its history and presets.
    fn delete(&self, id: ScriptId) -> Result<(), StoreError>;
    fn restore_from_trash(&self, id: ScriptId) -> Result<(), StoreError>;

    /// The versions of a script, oldest first.
    fn history(&self, id: ScriptId) -> Result<Vec<VersionMeta>, StoreError>;
    fn load_version(&self, id: ScriptId, v: VersionId) -> Result<ScriptSource, StoreError>;
    /// Writes the old content as a new version; nothing is erased.
    fn restore_version(&self, id: ScriptId, v: VersionId) -> Result<VersionId, StoreError>;
    /// A pinned version is never pruned.
    fn pin_version(&self, id: ScriptId, v: VersionId, pinned: bool) -> Result<(), StoreError>;

    fn list_presets(&self, id: ScriptId) -> Result<Vec<PresetMeta>, StoreError>;
    fn load_preset(&self, id: ScriptId, name: &str) -> Result<Preset, StoreError>;
    fn save_preset(&self, id: ScriptId, preset: &Preset) -> Result<(), StoreError>;
    fn delete_preset(&self, id: ScriptId, name: &str) -> Result<(), StoreError>;

    /// The instances of a layout; an unknown layout has none.
    fn load_instances(&self, layout: &LayoutId) -> Result<LayoutInstances, StoreError>;
    fn save_instances(
        &self,
        layout: &LayoutId,
        data: &LayoutInstances,
    ) -> Result<(), StoreError>;
}
