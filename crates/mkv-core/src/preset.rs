//! Canonical preset profiles for audio transcoding and container remuxing.

use serde::{Deserialize, Serialize};

/// A named configuration profile for audio transcoding.
/// Concurrency and hardware parameters (workers, io_jobs) are deliberately decoupled
/// from encoding presets per ADR-0008.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetProfile {
    pub id: String,
    pub name: String,
    pub bitrate: String,
    pub complexity: u32,
}

/// Returns the standard catalog of built-in preset profiles.
pub fn get_preset_profiles() -> Vec<PresetProfile> {
    vec![
        PresetProfile {
            id: "balanced".to_string(),
            name: "Balanced".to_string(),
            bitrate: "192k".to_string(),
            complexity: 5,
        },
        PresetProfile {
            id: "fast".to_string(),
            name: "Fast muxing".to_string(),
            bitrate: "128k".to_string(),
            complexity: 5,
        },
        PresetProfile {
            id: "hifi".to_string(),
            name: "Hi-Fi master".to_string(),
            bitrate: "320k".to_string(),
            complexity: 10,
        },
        PresetProfile {
            id: "custom".to_string(),
            name: "Custom".to_string(),
            bitrate: "192k".to_string(),
            complexity: 5,
        },
    ]
}

/// Looks up a built-in preset by case-insensitive id, accepting `default` for `balanced` and
/// `high_quality` / `high-quality` for `hifi`.
pub fn lookup_preset(id: &str) -> Option<PresetProfile> {
    let trimmed = id.trim();
    let target = if trimmed.eq_ignore_ascii_case("default") {
        "balanced"
    } else if trimmed.eq_ignore_ascii_case("high_quality")
        || trimmed.eq_ignore_ascii_case("high-quality")
    {
        "hifi"
    } else {
        trimmed
    };
    get_preset_profiles()
        .into_iter()
        .find(|p| p.id.eq_ignore_ascii_case(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_preset_ids_and_aliases() {
        for (query, id) in [
            ("BALANCED", "balanced"),
            (" default ", "balanced"),
            ("fast", "fast"),
            ("HiFi", "hifi"),
            ("high_quality", "hifi"),
            ("High-Quality", "hifi"),
            ("custom", "custom"),
        ] {
            let preset = lookup_preset(query).unwrap_or_else(|| panic!("no preset for {query:?}"));
            assert_eq!(preset.id, id, "query {query:?}");
        }
        assert!(lookup_preset("non_existent").is_none());
    }

    #[test]
    fn test_preset_serialized_shape_keeps_desktop_fields() {
        let preset = lookup_preset("balanced").expect("built-in preset");
        let value = serde_json::to_value(&preset).expect("preset serializes");
        let object = value.as_object().expect("preset serializes as an object");

        // Pin the fields the desktop reads by name, not the exhaustive key set: adding a field
        // is a legitimate change, whereas renaming or dropping one breaks
        // `createPresetMenu().load` in desktop-tauri/ui/preset-menu.js.
        for key in ["id", "name", "bitrate", "complexity"] {
            assert!(
                object.contains_key(key),
                "the desktop UI reads preset field {key}"
            );
        }
        assert!(
            object["complexity"].is_u64(),
            "complexity must stay a JSON number, got {}",
            object["complexity"]
        );
    }
}
