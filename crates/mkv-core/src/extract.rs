//! Track, font, chapter, and cover extraction using mkvextract.

use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::domain::{
    ep_type_label, parse_episode, promote_file, read_progress_stream, select_best_subtitle_track,
    EpisodeInfo, COVER_EXTENSIONS,
};
use crate::fonts::{FontClaim, FontRegistry};
use crate::scan::InspectedVideo;

#[derive(Debug, Default, Clone)]
pub struct ExtractSummary {
    pub mkv_path: PathBuf,
    pub extracted_sub: Option<PathBuf>,
    pub extracted_chapters: Option<PathBuf>,
    pub extracted_cover: Option<PathBuf>,
    pub extracted_fonts: usize,
    pub log: Vec<String>,
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
    let supported = ["s_text/ass".to_string(), "s_text/ssa".to_string()]
        .into_iter()
        .collect();
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

/// Extracts best-match subtitles, chapters, cover images, and new fonts from an MKV file.
pub async fn extract_mkv_file(
    mkv_path: &Path,
    inspected: &InspectedVideo,
    font_manager: &FontRegistry,
    out_dir: Option<&Path>,
    on_progress: Option<&(dyn Fn(f64) + Send + Sync)>,
) -> Result<ExtractSummary, String> {
    let mut summary = ExtractSummary {
        mkv_path: mkv_path.to_path_buf(),
        ..Default::default()
    };
    summary
        .log
        .push(format!("Processing: {}", mkv_path.display()));

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
    let chapter_stem = if let Some(p) = &parsed {
        format!("{file_stem}_{}", episode_suffix(Some(p), &file_stem))
    } else {
        file_stem.clone()
    };
    let sub_target =
        select_subtitle_for_extraction(inspected, &file_stem, dest_dir, parsed.as_ref());
    let chapter_target = if inspected.has_chapters() {
        Some(dest_dir.join(format!("{chapter_stem}_chapters.xml")))
    } else {
        None
    };

    let cover_target = if let Some(cover) = inspected.cover_attachment() {
        let raw_ext = cover
            .file_name
            .as_ref()
            .and_then(|f| {
                Path::new(f)
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
            })
            .unwrap_or_else(|| ".jpg".to_string());
        let ext = if COVER_EXTENSIONS.contains(&raw_ext.as_str()) {
            raw_ext
        } else {
            ".jpg".to_string()
        };
        let final_cover = dest_dir.join(format!("{file_stem}_cover{ext}"));
        if final_cover.exists() {
            None
        } else {
            cover.id.map(|id| (id, final_cover))
        }
    } else {
        None
    };

    let mut font_claims: Vec<(u32, FontClaim)> = Vec::new();
    for attachment in inspected.font_attachments() {
        if let (Some(id), Some(file_name)) = (attachment.id, attachment.file_name.as_ref()) {
            let size = attachment.size.unwrap_or(0);
            if let Some(claim) = font_manager.claim(file_name, size) {
                font_claims.push((id, claim));
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

    let temp_dir = tempfile::Builder::new()
        .prefix(".tmp_extract_")
        .tempdir_in(dest_dir)
        .map_err(|e| format!("Failed to create temporary extraction directory: {e}"))?;

    let mut extract_args = vec![mkv_path.to_string_lossy().into_owned()];

    let mut staged_sub: Option<(PathBuf, PathBuf)> = None;
    if let Some((tid, final_path)) = &sub_target {
        let staged = temp_dir.path().join(final_path.file_name().unwrap());
        extract_args.push("tracks".into());
        extract_args.push(format!("{tid}:{}", staged.display()));
        staged_sub = Some((staged, final_path.clone()));
    }

    let mut staged_chapters: Option<(PathBuf, PathBuf)> = None;
    if let Some(final_path) = &chapter_target {
        let staged = temp_dir.path().join(final_path.file_name().unwrap());
        extract_args.push("chapters".into());
        extract_args.push(staged.to_string_lossy().into_owned());
        staged_chapters = Some((staged, final_path.clone()));
    }

    let mut staged_cover: Option<(PathBuf, PathBuf)> = None;
    if cover_target.is_some() || !font_claims.is_empty() {
        extract_args.push("attachments".into());
        if let Some((aid, final_path)) = &cover_target {
            let staged = temp_dir.path().join(final_path.file_name().unwrap());
            extract_args.push(format!("{aid}:{}", staged.display()));
            staged_cover = Some((staged, final_path.clone()));
        }
        for (aid, claim) in &font_claims {
            extract_args.push(format!("{aid}:{}", claim.staging_path.display()));
        }
    }

    let mut cmd = Command::new("mkvextract");
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    cmd.kill_on_drop(true);
    cmd.args(&extract_args);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            for (_, mut claim) in font_claims {
                font_manager.release(&mut claim);
            }
            return Err(format!("Failed to run mkvextract: {e}"));
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut stderr_bytes = Vec::new();
    let stderr_handle = tokio::spawn(async move {
        if let Some(mut stream) = stderr {
            use tokio::io::AsyncReadExt;
            let _ = stream.read_to_end(&mut stderr_bytes).await;
        }
        stderr_bytes
    });

    let mut stdout_bytes = Vec::new();
    if let Some(out_stream) = stdout {
        let _ = read_progress_stream(out_stream, on_progress, Some(&mut stdout_bytes)).await;
    }

    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => {
            for (_, mut claim) in font_claims {
                font_manager.release(&mut claim);
            }
            return Err(format!("Failed to wait on mkvextract: {e}"));
        }
    };
    let stderr_output = stderr_handle.await.unwrap_or_default();

    let code = status.code().unwrap_or(-1);
    if code > 1 {
        let stderr = String::from_utf8_lossy(&stderr_output).trim().to_string();
        let stdout = String::from_utf8_lossy(&stdout_bytes).trim().to_string();
        let err_msg = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit code {code}")
        };
        for (_, mut claim) in font_claims {
            font_manager.release(&mut claim);
        }
        return Err(format!("mkvextract failed: {err_msg}"));
    }

    if let Some((staged, final_path)) = staged_sub {
        if staged.is_file() && staged.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            match promote_file(&staged, &final_path) {
                Ok(()) => {
                    summary
                        .log
                        .push(format!("   -> Subtitle: {}", final_path.display()));
                    summary.extracted_sub = Some(final_path);
                }
                Err(e) => {
                    summary.log.push(format!(
                        "   -> WARNING: Failed to promote subtitle to '{}': {e}",
                        final_path.display()
                    ));
                }
            }
        }
    }

    if let Some((staged, final_path)) = staged_chapters {
        if staged.is_file() && staged.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            match promote_file(&staged, &final_path) {
                Ok(()) => {
                    summary
                        .log
                        .push(format!("   -> Chapters: {}", final_path.display()));
                    summary.extracted_chapters = Some(final_path);
                }
                Err(e) => {
                    summary.log.push(format!(
                        "   -> WARNING: Failed to promote chapters to '{}': {e}",
                        final_path.display()
                    ));
                }
            }
        }
    }

    if let Some((staged, final_path)) = staged_cover {
        if staged.is_file() && staged.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            match promote_file(&staged, &final_path) {
                Ok(()) => {
                    summary
                        .log
                        .push(format!("   -> Cover: {}", final_path.display()));
                    summary.extracted_cover = Some(final_path);
                }
                Err(e) => {
                    summary.log.push(format!(
                        "   -> WARNING: Failed to promote cover to '{}': {e}",
                        final_path.display()
                    ));
                }
            }
        }
    }

    for (_, mut claim) in font_claims {
        if font_manager.promote(&mut claim).is_some() {
            summary.extracted_fonts += 1;
        }
    }
    if summary.extracted_fonts > 0 {
        summary.log.push(format!(
            "   -> Extracted {} new font(s).",
            summary.extracted_fonts
        ));
    }

    let _ = std::fs::remove_dir(font_manager.fonts_dir.join(".font_staging"));

    Ok(summary)
}
