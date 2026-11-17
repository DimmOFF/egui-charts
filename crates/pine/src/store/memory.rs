//! The store without files: the same API for tests and hosts that keep storage themselves.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use uuid::Uuid;

use super::names::stem;
use super::types::{
    LayoutId, LayoutInstances, Preset, PresetMeta, SCHEMA_VERSION, ScriptId, ScriptKind, ScriptMeta,
    ScriptSource, VersionId, VersionMeta, file_stamp, hash, normalize, pine_version, timestamp,
};
use super::{ScriptStore, StoreError};

#[derive(Default)]
pub struct MemoryScriptStore {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    scripts: BTreeMap<ScriptId, Script>,
    layouts: BTreeMap<LayoutId, LayoutInstances>,
}

#[derive(Clone)]
struct Script {
    meta: ScriptMeta,
    source: String,
    history: Vec<(VersionMeta, String)>,
    presets: BTreeMap<String, Preset>,
    trashed: bool,
}

impl MemoryScriptStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn script<'a>(state: &'a mut State, id: ScriptId) -> Result<&'a mut Script, StoreError> {
        state
            .scripts
            .get_mut(&id)
            .filter(|script| !script.trashed)
            .ok_or(StoreError::NotFound(id))
    }
}

impl ScriptStore for MemoryScriptStore {
    fn list(&self, kind: Option<ScriptKind>) -> Result<Vec<ScriptMeta>, StoreError> {
        let state = self.state();
        let mut metas: Vec<ScriptMeta> = state
            .scripts
            .values()
            .filter(|script| !script.trashed)
            .map(|script| script.meta.clone())
            .filter(|meta| kind.is_none_or(|kind| kind == meta.kind))
            .collect();
        metas.sort_by_cached_key(|meta| meta.title.to_lowercase());
        Ok(metas)
    }

    fn load(&self, id: ScriptId) -> Result<ScriptSource, StoreError> {
        let state = self.state();
        let script = state
            .scripts
            .get(&id)
            .filter(|script| !script.trashed)
            .ok_or(StoreError::NotFound(id))?;
        Ok(ScriptSource {
            meta: script.meta.clone(),
            source: script.source.clone(),
        })
    }

    fn create(&self, kind: ScriptKind, title: &str, src: &str) -> Result<ScriptId, StoreError> {
        let mut state = self.state();
        let id = Uuid::new_v4();
        let title = match title.trim() {
            "" => "Pine script".to_owned(),
            title => title.trim().to_owned(),
        };
        let at = timestamp();
        let source = normalize(src);
        let meta = ScriptMeta {
            schema_version: SCHEMA_VERSION,
            id,
            kind,
            file: format!("{}/{}.pine", kind.folder(), stem(&title)),
            title,
            pine_version: pine_version(&source),
            tags: Vec::new(),
            created_at: at.clone(),
            updated_at: at,
            content_hash: hash(&source),
            missing: false,
        };
        let version = VersionMeta {
            n: 1,
            file: format!("0001_{}.pine", file_stamp(&meta.created_at)),
            created_at: meta.created_at.clone(),
            hash: meta.content_hash.clone(),
            note: None,
            pinned: false,
        };
        state.scripts.insert(
            id,
            Script {
                meta,
                source: source.clone(),
                history: vec![(version, source)],
                presets: BTreeMap::new(),
                trashed: false,
            },
        );
        Ok(id)
    }

    fn save(
        &self,
        id: ScriptId,
        src: &str,
        note: Option<&str>,
    ) -> Result<Option<VersionId>, StoreError> {
        let mut state = self.state();
        let script = Self::script(&mut state, id)?;
        let source = normalize(src);
        let content = hash(&source);
        if content == script.meta.content_hash {
            return Ok(None);
        }
        let newest = script.history.last().map_or(1, |(meta, _)| meta.n + 1);
        let newest = match script.history.last().filter(|(meta, _)| meta.hash == content) {
            Some((meta, _)) => meta.n,
            None => {
                let at = timestamp();
                script.history.push((
                    VersionMeta {
                        n: newest,
                        file: format!("{newest:04}_{}.pine", file_stamp(&at)),
                        created_at: at,
                        hash: content.clone(),
                        note: note.map(str::to_owned),
                        pinned: false,
                    },
                    source.clone(),
                ));
                newest
            }
        };
        let limit = 100;
        while script.history.len() > limit {
            let Some(at) = script.history.iter().position(|(meta, _)| !meta.pinned) else {
                break;
            };
            script.history.remove(at);
        }
        script.source = source;
        script.meta.content_hash = content;
        script.meta.updated_at = timestamp();
        script.meta.missing = false;
        Ok(Some(newest))
    }

    fn rename(&self, id: ScriptId, new_title: &str) -> Result<(), StoreError> {
        let mut state = self.state();
        let script = Self::script(&mut state, id)?;
        let title = new_title.trim();
        if title.is_empty() {
            return Ok(());
        }
        script.meta.title = title.to_owned();
        script.meta.file = format!("{}/{}.pine", script.meta.kind.folder(), stem(title));
        script.meta.updated_at = timestamp();
        Ok(())
    }

    fn delete(&self, id: ScriptId) -> Result<(), StoreError> {
        let mut state = self.state();
        Self::script(&mut state, id)?.trashed = true;
        Ok(())
    }

    fn restore_from_trash(&self, id: ScriptId) -> Result<(), StoreError> {
        let mut state = self.state();
        let script = state.scripts.get_mut(&id).ok_or(StoreError::NotFound(id))?;
        script.trashed = false;
        Ok(())
    }

    fn history(&self, id: ScriptId) -> Result<Vec<VersionMeta>, StoreError> {
        let state = self.state();
        let script = state.scripts.get(&id).ok_or(StoreError::NotFound(id))?;
        Ok(script
            .history
            .iter()
            .map(|(meta, _)| meta.clone())
            .collect())
    }

    fn load_version(&self, id: ScriptId, v: VersionId) -> Result<ScriptSource, StoreError> {
        let state = self.state();
        let script = state.scripts.get(&id).ok_or(StoreError::NotFound(id))?;
        let (version, source) = script
            .history
            .iter()
            .find(|(meta, _)| meta.n == v)
            .ok_or(StoreError::VersionNotFound(v))?;
        let mut meta = script.meta.clone();
        meta.content_hash = version.hash.clone();
        meta.updated_at = version.created_at.clone();
        Ok(ScriptSource {
            meta,
            source: source.clone(),
        })
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
        let mut state = self.state();
        let script = Self::script(&mut state, id)?;
        let (meta, _) = script
            .history
            .iter_mut()
            .find(|(meta, _)| meta.n == v)
            .ok_or(StoreError::VersionNotFound(v))?;
        meta.pinned = pinned;
        Ok(())
    }

    fn list_presets(&self, id: ScriptId) -> Result<Vec<PresetMeta>, StoreError> {
        let state = self.state();
        let script = state.scripts.get(&id).ok_or(StoreError::NotFound(id))?;
        Ok(script
            .presets
            .values()
            .map(|preset| PresetMeta {
                name: preset.name.clone(),
                schema_version: preset.schema_version,
            })
            .collect())
    }

    fn load_preset(&self, id: ScriptId, name: &str) -> Result<Preset, StoreError> {
        let state = self.state();
        let script = state.scripts.get(&id).ok_or(StoreError::NotFound(id))?;
        script
            .presets
            .get(&stem(name))
            .cloned()
            .ok_or_else(|| StoreError::PresetNotFound(name.to_owned()))
    }

    fn save_preset(&self, id: ScriptId, preset: &Preset) -> Result<(), StoreError> {
        let mut state = self.state();
        let script = Self::script(&mut state, id)?;
        let mut preset = preset.clone();
        preset.schema_version = SCHEMA_VERSION;
        script.presets.insert(stem(&preset.name), preset);
        Ok(())
    }

    fn delete_preset(&self, id: ScriptId, name: &str) -> Result<(), StoreError> {
        let mut state = self.state();
        Self::script(&mut state, id)?.presets.remove(&stem(name));
        Ok(())
    }

    fn load_instances(&self, layout: &LayoutId) -> Result<LayoutInstances, StoreError> {
        Ok(self
            .state()
            .layouts
            .get(layout)
            .cloned()
            .unwrap_or_else(|| LayoutInstances::new(layout.clone())))
    }

    fn save_instances(
        &self,
        layout: &LayoutId,
        data: &LayoutInstances,
    ) -> Result<(), StoreError> {
        let mut data = data.clone();
        data.schema_version = SCHEMA_VERSION;
        data.layout_id = layout.clone();
        self.state().layouts.insert(layout.clone(), data);
        Ok(())
    }
}
