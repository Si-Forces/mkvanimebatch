//! Track, font, chapter, and cover extraction using mkvextract.

use std::path::{Path, PathBuf};

use crate::domain::{
    parse_episode, promote_file, safe_filename, select_best_subtitle_track, EpisodeInfo,
    COVER_EXTENSIONS, SUBPROCESS_TIMEOUT,
};
use crate::fonts::{FontClaimsGuard, FontPromotion, FontRegistry};
use crate::media_tool::{self, MediaTool, MediaToolRunner};
use crate::scan::{path_ext_lower, InspectedVideo};

#[derive(Debug, Default, Clone)]
pub struct ExtractSummary {
    pub extracted_sub: Option<PathBuf>,
    pub extracted_chapters: Option<PathBuf>,
    pub extracted_cover: Option<PathBuf>,
    pub extracted_fonts: usize,
    pub log: Vec<String>,
}

/// Stem shared by extracted subtitle and chapter files: `{stem}_{EpLabel}{ep}` when the
/// episode parses, otherwise the bare file stem.
fn sidecar_stem(file_stem: &str, parsed: Option<&EpisodeInfo>) -> String {
    match parsed {
        Some(info) => format!("{file_stem}_{}{}", info.ep_type.label(), info.ep),
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
    let supported = ["s_text/ass".to_string(), "s_text/ssa".to_string()]
        .into_iter()
        .collect();
    let (track_id, language) =
        select_best_subtitle_track(&inspected.subtitle_tracks(), Some(&supported))?;

    let stem = sidecar_stem(file_stem, parsed);
    let safe_lang = safe_filename(&language);
    let sub_path = file_dir.join(format!("{stem}_{safe_lang}_track{track_id}.ass"));
    Some((track_id, sub_path))
}

/// Whether mkvextract left a non-empty regular file at `path`.
fn has_content(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Promotes an artifact whose failure must not fail the file: a failed promotion is logged
/// as a warning and yields `None`.
fn promote_optional(
    staged: &Path,
    final_path: PathBuf,
    label: &str,
    log: &mut Vec<String>,
) -> Option<PathBuf> {
    if !has_content(staged) {
        return None;
    }
    match promote_file(staged, &final_path) {
        Ok(()) => {
            log.push(format!("   -> {label}: {}", final_path.display()));
            Some(final_path)
        }
        Err(e) => {
            log.push(format!(
                "   -> WARNING: Failed to promote {} to '{}': {e}",
                label.to_ascii_lowercase(),
                final_path.display()
            ));
            None
        }
    }
}

/// Extracts best-match subtitles, chapters, cover images, and new fonts from an MKV file with
/// one `mkvextract` run through `runner`.
pub async fn extract_mkv_file(
    mkv_path: &Path,
    inspected: &InspectedVideo,
    font_registry: &FontRegistry,
    out_dir: Option<&Path>,
    on_progress: Option<&(dyn Fn(f64) + Send + Sync)>,
    runner: &dyn MediaToolRunner,
) -> Result<ExtractSummary, String> {
    let mut summary = ExtractSummary {
        log: vec![format!("Processing: {}", mkv_path.display())],
        ..Default::default()
    };

    let file_stem = mkv_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".to_string());
    let dest_dir = out_dir.unwrap_or_else(|| mkv_path.parent().unwrap_or(Path::new(".")));
    if let Err(e) = std::fs::create_dir_all(dest_dir) {
        return Err(format!(
            "Failed to create destination directory '{}': {e}",
            dest_dir.display()
        ));
    }

    let parsed = parse_episode(&file_stem);
    let chapter_stem = sidecar_stem(&file_stem, parsed.as_ref());
    let sub_target =
        select_subtitle_for_extraction(inspected, &file_stem, dest_dir, parsed.as_ref());
    let chapter_target = inspected
        .has_chapters()
        .then(|| dest_dir.join(format!("{chapter_stem}_chapters.xml")));

    let cover_target = inspected.cover_attachment().and_then(|cover| {
        let ext = cover
            .file_name
            .as_deref()
            .map(|f| path_ext_lower(Path::new(f)))
            .filter(|e| COVER_EXTENSIONS.contains(&e.as_str()))
            .unwrap_or_else(|| ".jpg".to_string());
        let final_cover = dest_dir.join(format!("{file_stem}_cover{ext}"));
        if final_cover.exists() {
            None
        } else {
            cover.id.map(|id| (id, final_cover))
        }
    });

    let mut font_claims = FontClaimsGuard::new(font_registry);
    for attachment in inspected.font_attachments() {
        if let (Some(id), Some(file_name)) = (attachment.id, attachment.file_name.as_ref()) {
            let size = attachment.size.unwrap_or(0);
            if let Some(claim) = font_registry.claim(file_name, size) {
                font_claims.push(id, claim);
            }
        }
    }

    let has_work = sub_target.is_some()
        || chapter_target.is_some()
        || cover_target.is_some()
        || !font_claims.is_empty();
    if !has_work {
        summary
            .log
            .push("   -> No tracks, chapters, covers, or new fonts to extract.".to_string());
        return Ok(summary);
    }

    // Subtitle, chapter, and cover staging lives here, so dropping it on any early return
    // removes them; font staging is released by the `FontClaimsGuard`.
    let temp_dir = tempfile::Builder::new()
        .prefix(".tmp_extract_")
        .tempdir_in(dest_dir)
        .map_err(|e| format!("Failed to create temporary extraction directory: {e}"))?;

    let mut extract_args = vec![mkv_path.to_string_lossy().into_owned()];

    let stage_target = |path: &Path, kind: &str| -> Result<PathBuf, String> {
        let name = path
            .file_name()
            .ok_or_else(|| format!("{kind} target path has no file name: {}", path.display()))?;
        Ok(temp_dir.path().join(name))
    };

    let mut staged_sub: Option<(PathBuf, PathBuf)> = None;
    if let Some((tid, final_path)) = &sub_target {
        let staged = stage_target(final_path, "Subtitle")?;
        extract_args.push("tracks".into());
        extract_args.push(format!("{tid}:{}", staged.display()));
        staged_sub = Some((staged, final_path.clone()));
    }

    let mut staged_chapters: Option<(PathBuf, PathBuf)> = None;
    if let Some(final_path) = &chapter_target {
        let staged = stage_target(final_path, "Chapter")?;
        extract_args.push("chapters".into());
        extract_args.push(staged.to_string_lossy().into_owned());
        staged_chapters = Some((staged, final_path.clone()));
    }

    let mut staged_cover: Option<(PathBuf, PathBuf)> = None;
    if cover_target.is_some() || !font_claims.is_empty() {
        extract_args.push("attachments".into());
        if let Some((aid, final_path)) = &cover_target {
            let staged = stage_target(final_path, "Cover")?;
            extract_args.push(format!("{aid}:{}", staged.display()));
            staged_cover = Some((staged, final_path.clone()));
        }
        for (aid, claim) in font_claims.claims() {
            extract_args.push(format!("{aid}:{}", claim.staging_path.display()));
        }
    }

    let output = media_tool::run(
        runner,
        MediaTool::Mkvextract,
        &extract_args,
        SUBPROCESS_TIMEOUT,
        on_progress,
    )
    .await?;
    summary.log.extend(output.warning_log_line());

    if let Some((staged, final_path)) = staged_sub {
        if has_content(&staged) {
            promote_file(&staged, &final_path).map_err(|e| {
                format!(
                    "Failed to promote subtitle to '{}': {e}",
                    final_path.display()
                )
            })?;
            summary
                .log
                .push(format!("   -> Subtitle: {}", final_path.display()));
            summary.extracted_sub = Some(final_path);
        }
    }

    if let Some((staged, final_path)) = staged_chapters {
        summary.extracted_chapters =
            promote_optional(&staged, final_path, "Chapters", &mut summary.log);
    }

    if let Some((staged, final_path)) = staged_cover {
        summary.extracted_cover = promote_optional(&staged, final_path, "Cover", &mut summary.log);
    }

    while let Some((_, mut claim)) = font_claims.pop() {
        match font_registry.promote(&mut claim) {
            FontPromotion::Promoted(_) => summary.extracted_fonts += 1,
            FontPromotion::Duplicate => {}
            FontPromotion::Failed(reason) => summary.log.push(format!(
                "   -> WARNING: Font '{}' was not saved: {reason}",
                claim.name()
            )),
        }
    }

    if summary.extracted_fonts > 0 {
        summary.log.push(format!(
            "   -> Extracted {} new font(s).",
            summary.extracted_fonts
        ));
    }

    Ok(summary)
}
