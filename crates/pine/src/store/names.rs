//! File names for scripts: safe on every platform, unique inside a folder, and never able to
//! leave the store's own folders.

use std::path::{Component, Path, PathBuf};

/// Windows refuses these names with any extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The longest stem kept, so `<config>/pinescript/library/<kind>/<stem>.pine` stays well inside
/// every platform's path limit.
const MAX_STEM: usize = 96;

/// The file stem of a display name: invalid characters replaced, reserved names escaped.
pub(crate) fn stem(title: &str) -> String {
    let mut stem: String = title
        .chars()
        .map(|character| {
            if character.is_control() || "/\\:*?\"<>|".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    stem = stem.trim().trim_end_matches(['.', ' ']).to_owned();
    if stem.chars().count() > MAX_STEM {
        stem = stem.chars().take(MAX_STEM).collect();
    }
    if stem.is_empty() || stem == "." || stem == ".." {
        return "script".to_owned();
    }
    let upper = stem.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return format!("_{stem}");
    }
    stem
}

/// A file name that no file in `dir` uses yet, with `_2`, `_3`, ... on a conflict.
pub(crate) fn unique_in(dir: &Path, title: &str, extension: &str) -> (String, PathBuf) {
    let base = stem(title);
    let mut candidate = format!("{base}.{extension}");
    let mut copy = 2;
    while dir.join(&candidate).exists() {
        candidate = format!("{base}_{copy}.{extension}");
        copy += 1;
    }
    let path = dir.join(&candidate);
    (candidate, path)
}

/// The display name of a `.pine` file: its stem.
pub(crate) fn title_of(file: &Path) -> String {
    file.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Pine script")
        .to_owned()
}

/// A path kept inside `root`: relative, with no `..`, no absolute prefix and no root directory.
pub(crate) fn inside(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = relative.replace('\\', "/");
    let path = Path::new(&relative);
    if path.is_absolute() || path.has_root() {
        return None;
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_the_characters_no_file_system_takes() {
        assert_eq!(stem("my/rsi?v2"), "my_rsi_v2");
        assert_eq!(stem("  name...  "), "name");
        assert_eq!(stem(""), "script");
        assert_eq!(stem(".."), "script");
        assert_eq!(stem("con"), "_con");
        assert_eq!(stem("NUL"), "_NUL");
    }

    #[test]
    fn a_conflict_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rsi.pine"), "//").unwrap();
        let (name, path) = unique_in(dir.path(), "rsi", "pine");
        assert_eq!(name, "rsi_2.pine");
        assert!(path.ends_with("rsi_2.pine"));
    }

    #[test]
    fn paths_cannot_escape_their_root() {
        let root = Path::new("/tmp/store");
        assert!(inside(root, "indicators/rsi.pine").is_some());
        assert!(inside(root, "../rsi.pine").is_none());
        assert!(inside(root, "indicators/../../rsi.pine").is_none());
        assert!(inside(root, "/etc/passwd").is_none());
        assert!(inside(root, "//server/share/rsi.pine").is_none());
    }
}
