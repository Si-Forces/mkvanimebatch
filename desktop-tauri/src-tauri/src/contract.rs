pub const CONTRACT_VERSION: u64 = mkv_core::manifest::MANIFEST_VERSION;

pub type ExtraFields = serde_json::Map<String, serde_json::Value>;

pub use mkv_core::manifest::{InspectionManifest, InspectionMetadata, ManifestItem};
pub use mkv_core::pipeline::{ItemOutcome, JobEvent};

pub trait JobEventExt {
    fn attach_context(&mut self, mode: &str, job_id: u64);
}

impl JobEventExt for JobEvent {
    fn attach_context(&mut self, mode: &str, job_id: u64) {
        let extra = match self {
            Self::BatchStart { extra, .. }
            | Self::ItemStart { extra, .. }
            | Self::ItemProgress { extra, .. }
            | Self::ItemComplete { extra, .. }
            | Self::BatchComplete { extra, .. } => extra,
        };
        extra.insert(
            "mode".to_string(),
            serde_json::Value::String(mode.to_string()),
        );
        extra.insert(
            "job_id".to_string(),
            serde_json::Value::Number(job_id.into()),
        );
    }
}

pub fn parse_manifest_json(bytes: &[u8]) -> Result<InspectionManifest, String> {
    if bytes.is_empty() {
        return Err("Sidecar returned empty stdout.".to_string());
    }

    let manifest: InspectionManifest = serde_json::from_slice(bytes).map_err(|error| {
        let prefix = String::from_utf8_lossy(bytes)
            .chars()
            .take(100)
            .collect::<String>();
        format!("Failed to parse JSON: {error}. Got: {prefix}")
    })?;

    if manifest.version != CONTRACT_VERSION {
        return Err(format!(
            "Unsupported manifest version. Expected version {CONTRACT_VERSION}."
        ));
    }

    Ok(manifest)
}

pub fn parse_job_event(value: &str) -> Result<JobEvent, serde_json::Error> {
    serde_json::from_str(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip_preserves_additive_fields() {
        let input = serde_json::json!({
            "version": 2,
            "future_root": {"enabled": true},
            "subtitle_candidates": {
                ".": ["shared.ass"],
                "Season 01": ["Season 01/Show - 01.ass", "shared.ass"]
            },
            "items": [{
                "id": "Season 01/Show - 01.mkv",
                "name": "Show - 01.mkv",
                "release": "Season 01",
                "future_item": [1, 2, 3],
                "metadata": {
                    "resolution": "1080p",
                    "video_codec": "HEVC",
                    "video_tag": "1080p HEVC",
                    "audio_summary": "FLAC 2.0",
                    "has_flac": true,
                    "sub_summary": "ASS (IND)",
                    "fonts_count": 5,
                    "future_metadata": "preserved"
                }
            }]
        });

        let manifest = parse_manifest_json(input.to_string().as_bytes()).unwrap();
        assert_eq!(manifest.items[0].release, "Season 01");
        assert_eq!(
            manifest.subtitle_candidates["Season 01"],
            vec!["Season 01/Show - 01.ass", "shared.ass"]
        );
        let output = serde_json::to_value(manifest).unwrap();
        assert_eq!(output["future_root"], input["future_root"]);
        assert_eq!(output["subtitle_candidates"], input["subtitle_candidates"]);
        assert_eq!(
            output["items"][0]["future_item"],
            input["items"][0]["future_item"]
        );
        assert_eq!(
            output["items"][0]["metadata"]["future_metadata"],
            input["items"][0]["metadata"]["future_metadata"]
        );
    }

    #[test]
    fn manifest_rejects_unsupported_version() {
        let stale = parse_manifest_json(br#"{"version":1,"items":[]}"#).unwrap_err();
        assert!(stale.contains("Unsupported manifest version"));
        let future = parse_manifest_json(br#"{"version":3,"items":[]}"#).unwrap_err();
        assert!(future.contains("Unsupported manifest version"));
    }

    #[test]
    fn manifest_rejects_empty_output() {
        assert!(parse_manifest_json(b"").is_err());
    }

    #[test]
    fn event_round_trip_preserves_additive_fields_and_attaches_context() {
        let input = r#"{"version":1,"type":"item_complete","id":"ep.mkv","file":"ep.mkv","status":"completed","future_event":{"x":1}}"#;
        let mut event = parse_job_event(input).unwrap();
        event.attach_context("remux", 42);
        let output = serde_json::to_value(event).unwrap();
        assert_eq!(output["future_event"], serde_json::json!({"x": 1}));
        assert_eq!(output["mode"], "remux");
        assert_eq!(output["job_id"], 42);
    }

    #[test]
    fn batch_complete_deserializes_when_cancelled_omitted() {
        let input = r#"{"version":1,"type":"batch_complete","success":true}"#;
        let event = parse_job_event(input).unwrap();
        match event {
            JobEvent::BatchComplete {
                success, cancelled, ..
            } => {
                assert!(success);
                assert!(!cancelled);
            }
            _ => panic!("Expected BatchComplete variant"),
        }
    }
}
