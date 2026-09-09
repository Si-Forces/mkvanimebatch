//! Track, font, chapter, and cover extraction using mkvextract.

use std::path::{Path, PathBuf};

use crate::domain::{ep_type_label, select_best_subtitle_track, EpisodeInfo};
use crate::scan::InspectedVideo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractTarget {
    pub cmd_type: String, // "tracks", "chapters", "attachments"
    pub arg_id: Option<u32>,
    pub final_path: PathBuf,
    pub item_name: String,
}

fn episode_suffix(parsed: Option<&EpisodeInfo>, file_stem: &str) -> String {
    match parsed {
        Some(info) => format!("{}{}", ep_type_label(&info.ep_type, false), info.ep),
        None => file_stem.to_string(),
    }
}

/// Picks the best ASS subtitle track for extraction and computes its output path.
pub fn select_subtitle_for_extraction(
    inspected: &InspectedVideo,
    file_stem: &str,
    file_dir: &Path,
    parsed: Option<&EpisodeInfo>,
) -> Option<(u32, PathBuf)> {
    let supported = ["s_text/ass".to_string()].into_iter().collect();
    let result = select_best_subtitle_track(&inspected.subtitle_tracks(), Some(&supported));
    let (track_id, language, _) = result?;

    let ep_suffix = episode_suffix(parsed, file_stem);
    let sub_path = file_dir.join(format!(
        "{file_stem}_{ep_suffix}_{language}_track{track_id}.ass"
    ));

    Some((track_id, sub_path))
}

/// Builds command-line arguments for `mkvextract` invocation.
pub fn build_mkvextract_args(
    mkv_file: &Path,
    cmd_type: &str,
    items: &[(u32, PathBuf)],
) -> Vec<String> {
    let mut args = vec![mkv_file.to_string_lossy().to_string(), cmd_type.to_string()];
    for (id, path) in items {
        args.push(format!("{id}:{}", path.to_string_lossy()));
    }
    args
}
