//! The store against a real folder: the document's section 12, minus the watcher and the cache.

use std::fs;

use super::fs_store::{FsScriptStore, SaveStep};
use super::types::{Preset, ScriptKind, ScriptMeta, StoreConfig};
use super::{MemoryScriptStore, ScriptStore, StoreError};

fn store() -> (tempfile::TempDir, FsScriptStore) {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let config = StoreConfig::in_dir(dir.path().join("pinescript"));
    let store = FsScriptStore::open(config).expect("a store in the folder");
    (dir, store)
}

fn root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("pinescript")
}

fn titles(store: &FsScriptStore) -> Vec<String> {
    store
        .list(None)
        .expect("the scripts")
        .into_iter()
        .map(|meta| meta.title)
        .collect()
}

const RSI: &str = "//@version=6\nindicator(\"RSI\")\nplot(close)\n";

#[test]
fn create_load_rename_delete_and_restore() {
    let (dir, store) = store();
    let id = store
        .create(ScriptKind::Indicator, "RSI", RSI)
        .expect("created");
    let source = store.load(id).expect("loaded");
    assert_eq!(source.source, RSI);
    assert_eq!(source.meta.title, "RSI");
    assert_eq!(source.meta.file, "indicators/RSI.pine");
    assert_eq!(source.meta.pine_version, Some(6));
    assert!(root(&dir).join("library/indicators/RSI.pine").is_file());
    // A new script starts with its own version, which an unchanged save does not duplicate.
    assert_eq!(store.history(id).expect("history").len(), 1);
    assert_eq!(store.save(id, RSI, None).expect("saved"), None);

    store.rename(id, "My RSI").expect("renamed");
    let source = store.load(id).expect("loaded");
    assert_eq!(source.meta.title, "My RSI");
    assert_eq!(source.meta.file, "indicators/My RSI.pine");
    assert!(!root(&dir).join("library/indicators/RSI.pine").exists());
    assert_eq!(titles(&store), vec!["My RSI"]);

    store.delete(id).expect("deleted");
    assert!(titles(&store).is_empty());
    assert!(matches!(store.load(id), Err(StoreError::NotFound(_))));
    assert!(root(&dir)
        .join(format!("trash/{id}/source.pine"))
        .is_file());
    assert!(root(&dir).join(format!("trash/{id}/meta.json")).is_file());
    assert!(root(&dir).join(format!("trash/{id}/history")).is_dir());

    store.restore_from_trash(id).expect("restored");
    let source = store.load(id).expect("loaded");
    assert_eq!(source.source, RSI);
    assert_eq!(titles(&store), vec!["My RSI"]);
    assert_eq!(store.history(id).expect("history").len(), 1);
    assert!(!root(&dir).join(format!("trash/{id}")).exists());
}

#[test]
fn two_scripts_of_one_name_keep_separate_files() {
    let (dir, store) = store();
    let first = store.create(ScriptKind::Indicator, "RSI", RSI).unwrap();
    let second = store.create(ScriptKind::Indicator, "RSI", RSI).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        store.load(second).unwrap().meta.file,
        "indicators/RSI_2.pine"
    );
    assert!(root(&dir).join("library/indicators/RSI_2.pine").is_file());
    assert_eq!(store.list(Some(ScriptKind::Indicator)).unwrap().len(), 2);
    assert!(store.list(Some(ScriptKind::Strategy)).unwrap().is_empty());
}

#[test]
fn a_save_snapshots_only_changed_source_and_prunes_to_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = StoreConfig::in_dir(dir.path().join("pinescript"));
    config.history_limit = 3;
    let store = FsScriptStore::open(config).unwrap();
    let id = store.create(ScriptKind::Indicator, "RSI", "a").unwrap();
    assert_eq!(store.save(id, "a", None).unwrap(), None);
    for source in ["b", "c", "d", "e"] {
        store.save(id, source, Some("edit")).unwrap();
    }
    let history = store.history(id).unwrap();
    assert_eq!(
        history.iter().map(|version| version.n).collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    assert_eq!(fs::read_dir(root(&dir).join(format!("history/{id}")))
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "pine"))
        .count(), 3);
    assert_eq!(store.load(id).unwrap().source, "e");
}

#[test]
fn a_pinned_version_survives_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = StoreConfig::in_dir(dir.path().join("pinescript"));
    config.history_limit = 2;
    let store = FsScriptStore::open(config).unwrap();
    let id = store.create(ScriptKind::Indicator, "RSI", "a").unwrap();
    store.save(id, "b", None).unwrap();
    store.pin_version(id, 1, true).unwrap();
    store.save(id, "c", None).unwrap();
    store.save(id, "d", None).unwrap();
    let history = store.history(id).unwrap();
    // The pin holds its slot: only one unpinned version is left beside it.
    assert_eq!(
        history.iter().map(|version| version.n).collect::<Vec<_>>(),
        vec![1, 4]
    );
    assert!(history[0].pinned);
    assert_eq!(store.load_version(id, 1).unwrap().source, "a");
}

#[test]
fn a_rollback_writes_the_old_source_as_a_new_version() {
    let (_dir, store) = store();
    let id = store.create(ScriptKind::Indicator, "RSI", "a").unwrap();
    store.save(id, "b", None).unwrap();
    store.save(id, "c", None).unwrap();
    let version = store.restore_version(id, 1).unwrap();
    assert_eq!(version, 4);
    assert_eq!(store.load(id).unwrap().source, "a");
    let history = store.history(id).unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history[3].hash, store.load(id).unwrap().meta.content_hash);
    assert_eq!(store.load_version(id, 2).unwrap().source, "b");
}

#[test]
fn a_corrupt_meta_is_moved_aside_and_its_file_registered_again() {
    let (dir, store) = store();
    let id = store.create(ScriptKind::Indicator, "RSI", RSI).unwrap();
    let meta = root(&dir).join("meta").join(format!("{id}.json"));
    fs::write(&meta, "{ this is not json").unwrap();
    assert!(store.list(None).unwrap().is_empty());
    let aside: Vec<_> = fs::read_dir(root(&dir).join("meta"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".corrupt-"))
        .collect();
    assert_eq!(aside.len(), 1);

    let config = StoreConfig::in_dir(root(&dir));
    let reopened = FsScriptStore::open(config).unwrap();
    let metas = reopened.list(None).unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].title, "RSI");
    assert_ne!(metas[0].id, id);
}

#[test]
fn a_file_added_outside_is_registered_and_a_removed_one_kept() {
    let (dir, store) = store();
    let id = store.create(ScriptKind::Indicator, "RSI", RSI).unwrap();
    fs::write(
        root(&dir).join("library/indicators/mine.pine"),
        "strategy(\"Mine\")\nplot(close)\n",
    )
    .unwrap();
    fs::remove_file(root(&dir).join("library/indicators/RSI.pine")).unwrap();

    let reopened = FsScriptStore::open(StoreConfig::in_dir(root(&dir))).unwrap();
    let metas = reopened.list(None).unwrap();
    assert_eq!(metas.len(), 2);
    let mine = metas
        .iter()
        .find(|meta| meta.title == "mine")
        .expect("the file added outside");
    assert_eq!(mine.kind, ScriptKind::Strategy);
    let missing: &ScriptMeta = metas
        .iter()
        .find(|meta| meta.id == id)
        .expect("the script whose file was removed");
    assert!(missing.missing);
    // The newest snapshot is what a missing file still offers.
    assert_eq!(reopened.load(id).unwrap().source, RSI);
}

#[test]
fn names_and_layouts_cannot_leave_the_store() {
    let (dir, store) = store();
    let id = store
        .create(ScriptKind::Indicator, "../evil/../rsi?", RSI)
        .unwrap();
    let file = store.load(id).unwrap().meta.file;
    assert_eq!(file, "indicators/.._evil_.._rsi_.pine");
    assert!(root(&dir).join("library").join(&file).is_file());
    assert!(!root(&dir).join("meta/evil").exists());

    let layout = "../../layout".to_owned();
    let data = super::LayoutInstances::new(layout.clone());
    store.save_instances(&layout, &data).unwrap();
    assert!(root(&dir).join("instances/.._.._layout.json").is_file());
    assert_eq!(store.load_instances(&layout).unwrap().layout_id, layout);
    assert!(store.load_instances(&"unknown".to_owned()).unwrap().instances.is_empty());
}

#[test]
fn a_folder_from_a_newer_build_is_not_written_to() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pinescript");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("manifest.json"), r#"{"schema_version":2}"#).unwrap();
    let error = FsScriptStore::open(StoreConfig::in_dir(root.clone())).unwrap_err();
    assert!(matches!(
        error,
        StoreError::VersionTooNew {
            found: 2,
            supported: 1
        }
    ));
    assert_eq!(
        fs::read_to_string(root.join("manifest.json")).unwrap(),
        r#"{"schema_version":2}"#
    );
}

#[test]
fn a_failed_save_keeps_the_old_file_and_recovers_on_the_next_one() {
    let (_dir, store) = store();
    let id = store.create(ScriptKind::Indicator, "RSI", "a").unwrap();
    store.fail_at(SaveStep::Source);
    assert!(store.save(id, "b", None).is_err());
    // The snapshot is already written, the file and the meta are not: the working copy is `a`.
    assert_eq!(store.load(id).unwrap().source, "a");
    assert_eq!(store.save(id, "b", None).unwrap(), Some(2));
    assert_eq!(store.load(id).unwrap().source, "b");
    // The retry reused the snapshot instead of writing a second one.
    assert_eq!(store.history(id).unwrap().len(), 2);
}

#[test]
fn a_failure_before_the_snapshot_leaves_the_store_untouched() {
    let (_dir, store) = store();
    let id = store.create(ScriptKind::Indicator, "RSI", "a").unwrap();
    store.fail_at(SaveStep::Snapshot);
    assert!(store.save(id, "b", None).is_err());
    assert_eq!(store.load(id).unwrap().source, "a");
    assert_eq!(store.history(id).unwrap().len(), 1);
}

#[test]
fn presets_round_trip_and_do_not_leave_their_folder() {
    let (dir, store) = store();
    let id = store.create(ScriptKind::Indicator, "RSI", RSI).unwrap();
    let mut preset = Preset::new("../scalping");
    preset.inputs.insert("Length".to_owned(), serde_json::json!(7));
    store.save_preset(id, &preset).unwrap();
    assert!(root(&dir).join(format!("presets/{id}/.._scalping.json")).is_file());
    assert_eq!(store.list_presets(id).unwrap().len(), 1);
    assert_eq!(store.load_preset(id, "../scalping").unwrap().name, "../scalping");
    assert_eq!(
        store.load_preset(id, "../scalping").unwrap().inputs["Length"],
        serde_json::json!(7)
    );
    store.delete_preset(id, "../scalping").unwrap();
    assert!(store.list_presets(id).unwrap().is_empty());
    assert!(matches!(
        store.load_preset(id, "missing"),
        Err(StoreError::PresetNotFound(_))
    ));
    // Presets outlive the script in the trash and come back with it.
    store.save_preset(id, &preset).unwrap();
    store.delete(id).unwrap();
    store.restore_from_trash(id).unwrap();
    assert_eq!(store.list_presets(id).unwrap().len(), 1);
}

#[test]
fn the_memory_store_keeps_the_same_api() {
    let store = MemoryScriptStore::new();
    let id = store
        .create(ScriptKind::Indicator, "RSI", RSI)
        .expect("created");
    assert_eq!(store.load(id).unwrap().source, RSI);
    assert_eq!(store.save(id, RSI, None).unwrap(), None);
    assert_eq!(store.save(id, "b", None).unwrap(), Some(2));
    assert_eq!(store.restore_version(id, 1).unwrap(), 3);
    assert_eq!(store.load(id).unwrap().source, RSI);
    store.pin_version(id, 1, true).unwrap();
    assert!(store.history(id).unwrap()[0].pinned);
    store.rename(id, "Mine").unwrap();
    assert_eq!(store.load(id).unwrap().meta.title, "Mine");
    store.delete(id).unwrap();
    assert!(store.list(None).unwrap().is_empty());
    store.restore_from_trash(id).unwrap();
    assert_eq!(store.list(None).unwrap().len(), 1);
    assert_eq!(store.load(id).unwrap().source, RSI);
    let layout = "layout".to_owned();
    store
        .save_instances(&layout, &super::LayoutInstances::new(layout.clone()))
        .unwrap();
    assert!(store.load_instances(&layout).unwrap().instances.is_empty());
}
