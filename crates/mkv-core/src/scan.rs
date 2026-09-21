//! Media asset scanning, MKV chapter/cover classification, typed metadata inspection,
//! and persistent inspection caching.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use parking_lot::Mutex;
use regex::bytes::Regex as BytesRegex;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::warn;

use crate::domain::{
    natural_sort_key, parse_episode, AudioTrack, EpisodeInfo, MkvTrack, COVER_EXTENSIONS,
    FONT_EXTENSIONS,
};
use crate::fonts::FONT_DIR_NAMES;

// ─── MKVMERGE METADATA TYPES ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MkvAttachment {
    #[serde(default)]
    pub id: Option<u32>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MkvChapter {
    #[serde(default)]
    pub num_entries: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MkvmergeMetadata {
    #[serde(default)]
    pub tracks: Vec<MkvTrack>,
    #[serde(default)]
    pub attachments: Vec<MkvAttachment>,
    #[serde(default)]
    pub chapters: Vec<MkvChapter>,
}

/// Probes MKV metadata using `mkvmerge -J` subprocess.
pub async fn probe_video_mkvmerge(path: &Path) -> Result<MkvmergeMetadata, String> {
    let mut cmd = tokio::process::Command::new("mkvmerge");
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    cmd.kill_on_drop(true);
    let output = cmd
        .args(["-J", &path.to_string_lossy()])
        .output()
        .await
        .map_err(|e| format!("Failed to execute mkvmerge: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "mkvmerge exit code {:?}: {stderr}",
            output.status.code()
        ));
    }

    let val: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse mkvmerge JSON: {e}"))?;

    if let Some(false) = val
        .pointer("/container/recognized")
        .and_then(|v| v.as_bool())
    {
        return Err("File is not a recognized Matroska container".to_string());
    }

    serde_json::from_value(val).map_err(|e| format!("Failed to parse mkvmerge metadata: {e}"))
}

/// Probes MKV container headers, tracks, attachments, and chapters using hybrid native EBML
/// parsing with automatic fallback to `mkvmerge -J` (ADR 0006).
pub async fn probe_video(path: &Path) -> Result<MkvmergeMetadata, String> {
    // 1. Fast path: native EBML probe directly in pure Rust (< 1ms)
    if let Ok(meta) = crate::ebml::probe_ebml_native(path) {
        return Ok(meta);
    }
    // 2. Safe fallback: shell out to mkvmerge -J for non-standard containers or quirks (ADR 0006)
    probe_video_mkvmerge(path).await
}

/// Looks up cached inspection metadata or probes the video with `mkvmerge`, caching on success.
pub async fn get_or_probe_video(
    path: &Path,
    cache: Option<&PersistentInspectCache>,
) -> Result<MkvmergeMetadata, String> {
    if let Some(cached) = cache.and_then(|c| c.get_path(path)) {
        return Ok(cached);
    }
    let probed = probe_video(path).await?;
    if let Some(c) = cache {
        c.put_path(path, probed.clone());
    }
    Ok(probed)
}

// ─── ATTACHMENT & CHAPTER CLASSIFICATION ──────────────────────────────────────

static CHAPTER_TAG_RE: LazyLock<BytesRegex> =
    LazyLock::new(|| BytesRegex::new(r"(?i)<Chapters[ >]").unwrap());

pub static COVER_LIKE_STEMS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "cover",
        "cover_land",
        "small_cover",
        "small_cover_land",
        "poster",
        "thumbnail",
        "thumb",
        "fanart",
    ]
    .into_iter()
    .collect()
});

/// Sniff whether a file is MKV chapter XML by reading up to 4096 bytes and checking
/// case-insensitively for `<Chapters[ >]`.
pub fn is_chapter_xml(path: &Path) -> bool {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 4096];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    if n == 0 {
        return false;
    }
    CHAPTER_TAG_RE.is_match(&buf[..n])
}

fn path_ext_lower(path: &Path) -> String {
    path.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn path_stem_lower(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Returns true if the file stem is "cover" (case-insensitive) and the extension
/// is in `COVER_EXTENSIONS`.
pub fn is_cover_filename(file_name: &str) -> bool {
    let p = Path::new(file_name);
    let ext = path_ext_lower(p);
    if !COVER_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }
    path_stem_lower(p) == "cover"
}

/// Returns true if the attachment filename or content-type indicates a cover image.
pub fn is_cover_attachment(file_name: &str, content_type: &str) -> bool {
    if is_cover_filename(file_name) {
        return true;
    }
    if !content_type.to_lowercase().starts_with("image/") {
        return false;
    }
    let stem = path_stem_lower(Path::new(file_name));
    COVER_LIKE_STEMS.contains(stem.as_str())
}

/// Returns true if the attachment filename extension or content-type indicates a font.
pub fn is_font_attachment(file_name: &str, content_type: &str) -> bool {
    let ext = path_ext_lower(Path::new(file_name));
    if FONT_EXTENSIONS.contains(&ext.as_str()) {
        return true;
    }
    let ct_lower = content_type.to_lowercase();
    ct_lower.contains("font") || ct_lower.contains("truetype") || ct_lower.contains("opentype")
}

/// Parse attachments once, returning the first cover attachment and all font attachments.
pub fn parse_attachments(
    attachments: &[MkvAttachment],
) -> (Option<MkvAttachment>, Vec<MkvAttachment>) {
    let mut cover = None;
    let mut fonts = Vec::new();
    for att in attachments {
        let file_name = match &att.file_name {
            Some(f) => f,
            None => continue,
        };
        let content_type = att.content_type.as_deref().unwrap_or("");
        if cover.is_none() && is_cover_attachment(file_name, content_type) {
            cover = Some(att.clone());
        }
        if is_font_attachment(file_name, content_type) {
            fonts.push(att.clone());
        }
    }
    (cover, fonts)
}

// ─── INSPECTED VIDEO ─────────────────────────────────────────────────────────

/// Typed wrapper around raw `mkvmerge -J` metadata providing normalized schema access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InspectedVideo {
    data: MkvmergeMetadata,
}

impl InspectedVideo {
    pub fn new(data: MkvmergeMetadata) -> Self {
        Self { data }
    }

    pub fn metadata(&self) -> &MkvmergeMetadata {
        &self.data
    }

    pub fn into_metadata(self) -> MkvmergeMetadata {
        self.data
    }

    pub fn tracks(&self) -> &[MkvTrack] {
        &self.data.tracks
    }

    pub fn attachments(&self) -> &[MkvAttachment] {
        &self.data.attachments
    }

    pub fn video_track(&self) -> Option<&MkvTrack> {
        self.data.tracks.iter().find(|t| t.track_type == "video")
    }

    pub fn video_track_pos(&self) -> i32 {
        self.data
            .tracks
            .iter()
            .position(|t| t.track_type == "video")
            .map(|pos| pos as i32)
            .unwrap_or(-1)
    }

    pub fn audio_tracks(&self) -> Vec<AudioTrack> {
        let mut normalized = Vec::new();
        for (position, track) in self.data.tracks.iter().enumerate() {
            if track.track_type != "audio" {
                continue;
            }
            let id = track.id.unwrap_or(0);
            let pos = position as u32;
            let audio_idx = normalized.len();
            let lang = track.properties.language.clone().unwrap_or_default();
            let name = track.properties.track_name.clone().unwrap_or_default();
            let channels = track.properties.audio_channels.unwrap_or(0);
            let codec = track
                .properties
                .codec_id
                .as_deref()
                .unwrap_or("")
                .to_uppercase();

            normalized.push(AudioTrack {
                id,
                pos,
                audio_idx,
                lang,
                channels,
                name,
                codec,
            });
        }
        normalized
    }

    pub fn subtitle_tracks(&self) -> Vec<MkvTrack> {
        self.data
            .tracks
            .iter()
            .filter(|t| t.track_type == "subtitles")
            .cloned()
            .collect()
    }

    pub fn has_chapters(&self) -> bool {
        self.data
            .chapters
            .iter()
            .any(|c| c.num_entries.unwrap_or(0) > 0)
    }

    pub fn cover_attachment(&self) -> Option<MkvAttachment> {
        let (cover, _) = parse_attachments(&self.data.attachments);
        cover
    }

    pub fn font_attachments(&self) -> Vec<MkvAttachment> {
        let (_, fonts) = parse_attachments(&self.data.attachments);
        fonts
    }

    pub fn fonts_count(&self) -> Option<usize> {
        Some(self.font_attachments().len())
    }
}

impl From<MkvmergeMetadata> for InspectedVideo {
    fn from(data: MkvmergeMetadata) -> Self {
        Self::new(data)
    }
}

// ─── INSPECTION RESULT ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectionResult {
    pub video: Option<InspectedVideo>,
    pub error: Option<String>,
}

impl InspectionResult {
    pub fn success(data: MkvmergeMetadata) -> Self {
        Self {
            video: Some(InspectedVideo::new(data)),
            error: None,
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            video: None,
            error: Some(error.into()),
        }
    }

    pub fn is_success(&self) -> bool {
        self.video.is_some()
    }
}

// ─── PARSED FILE & ASSET SCANNING ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedFile {
    pub path: PathBuf,
    pub name: String,
    pub stem: String,
    pub parsed: Option<EpisodeInfo>,
}

pub fn create_parsed_file(path: impl Into<PathBuf>) -> ParsedFile {
    let path = path.into();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed = parse_episode(&stem);
    ParsedFile {
        path,
        name,
        stem,
        parsed,
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScanResults {
    pub videos: Vec<ParsedFile>,
    pub subs: Vec<ParsedFile>,
    pub chapters: Vec<ParsedFile>,
    pub covers: Vec<ParsedFile>,
}

/// Scans directory entries, grouping files into videos (.mkv), subs (.ass, .srt),
/// chapters (.xml verified via `is_chapter_xml`), and covers (cover stem or ending in `_cover`).
pub fn scan_assets(
    dir: &Path,
    mut cache: Option<&mut HashMap<PathBuf, ScanResults>>,
) -> ScanResults {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if let Some(c) = &cache {
        if let Some(hit) = c.get(&canonical) {
            return hit.clone();
        }
    }

    let mut videos = Vec::new();
    let mut subs = Vec::new();
    let mut chapters = Vec::new();
    let mut covers = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.file_name().and_then(|s| s.to_str()).is_none() {
                continue;
            }
            let ext = path_ext_lower(&path);

            if ext == ".mkv" {
                videos.push(create_parsed_file(&path));
            } else if ext == ".ass" || ext == ".srt" {
                subs.push(create_parsed_file(&path));
            } else if ext == ".xml" && is_chapter_xml(&path) {
                chapters.push(create_parsed_file(&path));
            } else if COVER_EXTENSIONS.contains(&ext.as_str()) {
                let stem = path_stem_lower(&path);
                if stem == "cover" || stem.ends_with("_cover") {
                    covers.push(create_parsed_file(&path));
                }
            }
        }
    }

    videos.sort_by_cached_key(|a| natural_sort_key(&a.name));
    subs.sort_by_cached_key(|a| natural_sort_key(&a.name));
    chapters.sort_by_cached_key(|a| natural_sort_key(&a.name));
    covers.sort_by_cached_key(|a| natural_sort_key(&a.name));

    let result = ScanResults {
        videos,
        subs,
        chapters,
        covers,
    };

    if let Some(c) = &mut cache {
        c.insert(canonical, result.clone());
    }

    result
}

/// Recursively scans for `.mkv` files, naturally sorted by path/name, respecting ignore directories.
pub fn scan_mkv_files(
    dir_path: &Path,
    recursive: bool,
    ignore_paths: Option<&HashSet<PathBuf>>,
) -> Vec<ParsedFile> {
    let mut results = Vec::new();
    let mut stack = vec![dir_path.to_path_buf()];

    let resolved_ignores: HashSet<PathBuf> = ignore_paths
        .map(|set| {
            set.iter()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .collect()
        })
        .unwrap_or_default();

    while let Some(current_dir) = stack.pop() {
        let entries = match std::fs::read_dir(&current_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if file_type.is_file() {
                if name.to_lowercase().ends_with(".mkv") {
                    results.push(create_parsed_file(&path));
                }
            } else if file_type.is_dir() && recursive {
                let name_lower = name.to_lowercase();
                if name_lower == "output" || FONT_DIR_NAMES.contains(&name_lower.as_str()) {
                    continue;
                }
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                if resolved_ignores.contains(&canon) {
                    continue;
                }
                stack.push(path);
            }
        }
    }

    results.sort_by_cached_key(|a| natural_sort_key(&a.path.to_string_lossy()));
    results
}

// ─── PERSISTENT INSPECT CACHE ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InspectCacheEntry {
    pub size: u64,
    #[serde(default)]
    pub mtime_ns: u128,
    #[serde(default)]
    pub mtime: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_data: Option<MkvmergeMetadata>,
}

#[derive(Debug, Default)]
struct CacheState {
    data: Arc<HashMap<String, InspectCacheEntry>>,
    dirty: bool,
    revision: u64,
    last_save_ts: Option<Instant>,
    resolved_keys: HashMap<String, String>,
}

pub fn get_default_cache_dir() -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        if !local_app_data.is_empty() {
            return PathBuf::from(local_app_data).join("mkvanimebatch");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".cache").join("mkvanimebatch");
        }
    }
    PathBuf::from(".cache").join("mkvanimebatch")
}

pub fn get_default_cache_file() -> PathBuf {
    get_default_cache_dir().join("inspect_cache.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileSignature {
    pub size: u64,
    pub mtime_ns: u128,
}

impl FileSignature {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = path.metadata().ok()?;
        let size = meta.len();
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Some(Self { size, mtime_ns })
    }
}

/// Thread-safe persistent JSON cache for mkvmerge inspection metadata keyed on (path, size, mtime).
#[derive(Debug)]
pub struct PersistentInspectCache {
    pub cache_file: PathBuf,
    pub enabled: bool,
    state: Mutex<CacheState>,
}

impl PersistentInspectCache {
    pub fn new(cache_file: Option<PathBuf>, enabled: bool) -> Self {
        let cache_file = cache_file.unwrap_or_else(get_default_cache_file);
        let cache = Self {
            cache_file,
            enabled,
            state: Mutex::new(CacheState::default()),
        };
        if enabled {
            cache.load();
        }
        cache
    }

    fn cache_key(&self, path: &Path) -> String {
        let raw = path.to_string_lossy().into_owned();
        {
            let state = self.state.lock();
            if let Some(k) = state.resolved_keys.get(&raw) {
                return k.clone();
            }
        }
        #[cfg(windows)]
        let mut resolved = path
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| raw.clone());
        #[cfg(windows)]
        {
            if let Some(stripped) = resolved.strip_prefix(r"\\?\") {
                resolved = stripped.to_string();
            }
        }
        #[cfg(not(windows))]
        let resolved = path
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| raw.clone());
        let mut state = self.state.lock();
        state.resolved_keys.insert(raw, resolved.clone());
        resolved
    }

    fn load(&self) {
        if !self.cache_file.exists() {
            return;
        }
        match std::fs::read_to_string(&self.cache_file) {
            Ok(content) => {
                match serde_json::from_str::<HashMap<String, InspectCacheEntry>>(&content) {
                    Ok(loaded) => {
                        let mut state = self.state.lock();
                        state.data = Arc::new(loaded);
                        state.dirty = false;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to parse inspect cache from {}: {}",
                            self.cache_file.display(),
                            e
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to read inspect cache from {}: {}",
                    self.cache_file.display(),
                    e
                );
            }
        }
    }

    pub fn get(&self, path: &Path, sig: FileSignature) -> Option<MkvmergeMetadata> {
        if !self.enabled {
            return None;
        }
        let key = self.cache_key(path);
        let state = self.state.lock();
        let entry = state.data.get(&key)?;
        if entry.size == sig.size && entry.mtime_ns == sig.mtime_ns {
            entry.json_data.clone()
        } else {
            None
        }
    }

    pub fn get_path(&self, path: &Path) -> Option<MkvmergeMetadata> {
        let sig = FileSignature::of(path)?;
        self.get(path, sig)
    }

    pub fn put(&self, path: &Path, sig: FileSignature, data: MkvmergeMetadata) {
        if !self.enabled {
            return;
        }
        let key = self.cache_key(path);
        let entry = InspectCacheEntry {
            size: sig.size,
            mtime_ns: sig.mtime_ns,
            mtime: Some(sig.mtime_ns as f64 / 1_000_000_000.0),
            json_data: Some(data),
        };
        let mut state = self.state.lock();
        if state.data.get(&key) != Some(&entry) {
            let data = Arc::make_mut(&mut state.data);
            data.insert(key, entry);
            state.dirty = true;
            state.revision += 1;
        }
    }

    pub fn put_path(&self, path: &Path, data: MkvmergeMetadata) {
        if let Some(sig) = FileSignature::of(path) {
            self.put(path, sig, data);
        }
    }

    pub fn save(&self) {
        self.save_internal(std::time::Duration::ZERO);
    }

    pub fn save_shared(&self) {
        self.save_internal(std::time::Duration::from_millis(500));
    }

    fn save_internal(&self, min_interval: std::time::Duration) {
        if !self.enabled {
            return;
        }

        let (data_snapshot, saved_revision) = {
            let state = self.state.lock();
            if !state.dirty {
                return;
            }
            if !min_interval.is_zero() {
                if let Some(last) = state.last_save_ts {
                    if last.elapsed() < min_interval {
                        return;
                    }
                }
            }
            (Arc::clone(&state.data), state.revision)
        };

        let parent = match self.cache_file.parent() {
            Some(p) => {
                let _ = std::fs::create_dir_all(p);
                p
            }
            None => Path::new("."),
        };

        match NamedTempFile::new_in(parent) {
            Ok(mut temp_file) => {
                let mut writer = std::io::BufWriter::new(&mut temp_file);
                if serde_json::to_writer(&mut writer, &*data_snapshot).is_ok()
                    && writer.flush().is_ok()
                {
                    drop(writer);
                    if let Ok(_persisted) = temp_file.persist(&self.cache_file) {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ =
                                _persisted.set_permissions(std::fs::Permissions::from_mode(0o644));
                        }
                        let mut state = self.state.lock();
                        if state.revision == saved_revision {
                            state.dirty = false;
                        }
                        state.last_save_ts = Some(Instant::now());
                    } else {
                        warn!(
                            "Failed to atomically replace inspect cache at {}",
                            self.cache_file.display()
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to create tempfile for inspect cache save in {}: {}",
                    parent.display(),
                    e
                );
            }
        }
    }

    pub fn clear(&self) {
        let mut state = self.state.lock();
        state.data = Arc::default();
        state.dirty = false;
        state.revision = 0;
        state.last_save_ts = None;
        if self.cache_file.exists() {
            let _ = std::fs::remove_file(&self.cache_file);
        }
    }
}

impl Default for PersistentInspectCache {
    fn default() -> Self {
        Self::new(None, true)
    }
}
