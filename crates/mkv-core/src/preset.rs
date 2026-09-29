//! Canonical preset profiles for audio transcoding and container remuxing.

use serde::{Deserialize, Serialize};

/// A named configuration profile for audio transcoding.
/// Concurrency and hardware parameters (workers, io_jobs) are deliberately decoupled
/// from encoding presets per ADR-0008.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetProfile {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub bitrate: String,
    pub complexity: u32,
}

/// Returns the standard catalog of built-in preset profiles.
pub fn get_preset_profiles() -> Vec<PresetProfile> {
    vec![
        PresetProfile {
            id: "balanced".to_string(),
            name: "Balanced Anime (Opus 192k)".to_string(),
            icon: "🌸".to_string(),
            bitrate: "192k".to_string(),
            complexity: 5,
        },
        PresetProfile {
            id: "fast".to_string(),
            name: "Fast Muxing (128k)".to_string(),
            icon: "⚡".to_string(),
            bitrate: "128k".to_string(),
            complexity: 5,
        },
        PresetProfile {
            id: "hifi".to_string(),
            name: "Hi-Fi Master (320k)".to_string(),
            icon: "🎧".to_string(),
            bitrate: "320k".to_string(),
            complexity: 10,
        },
        PresetProfile {
            id: "custom".to_string(),
            name: "Custom Configuration".to_string(),
            icon: "🛠️".to_string(),
            bitrate: "192k".to_string(),
            complexity: 5,
        },
    ]
}

/// Looks up a built-in preset by case-insensitive id, accepting `default` for `balanced` and
/// `high_quality` / `high-quality` for `hifi`.
pub fn lookup_preset(id: &str) -> Option<PresetProfile> {
    let normalized = id.trim().to_ascii_lowercase();
    let target = match normalized.as_str() {
        "default" => "balanced",
        "high_quality" | "high-quality" => "hifi",
        other => other,
    };
    get_preset_profiles().into_iter().find(|p| p.id == target)
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
    fn test_preset_serialized_shape_excludes_description() {
        let preset = lookup_preset("balanced").expect("built-in preset");
        let value = serde_json::to_value(&preset).expect("preset serializes");
        let object = value.as_object().expect("preset serializes as an object");

        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["bitrate", "complexity", "icon", "id", "name"],
            "PresetProfile's serialized key set is an IPC contract with the desktop UI"
        );
        assert!(
            object["complexity"].is_u64(),
            "complexity must stay a JSON number, got {}",
            object["complexity"]
        );
    }
}
