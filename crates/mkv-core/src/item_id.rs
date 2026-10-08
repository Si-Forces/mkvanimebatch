//! The canonical identity of a discovered video, buildable only in its normalised form. The
//! module also holds `get_item_id`, the raw root-relative path string used for release
//! directories and subtitle candidates, which is not an `ItemId`.

use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Deserializer, Serialize};

/// The canonical identity of one discovered video: its path relative to the input directory,
/// `/`-separated, with no `.` segments. Whitespace in the path is part of the name and is kept:
/// trimming it would name a different file.
///
/// Event IDs, manifest item IDs, Selected Items and Track Assignment targets all compare as
/// `ItemId`s. The field is private and every constructor normalises, so two spellings of one
/// video are equal by type rather than by each call site remembering to normalise. It
/// serialises as a plain string and normalises again when deserialised.
///
/// Normalisation is the only invariant: an `ItemId` can be empty or name a path outside the
/// input directory. `BatchRequest::resolve` judges validity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl ItemId {
    /// The ID of a discovered file under `input_dir`. A path outside `input_dir` keeps its full
    /// spelling, as [`get_item_id`] reports it, then normalises as [`ItemId::parse`] does, so
    /// surrounding whitespace in the path is kept.
    pub fn of(path: &Path, input_dir: &Path) -> Self {
        Self::parse(&get_item_id(path, input_dir))
    }

    /// The ID a user-supplied relative spelling names: backslashes read as separators, `.`
    /// segments dropped and repeated separators collapsed. Surrounding whitespace is kept, as
    /// it names a different file.
    ///
    /// Infallible: whether the spelling is empty, contained, an `.mkv` file or present on disk
    /// is for `BatchRequest::resolve` to judge, because its errors quote the raw spelling.
    pub fn parse(spelling: &str) -> Self {
        let norm = spelling.replace('\\', "/");
        let parts: Vec<_> = Path::new(&norm)
            .components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();
        Self(parts.join("/"))
    }

    /// The normalised, `/`-separated spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ItemId> for String {
    fn from(id: ItemId) -> Self {
        id.0
    }
}

impl<'de> Deserialize<'de> for ItemId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|spelling| Self::parse(&spelling))
    }
}

/// Derives a root-relative POSIX path ID. The engine uses it for release directories and
/// subtitle candidates.
/// Normalizes backslashes `\` to `/`. If stripped is empty or points to root, returns `"."`.
pub fn get_item_id(path: &Path, root_dir: &Path) -> String {
    let rel = match path.strip_prefix(root_dir) {
        Ok(p) => p.to_path_buf(),
        Err(_) => {
            if let (Ok(p), Ok(r)) = (path.canonicalize(), root_dir.canonicalize()) {
                p.strip_prefix(&r)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|_| path.to_path_buf())
            } else {
                path.to_path_buf()
            }
        }
    };

    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." {
        ".".to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_item_id() {
        let root = Path::new("/anime/series");
        assert_eq!(get_item_id(Path::new("/anime/series"), root), ".");
        assert_eq!(
            get_item_id(Path::new("/anime/series/ep01.mkv"), root),
            "ep01.mkv"
        );
        assert_eq!(
            get_item_id(Path::new("/anime/series/Season 1/ep01.mkv"), root),
            "Season 1/ep01.mkv"
        );
    }

    #[test]
    fn of_and_parse_agree_across_spellings() {
        let root = Path::new("/anime");
        let id = ItemId::of(&root.join("a").join("b.mkv"), root);
        assert_eq!(id.as_str(), "a/b.mkv");
        for spelling in ["./a/b.mkv", "a//b.mkv", "a\\b.mkv", "a/./b.mkv"] {
            assert_eq!(ItemId::parse(spelling), id, "spelling {spelling:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_part_of_the_name() {
        let id = ItemId::parse(" Season 1/01.mkv");
        assert_eq!(id.as_str(), " Season 1/01.mkv");
        assert_eq!(ItemId::parse(" a/b.mkv ").as_str(), " a/b.mkv ");

        let root = Path::new("/anime");
        assert_eq!(ItemId::of(&root.join(" Season 1").join("01.mkv"), root), id);
        assert_ne!(ItemId::parse(" a.mkv"), ItemId::parse("a.mkv"));
    }

    #[test]
    fn of_equals_normalised_get_item_id() {
        let root = Path::new("/anime");
        // A path outside `input_dir` keeps its full spelling.
        assert_eq!(
            ItemId::of(Path::new("other/x.mkv"), root).as_str(),
            "other/x.mkv"
        );
        // `get_item_id` names the root `.`; normalising drops that segment.
        assert_eq!(ItemId::of(root, root).as_str(), "");
    }

    // The root component joins as `MAIN_SEPARATOR`, so the expected spelling is Unix-only.
    #[cfg(unix)]
    #[test]
    fn of_keeps_the_root_of_an_absolute_path_outside_input_dir() {
        assert_eq!(
            ItemId::of(Path::new("/other/x.mkv"), Path::new("/anime")).as_str(),
            "//other/x.mkv"
        );
    }

    #[test]
    fn serialises_as_a_plain_string_and_normalises_on_deserialise() {
        let id = ItemId::parse("a/b.mkv");
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""a/b.mkv""#);
        let parsed: ItemId = serde_json::from_str(r#""./a//b.mkv""#).unwrap();
        assert_eq!(parsed, id);
    }
}
