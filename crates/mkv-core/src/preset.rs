//! Canonical preset profiles for audio transcoding and container remuxing.

use serde::{Deserialize, Serialize};

/// A named configuration profile for audio transcoding and container retention.
/// Concurrency and hardware parameters (workers, io_jobs) are deliberately decoupled
/// from encoding presets per ADR-0008.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetProfile {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: String,
    pub bitrate: String,
    pub complexity: u32,
    pub keep_fonts: bool,
}

/// Returns the standard catalog of built-in preset profiles.
pub fn get_preset_profiles() -> Vec<PresetProfile> {
    vec![
        PresetProfile {
            id: "balanced".to_string(),
            name: "Balanced Anime (Opus 192k)".to_string(),
            icon: "🌸".to_string(),
            description: "Opus 192k • Keep Fonts • Complexity 5".to_string(),
            bitrate: "192k".to_string(),
            complexity: 5,
            keep_fonts: true,
        },
        PresetProfile {
            id: "fast".to_string(),
            name: "Fast Muxing (128k)".to_string(),
            icon: "⚡".to_string(),
            description: "Opus 128k • Complexity 5".to_string(),
            bitrate: "128k".to_string(),
            complexity: 5,
            keep_fonts: false,
        },
        PresetProfile {
            id: "hifi".to_string(),
            name: "Hi-Fi Master (320k)".to_string(),
            icon: "🎧".to_string(),
            description: "Opus 320k • Keep Fonts • Complexity 10".to_string(),
            bitrate: "320k".to_string(),
            complexity: 10,
            keep_fonts: true,
        },
        PresetProfile {
            id: "custom".to_string(),
            name: "Custom Configuration".to_string(),
            icon: "🛠️".to_string(),
            description: "Custom user configuration".to_string(),
            bitrate: "192k".to_string(),
            complexity: 10,
            keep_fonts: false,
        },
    ]
}

/// Lookup a preset by case-insensitive identifier (supporting 'high_quality' alias for 'hifi').
pub fn lookup_preset(id: &str) -> Option<PresetProfile> {
    let normalized = id.trim().to_ascii_lowercase();
    let target = match normalized.as_str() {
        "default" | "balanced" => "balanced",
        "high_quality" | "high-quality" | "hifi" => "hifi",
        other => other,
    };
    get_preset_profiles()
        .into_iter()
        .find(|p| p.id.to_ascii_lowercase() == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preset_catalog() {
        let presets = get_preset_profiles();
        assert_eq!(presets.len(), 4);
        assert!(presets.iter().any(|p| p.id == "balanced"));
        assert!(presets.iter().any(|p| p.id == "fast"));
        assert!(presets.iter().any(|p| p.id == "hifi"));
        assert!(presets.iter().any(|p| p.id == "custom"));
    }

    #[test]
    fn test_lookup_preset_case_insensitive() {
        let preset = lookup_preset("BALANCED").expect("should find balanced preset");
        assert_eq!(preset.bitrate, "192k");
        assert_eq!(preset.complexity, 5);
        assert!(preset.keep_fonts);

        assert!(lookup_preset("non_existent").is_none());
    }

    #[test]
    fn test_lookup_preset_default_alias() {
        let preset = lookup_preset("default").expect("should find balanced preset for 'default'");
        assert_eq!(preset.id, "balanced");
        assert_eq!(preset.bitrate, "192k");
        assert_eq!(preset.complexity, 5);
        assert!(preset.keep_fonts);
    }
}
