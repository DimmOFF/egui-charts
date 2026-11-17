//! The file-backed store: plain `.pine` files, JSON metadata, and atomic writes.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::names::{inside, stem, title_of, unique_in};
use super::types::{
    LayoutId, LayoutInstances, Preset, PresetMeta, SCHEMA_VERSION, ScriptId, ScriptKind, ScriptMeta,
    ScriptSource, StoreConfig, VersionId, VersionMeta, file_stamp, hash, normalize, pine_version,
    timestamp,
};
use super::{ScriptStore, StoreError};

/// The history index, as `history/<script_id>/index.json` holds it.
#[derive(Serialize, Deserialize)]
struct HistoryIndex {
    schema_version: u32,
    script_id: ScriptId,
    #[serde(default)]
    versions: Vec<VersionMeta>,
}

/// `manifest.json`, which names the schema of the whole folder.
#[derive(Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
}

/// The step of [`ScriptStore::save`] a test makes fail, to prove the order holds.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveStep {
    /// Before the history snapshot.
    Snapshot,
    /// After the snapshot and its index, before the `.pine` file.
    Source,
    /// After the `.pine` file, before `meta`.
    Meta,
}

#[derive(Debug)]
pub struct FsScriptStore {
    config: StoreConfig,
    root: PathBuf,
    lock: Mutex<()>,
    #[cfg(test)]
    fail_at: Mutex<Option<SaveStep>>,
}

impl FsScriptStore {
    /// Opens the folder `config` names, creating it and its manifest on the first run.
    ///
    /// Registers `.pine` files it finds without metadata and marks the scripts whose file was
    /// removed outside the application.
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        let store = Self {
            root: config.dir.clone(),
            config,
            lock: Mutex::new(()),
            #[cfg(test)]
            fail_at: Mutex::new(None),
        };
        store.ensure_root()?;
        store.reconcile()?;
        Ok(store)
    }

    /// The `pinescript/` folder itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn ensure_root(&self) -> Result<(), StoreError> {
        let library = self.library();
        let meta = self.meta_dir();
        for dir in [&self.root, &library, &meta] {
            std::fs::create_dir_all(dir).map_err(|error| StoreError::io(dir, error))?;
        }
        let manifest = self.root.join("manifest.json");
        match read_json::<Manifest>(&manifest)? {
            Some(manifest) if manifest.schema_version > SCHEMA_VERSION => {
                Err(StoreError::VersionTooNew {
                    found: manifest.schema_version,
                    supported: SCHEMA_VERSION,
                })
            }
            Some(_) => Ok(()),
            None => write_json(
                &manifest,
                &Manifest {
                    schema_version: SCHEMA_VERSION,
                },
            ),
        }
    }

    /// Registers `.pine` files that have no metadata and marks the ones that lost their file.
    fn reconcile(&self) -> Result<(), StoreError> {
        let mut metas = self.metas()?;
        for meta in &mut metas {
            let missing = !self.source_path(meta).is_ok_and(|path| path.is_file());
            if missing != meta.missing {
                meta.missing = missing;
                write_json(&self.meta_path(meta.id), meta)?;
            }
        }
        for kind in [ScriptKind::Indicator, ScriptKind::Strategy, ScriptKind::Library] {
            let dir = self.kind_dir(kind);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("pine") {
                    continue;
                }
                let Some(file) = relative(&self.library(), &path) else {
                    continue;
                };
                if metas.iter().any(|meta| meta.file == file) {
                    continue;
                }
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let source = normalize(&source);
                let at = timestamp();
                let meta = ScriptMeta {
                    schema_version: SCHEMA_VERSION,
                    id: Uuid::new_v4(),
                    kind: ScriptKind::from_source(&source),
                    title: title_of(&path),
                    file,
                    pine_version: pine_version(&source),
                    tags: Vec::new(),
                    created_at: at.clone(),
                    updated_at: at,
                    content_hash: hash(&source),
                    missing: false,
                };
                log::info!("registering the script {}", path.display());
                write_json(&self.meta_path(meta.id), &meta)?;
            }
        }
        Ok(())
    }

    fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    fn kind_dir(&self, kind: ScriptKind) -> PathBuf {
        self.library().join(kind.folder())
    }

    fn meta_dir(&self) -> PathBuf {
        self.root.join("meta")
    }

    fn meta_path(&self, id: ScriptId) -> PathBuf {
        self.meta_dir().join(format!("{id}.json"))
    }

    fn history_dir(&self, id: ScriptId) -> PathBuf {
        self.root.join("history").join(id.to_string())
    }

    fn presets_dir(&self, id: ScriptId) -> PathBuf {
        self.root.join("presets").join(id.to_string())
    }

    fn instances_path(&self, layout: &LayoutId) -> PathBuf {
        self.root
            .join("instances")
            .join(format!("{}.json", stem(layout)))
    }

    fn trash_dir(&self, id: ScriptId) -> PathBuf {
        self.root.join("trash").join(id.to_string())
    }

    /// The `.pine` path `meta.file` names, kept inside `library/`.
    fn source_path(&self, meta: &ScriptMeta) -> Result<PathBuf, StoreError> {
        inside(&self.library(), &meta.file)
            .ok_or_else(|| StoreError::UnsafePath(meta.file.clone()))
    }

    fn find(&self, id: ScriptId) -> Result<ScriptMeta, StoreError> {
        read_json(&self.meta_path(id))?.ok_or(StoreError::NotFound(id))
    }

    fn metas(&self) -> Result<Vec<ScriptMeta>, StoreError> {
        let dir = self.meta_dir();
        let entries = std::fs::read_dir(&dir).map_err(|error| StoreError::io(&dir, error))?;
        let mut metas = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::io(&dir, error))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            if let Some(meta) = read_json::<ScriptMeta>(&path)? {
                metas.push(meta);
            }
        }
        Ok(metas)
    }

    fn index(&self, id: ScriptId) -> Result<Option<HistoryIndex>, StoreError> {
        read_json(&self.history_dir(id).join("index.json"))
    }

    fn read_version(&self, id: ScriptId, version: &VersionMeta) -> Result<String, StoreError> {
        let path = self.history_dir(id).join(&version.file);
        std::fs::read_to_string(&path)
            .map(|source| normalize(&source))
            .map_err(|error| StoreError::io(&path, error))
    }

    /// Writes one snapshot and its index, deduplicating against the newest version.
    fn push_version(
        &self,
        id: ScriptId,
        source: &str,
        note: Option<&str>,
    ) -> Result<VersionId, StoreError> {
        let dir = self.history_dir(id);
        let mut index = self.index(id)?.unwrap_or_else(|| HistoryIndex {
            schema_version: SCHEMA_VERSION,
            script_id: id,
            versions: Vec::new(),
        });
        let content = hash(source);
        if let Some(newest) = index.versions.last()
            && newest.hash == content
        {
            return Ok(newest.n);
        }
        let at = timestamp();
        let n = index.versions.last().map_or(1, |version| version.n + 1);
        let file = format!("{n:04}_{}.pine", file_stamp(&at));
        write_atomic(&dir.join(&file), source.as_bytes())?;
        index.versions.push(VersionMeta {
            n,
            file,
            created_at: at,
            hash: content,
            note: note.map(str::to_owned),
            pinned: false,
        });
        let pruned = prune(&mut index.versions, self.config.history_limit);
        write_json(&dir.join("index.json"), &index)?;
        for file in pruned {
            if let Err(error) = std::fs::remove_file(dir.join(&file)) {
                log::warn!("could not remove the pruned history file {file}: {error}");
            }
        }
        Ok(n)
    }

    #[cfg(test)]
    pub(crate) fn fail_at(&self, step: SaveStep) {
        *self
            .fail_at
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(step);
    }

    #[cfg(test)]
    fn check_fail(&self, step: SaveStep) -> Result<(), StoreError> {
        let mut at = self
            .fail_at
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *at == Some(step) {
            *at = None;
            return Err(StoreError::io(
                Path::new("<failpoint>"),
                std::io::Error::other("the test made this step fail"),
            ));
        }
        Ok(())
    }
}

impl ScriptStore for FsScriptStore {
    fn list(&self, kind: Option<ScriptKind>) -> Result<Vec<ScriptMeta>, StoreError> {
        let mut metas: Vec<ScriptMeta> = self
            .metas()?
            .into_iter()
            .filter(|meta| kind.is_none_or(|kind| kind == meta.kind))
            .collect();
        metas.sort_by_cached_key(|meta| meta.title.to_lowercase());
        Ok(metas)
    }

    fn load(&self, id: ScriptId) -> Result<ScriptSource, StoreError> {
        let mut meta = self.find(id)?;
        let path = self.source_path(&meta)?;
        match std::fs::read_to_string(&path) {
            Ok(source) => Ok(ScriptSource {
                meta,
                source: normalize(&source),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Removed outside the application: the newest snapshot is what is left of it.
                meta.missing = true;
                write_json(&self.meta_path(id), &meta)?;
                let versions = self.index(id)?.map(|index| index.versions).unwrap_or_default();
                let source = match versions.last() {
                    Some(version) => self.read_version(id, version)?,
                    None => String::new(),
                };
                log::warn!("{} is missing; its newest snapshot was loaded", path.display());
                Ok(ScriptSource { meta, source })
            }
            Err(error) => Err(StoreError::io(&path, error)),
        }
    }

    fn create(&self, kind: ScriptKind, title: &str, src: &str) -> Result<ScriptId, StoreError> {
        let _guard = self.lock();
        let id = Uuid::new_v4();
        let source = normalize(src);
        let (name, path) = unique_in(&self.kind_dir(kind), title, "pine");
        let title = match title.trim() {
            "" => title_of(&path),
            title => title.to_owned(),
        };
        let at = timestamp();
        let meta = ScriptMeta {
            schema_version: SCHEMA_VERSION,
            id,
            kind,
            title,
            file: format!("{}/{}", kind.folder(), name),
            pine_version: pine_version(&source),
            tags: Vec::new(),
            created_at: at.clone(),
            updated_at: at,
            content_hash: hash(&source),
            missing: false,
        };
        self.push_version(id, &source, None)?;
        write_atomic(&path, source.as_bytes())?;
        write_json(&self.meta_path(id), &meta)?;
        Ok(id)
    }

    fn save(
        &self,
        id: ScriptId,
        src: &str,
        note: Option<&str>,
    ) -> Result<Option<VersionId>, StoreError> {
        let _guard = self.lock();
        let mut meta = self.find(id)?;
        let source = normalize(src);
        let content = hash(&source);
        if content == meta.content_hash {
            if meta.missing {
                write_atomic(&self.source_path(&meta)?, source.as_bytes())?;
                meta.missing = false;
                write_json(&self.meta_path(id), &meta)?;
            }
            return Ok(None);
        }
        // History first, so a failure never loses the content the meta still points at.
        #[cfg(test)]
        self.check_fail(SaveStep::Snapshot)?;
        let version = self.push_version(id, &source, note)?;
        #[cfg(test)]
        self.check_fail(SaveStep::Source)?;
        write_atomic(&self.source_path(&meta)?, source.as_bytes())?;
        #[cfg(test)]
        self.check_fail(SaveStep::Meta)?;
        meta.content_hash = content;
        meta.updated_at = timestamp();
        meta.missing = false;
        write_json(&self.meta_path(id), &meta)?;
        Ok(Some(version))
    }

    fn rename(&self, id: ScriptId, new_title: &str) -> Result<(), StoreError> {
        let _guard = self.lock();
        let mut meta = self.find(id)?;
        let title = new_title.trim();
        if title.is_empty() || title == meta.title {
            return Ok(());
        }
        let old = self.source_path(&meta)?;
        let source = match std::fs::read_to_string(&old) {
            Ok(source) => normalize(&source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(StoreError::io(&old, error)),
        };
        // The file name is the title; a title that sanitizes to the same stem stays where it is.
        let same = old.file_stem().and_then(|stem| stem.to_str()) == Some(stem(title).as_str());
        let moved = if same {
            None
        } else {
            let (name, path) = unique_in(&self.kind_dir(meta.kind), title, "pine");
            write_atomic(&path, source.as_bytes())?;
            Some((format!("{}/{}", meta.kind.folder(), name), old))
        };
        if let Some((file, _)) = &moved {
            meta.file = file.clone();
        }
        meta.title = title.to_owned();
        meta.updated_at = timestamp();
        write_json(&self.meta_path(id), &meta)?;
        if let Some((_, old)) = moved
            && let Err(error) = std::fs::remove_file(&old)
        {
            log::warn!("could not remove {}: {error}", old.display());
        }
        Ok(())
    }

    fn delete(&self, id: ScriptId) -> Result<(), StoreError> {
        let _guard = self.lock();
        let meta = self.find(id)?;
        let trash = self.trash_dir(id);
        write_json(&trash.join("meta.json"), &meta)?;
        let source = self.source_path(&meta)?;
        match std::fs::read(&source) {
            Ok(bytes) => write_atomic(&trash.join("source.pine"), &bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StoreError::io(&source, error)),
        }
        move_into(&self.history_dir(id), &trash.join("history"))?;
        move_into(&self.presets_dir(id), &trash.join("presets"))?;
        remove_file(&source)?;
        remove_file(&self.meta_path(id))
    }

    fn restore_from_trash(&self, id: ScriptId) -> Result<(), StoreError> {
        let _guard = self.lock();
        let trash = self.trash_dir(id);
        let mut meta: ScriptMeta =
            read_json(&trash.join("meta.json"))?.ok_or(StoreError::NotFound(id))?;
        move_into(&trash.join("history"), &self.history_dir(id))?;
        move_into(&trash.join("presets"), &self.presets_dir(id))?;
        // The library name may have been taken while the script sat in the trash.
        let taken = inside(&self.library(), &meta.file).filter(|path| !path.exists());
        let (target, file) = match taken {
            Some(path) => {
                let file = meta.file.clone();
                (path, file)
            }
            None => {
                let (name, path) = unique_in(&self.kind_dir(meta.kind), &meta.title, "pine");
                (path, format!("{}/{}", meta.kind.folder(), name))
            }
        };
        let source = match std::fs::read(trash.join("source.pine")) {
            Ok(source) => Some(normalize(&String::from_utf8_lossy(&source))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(StoreError::io(&trash.join("source.pine"), error)),
        };
        let source = match source {
            Some(source) => source,
            None => match self.index(id)?.and_then(|mut index| index.versions.pop()) {
                Some(version) => self.read_version(id, &version)?,
                None => String::new(),
            },
        };
        meta.file = file;
        meta.missing = false;
        meta.updated_at = timestamp();
        meta.content_hash = hash(&source);
        write_atomic(&target, source.as_bytes())?;
        write_json(&self.meta_path(id), &meta)?;
        remove_file(&trash.join("source.pine"))?;
        remove_file(&trash.join("meta.json"))?;
        let _ = std::fs::remove_dir(&trash);
        Ok(())
    }

    fn history(&self, id: ScriptId) -> Result<Vec<VersionMeta>, StoreError> {
        Ok(self.index(id)?.map(|index| index.versions).unwrap_or_default())
    }

    fn load_version(&self, id: ScriptId, v: VersionId) -> Result<ScriptSource, StoreError> {
        let mut meta = self.find(id)?;
        let index = self.index(id)?.ok_or(StoreError::VersionNotFound(v))?;
        let version = index
            .versions
            .iter()
            .find(|version| version.n == v)
            .ok_or(StoreError::VersionNotFound(v))?
            .clone();
        let source = self.read_version(id, &version)?;
        meta.content_hash = version.hash;
        meta.updated_at = version.created_at;
        Ok(ScriptSource { meta, source })
    }

    fn restore_version(&self, id: ScriptId, v: VersionId) -> Result<VersionId, StoreError> {
        let source = self.load_version(id, v)?.source;
        self.save(id, &source, Some(&format!("restored from version {v}")))?;
        let content = hash(&normalize(&source));
        self.history(id)?
            .into_iter()
            .rev()
            .find(|version| version.hash == content)
            .map(|version| version.n)
            .ok_or(StoreError::VersionNotFound(v))
    }

    fn pin_version(&self, id: ScriptId, v: VersionId, pinned: bool) -> Result<(), StoreError> {
        let _guard = self.lock();
        let mut index = self.index(id)?.ok_or(StoreError::VersionNotFound(v))?;
        let version = index
            .versions
            .iter_mut()
            .find(|version| version.n == v)
            .ok_or(StoreError::VersionNotFound(v))?;
        version.pinned = pinned;
        write_json(&self.history_dir(id).join("index.json"), &index)
    }

    fn list_presets(&self, id: ScriptId) -> Result<Vec<PresetMeta>, StoreError> {
        let dir = self.presets_dir(id);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(StoreError::io(&dir, error)),
        };
        let mut presets = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::io(&dir, error))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            if let Some(preset) = read_json::<Preset>(&path)? {
                presets.push(PresetMeta {
                    name: preset.name,
                    schema_version: preset.schema_version,
                });
            }
        }
        presets.sort_by_cached_key(|preset| preset.name.to_lowercase());
        Ok(presets)
    }

    fn load_preset(&self, id: ScriptId, name: &str) -> Result<Preset, StoreError> {
        let path = self.presets_dir(id).join(format!("{}.json", stem(name)));
        read_json(&path)?.ok_or_else(|| StoreError::PresetNotFound(name.to_owned()))
    }

    fn save_preset(&self, id: ScriptId, preset: &Preset) -> Result<(), StoreError> {
        let _guard = self.lock();
        let mut preset = preset.clone();
        preset.schema_version = SCHEMA_VERSION;
        let path = self
            .presets_dir(id)
            .join(format!("{}.json", stem(&preset.name)));
        write_json(&path, &preset)
    }

    fn delete_preset(&self, id: ScriptId, name: &str) -> Result<(), StoreError> {
        let _guard = self.lock();
        let path = self.presets_dir(id).join(format!("{}.json", stem(name)));
        remove_file(&path)
    }

    fn load_instances(&self, layout: &LayoutId) -> Result<LayoutInstances, StoreError> {
        Ok(
            read_json(&self.instances_path(layout))?
                .unwrap_or_else(|| LayoutInstances::new(layout.clone())),
        )
    }

    fn save_instances(
        &self,
        layout: &LayoutId,
        data: &LayoutInstances,
    ) -> Result<(), StoreError> {
        let _guard = self.lock();
        let mut data = data.clone();
        data.schema_version = SCHEMA_VERSION;
        data.layout_id = layout.clone();
        write_json(&self.instances_path(layout), &data)
    }
}

/// Drops the oldest unpinned versions until `limit` remain; returns the files to delete.
fn prune(versions: &mut Vec<VersionMeta>, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut pruned = Vec::new();
    let mut at = 0;
    while versions.len() > limit && at < versions.len() {
        if versions[at].pinned {
            at += 1;
            continue;
        }
        pruned.push(versions.remove(at).file);
    }
    pruned
}

/// `path` relative to `root`, with `/` separators, when it is inside.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let components: Vec<&str> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<_>>()?;
    Some(components.join("/"))
}

/// Writes `bytes` through a temporary file in the same folder, synced and renamed over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write;

    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| StoreError::io(parent, error))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| StoreError::io(parent, error))?;
    temp.write_all(bytes)
        .map_err(|error| StoreError::io(path, error))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| StoreError::io(path, error))?;
    temp.persist(path)
        .map_err(|error| StoreError::io(path, error.error))?;
    sync_dir(parent);
    Ok(())
}

/// Makes the rename itself durable, which needs the folder opened as a handle; Windows cannot.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_: &Path) {}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let json = serde_json::to_vec_pretty(value).map_err(|source| StoreError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    write_atomic(path, &json)
}

/// The decoded JSON at `path`, or `None` when it is absent.
///
/// A file that no longer parses is moved to `*.corrupt-<timestamp>` and reported as absent, so a
/// hand-edited file costs one script or version instead of the whole folder.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::io(path, error)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let aside = corrupt_name(path);
            log::warn!(
                "{} does not parse: {error}; moved to {}",
                path.display(),
                aside.display()
            );
            if let Err(error) = std::fs::rename(path, &aside) {
                log::warn!("could not move {} aside: {error}", path.display());
            }
            Ok(None)
        }
    }
}

/// A free `*.corrupt-<timestamp>` name beside `path`.
fn corrupt_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("data");
    let stamp = file_stamp(&timestamp());
    let mut name = format!("{stem}.corrupt-{stamp}");
    let mut copy = 2;
    while path.with_file_name(&name).exists() {
        name = format!("{stem}.corrupt-{stamp}-{copy}");
        copy += 1;
    }
    path.with_file_name(name)
}

/// Moves a file or folder, doing nothing when `from` is absent.
fn move_into(from: &Path, to: &Path) -> Result<(), StoreError> {
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        // A copy already in the trash is the older one; the live data wins, nothing is lost.
        log::warn!("{} already exists; kept {}", to.display(), from.display());
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|error| StoreError::io(parent, error))?;
    }
    std::fs::rename(from, to).map_err(|error| StoreError::io(from, error))
}

/// Removes a file, treating an absent one as removed.
fn remove_file(path: &Path) -> Result<(), StoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::io(path, error)),
    }
}
