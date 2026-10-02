//! Media asset scanning, MKV chapter/cover classification, typed metadata inspection,
//! and persistent inspection caching.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

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

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Probes MKV metadata using `mkvmerge -J` subprocess.
pub async fn probe_video_mkvmerge(path: &Path) -> Result<MkvmergeMetadata, String> {
    let mut cmd = tokio::process::Command::new("mkvmerge");
    #[cfg(windows)]
    cmd.creation_flags(crate::domain::CREATE_NO_WINDOW);
    cmd.kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_TIMEOUT, cmd.arg("-J").arg(path).output())
        .await
        .map_err(|_| {
            format!(
                "mkvmerge probe timed out after {} seconds",
                PROBE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("Failed to execute mkvmerge: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "mkvmerge exit code {:?}: {}",
            output.status.code(),
            stderr.trim()
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

/// Probes MKV container headers, tracks, attachments, and chapters with the native EBML
/// parser, falling back to `mkvmerge -J` when native parsing fails (ADR 0006).
///
/// The native probe runs on the blocking pool so a cold or network read cannot stall a reactor
/// worker (ADR 0013 Amendment 7). Once started it is not cancellable and reads to completion.
pub async fn probe_video(path: &Path) -> Result<MkvmergeMetadata, String> {
    let native_path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || crate::ebml::probe_ebml_native(&native_path)).await {
        Ok(Ok(meta)) => return Ok(meta),
        Ok(Err(_)) => {}
        Err(join_err) => warn!(
            "Native probe task failed for {}: {join_err}",
            path.display()
        ),
    }
    probe_video_mkvmerge(path).await
}

/// Looks up cached inspection metadata or probes the video with [`probe_video`], caching on
/// success.
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

/// Leading bytes of a file inspected by [`is_chapter_xml`].
const CHAPTER_SNIFF_BYTES: usize = 4096;

const COVER_LIKE_STEMS: &[&str] = &[
    "cover",
    "cover_land",
    "small_cover",
    "small_cover_land",
    "poster",
    "thumbnail",
    "thumb",
    "fanart",
];

/// Sniff whether a file is MKV chapter XML by reading up to 4096 bytes and checking
/// case-insensitively for `<Chapters[ >]`.
pub fn is_chapter_xml(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut buf = [0u8; CHAPTER_SNIFF_BYTES];
    file.read(&mut buf)
        .is_ok_and(|n| CHAPTER_TAG_RE.is_match(&buf[..n]))
}

/// Lowercased extension with its leading dot (`".mkv"`), or `""` when there is none.
pub(crate) fn path_ext_lower(path: &Path) -> String {
    path.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn path_stem_lower(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The canonical spelling of `path`, or `path` itself when it cannot be resolved.
pub(crate) fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Returns true if the file stem is "cover" or "folder" (case-insensitive) and the
/// extension is in `COVER_EXTENSIONS`.
pub fn is_cover_filename(file_name: &str) -> bool {
    let p = Path::new(file_name);
    let ext = path_ext_lower(p);
    if !COVER_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }
    matches!(path_stem_lower(p).as_str(), "cover" | "folder")
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
    COVER_LIKE_STEMS.contains(&stem.as_str())
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

/// `(file_name, content_type)` of an attachment; attachments without a file name are never
/// classified.
fn attachment_names(att: &MkvAttachment) -> Option<(&str, &str)> {
    Some((
        att.file_name.as_deref()?,
        att.content_type.as_deref().unwrap_or(""),
    ))
}

// ─── INSPECTED VIDEO ─────────────────────────────────────────────────────────

/// Typed wrapper around raw `mkvmerge -J` metadata providing normalized schema access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedVideo {
    data: MkvmergeMetadata,
}

impl InspectedVideo {
    pub fn new(data: MkvmergeMetadata) -> Self {
        Self { data }
    }

    pub fn tracks(&self) -> &[MkvTrack] {
        &self.data.tracks
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

    /// The first attachment classified as cover art.
    pub fn cover_attachment(&self) -> Option<&MkvAttachment> {
        self.data.attachments.iter().find(|att| {
            attachment_names(att).is_some_and(|(name, ct)| is_cover_attachment(name, ct))
        })
    }

    pub fn font_attachments(&self) -> Vec<&MkvAttachment> {
        self.data
            .attachments
            .iter()
            .filter(|att| {
                attachment_names(att).is_some_and(|(name, ct)| is_font_attachment(name, ct))
            })
            .collect()
    }
}

// ─── INSPECTION RESULT ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
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
/// chapters (.xml verified via `is_chapter_xml`), and applicable cover images.
pub fn scan_assets(
    dir: &Path,
    mut cache: Option<&mut HashMap<PathBuf, ScanResults>>,
) -> ScanResults {
    let canonical = canonical_or_self(dir);
    if let Some(c) = &cache {
        if let Some(hit) = c.get(&canonical) {
            return hit.clone();
        }
    }

    let mut videos = Vec::new();
    let mut subs = Vec::new();
    let mut chapters = Vec::new();
    let mut covers = Vec::new();
    let mut cover_candidates = Vec::new();

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
                cover_candidates.push(path);
            }
        }
    }
    let video_stems: HashSet<&str> = videos.iter().map(|v| v.stem.as_str()).collect();
    for path in cover_candidates {
        let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let lower = stem.to_lowercase();
        if video_stems.contains(stem)
            || matches!(lower.as_str(), "cover" | "folder")
            || lower
                .strip_suffix("_cover")
                .or_else(|| lower.strip_suffix(".cover"))
                .is_some_and(|episode| parse_episode(episode).is_some())
        {
            covers.push(create_parsed_file(path));
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
    // Reported paths keep the spelling of `dir_path`, so they compare equal to the asset
    // directories derived from the same input. Dedup keys and `ignore_paths` matching use
    // canonical paths instead: each stack entry carries its directory's canonical spelling when
    // it is known, so entries beneath it need no `realpath`. Anything reached through a
    // symlink, or beneath a directory that failed to resolve, is resolved per entry.
    let root_canonical = dir_path.canonicalize().ok();
    let mut visited_dirs = HashSet::new();
    visited_dirs.insert(
        root_canonical
            .clone()
            .unwrap_or_else(|| dir_path.to_path_buf()),
    );
    let mut stack = vec![(dir_path.to_path_buf(), root_canonical)];
    let mut visited_files = HashSet::new();
    let resolved_ignores: HashSet<PathBuf> = ignore_paths
        .map(|set| set.iter().map(|p| canonical_or_self(p)).collect())
        .unwrap_or_default();
    let is_ignored = |p: &Path| -> bool { resolved_ignores.iter().any(|ig| p.starts_with(ig)) };

    while let Some((current_dir, current_canonical)) = stack.pop() {
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
                Some(n) => n,
                None => continue,
            };

            // A symlink names its target through a second spelling, so its canonical path is
            // never the parent's canonical path joined with its name.
            let derived_canonical = || {
                current_canonical
                    .as_ref()
                    .filter(|_| !file_type.is_symlink())
                    .map(|dir| dir.join(name))
            };

            let is_file = file_type.is_file() || (file_type.is_symlink() && path.is_file());
            let is_dir = file_type.is_dir() || (file_type.is_symlink() && path.is_dir());

            if is_file {
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("mkv"))
                {
                    let key = derived_canonical().unwrap_or_else(|| canonical_or_self(&path));
                    if !is_ignored(&key) && visited_files.insert(key) {
                        results.push(create_parsed_file(&path));
                    }
                }
            } else if is_dir && recursive {
                if name.eq_ignore_ascii_case("output")
                    || FONT_DIR_NAMES.iter().any(|d| name.eq_ignore_ascii_case(d))
                {
                    continue;
                }
                let canonical = derived_canonical().or_else(|| path.canonicalize().ok());
                let key = canonical.clone().unwrap_or_else(|| path.clone());
                if is_ignored(&key) || !visited_dirs.insert(key) {
                    continue;
                }
                stack.push((path, canonical));
            }
        }
    }

    results.sort_by_cached_key(|a| natural_sort_key(&a.path.to_string_lossy()));
    results
}

// ─── PERSISTENT INSPECT CACHE ────────────────────────────────────────────────

/// On-disk cache record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct InspectCacheEntry {
    size: u64,
    #[serde(default)]
    mtime_ns: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    json_data: Option<MkvmergeMetadata>,
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

fn get_default_cache_file() -> PathBuf {
    get_default_cache_dir().join("inspect_cache.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSignature {
    pub size: u64,
    pub mtime_ns: u128,
}

impl FileSignature {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = path.metadata().ok()?;
        Some(Self::from_metadata(&meta))
    }

    /// Signature from already-fetched metadata. It matches [`FileSignature::of`] only when
    /// `meta` was obtained with symlink resolution; for `DirEntry` metadata use
    /// [`FileSignature::of_dir_entry`].
    pub fn from_metadata(meta: &std::fs::Metadata) -> Self {
        let size = meta.len();
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self { size, mtime_ns }
    }

    /// Signature for a directory entry, or `None` when the entry does not resolve to a
    /// regular file.
    ///
    /// `meta` must be the value `entry.metadata()` returned; it is taken as an argument so a
    /// caller that already read it pays no second `lstat`. A symlinked entry is re-resolved
    /// because `meta` describes the link itself. Only regular files are signed: a directory,
    /// FIFO, socket, or device reports a `len()` unrelated to content, and opening a FIFO
    /// blocks.
    ///
    /// The symlink test reads `meta` rather than `entry.file_type()`, which on Unix costs
    /// its own `lstat` on filesystems that report `DT_UNKNOWN`.
    pub fn of_dir_entry(entry: &std::fs::DirEntry, meta: &std::fs::Metadata) -> Option<Self> {
        if meta.is_symlink() {
            let resolved = entry.path().metadata().ok()?;
            resolved.is_file().then(|| Self::from_metadata(&resolved))
        } else {
            meta.is_file().then(|| Self::from_metadata(meta))
        }
    }
}

/// Thread-safe persistent JSON cache for mkvmerge inspection metadata keyed on (path, size, mtime).
#[derive(Debug)]
pub struct PersistentInspectCache {
    cache_file: PathBuf,
    enabled: bool,
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
        if let Some(k) = self.state.lock().resolved_keys.get(&raw) {
            return k.clone();
        }
        let resolved = canonical_or_self(path).to_string_lossy().into_owned();
        #[cfg(windows)]
        let resolved = match resolved.strip_prefix(r"\\?\") {
            Some(stripped) => stripped.to_string(),
            None => resolved,
        };
        self.state
            .lock()
            .resolved_keys
            .insert(raw, resolved.clone());
        resolved
    }

    fn load(&self) {
        let Ok(file) = File::open(&self.cache_file) else {
            return;
        };
        let reader = std::io::BufReader::new(file);
        match serde_json::from_reader::<_, HashMap<String, InspectCacheEntry>>(reader) {
            Ok(loaded) => {
                let mut state = self.state.lock();
                state.data = Arc::new(loaded);
                state.dirty = false;
            }
            Err(e) => {
                warn!(
                    "Failed to parse inspect cache from {}: {e}",
                    self.cache_file.display()
                );
            }
        }
    }

    /// Cached metadata for `path`, if its current size and mtime match the cached entry.
    pub fn get_path(&self, path: &Path) -> Option<MkvmergeMetadata> {
        if !self.enabled {
            return None;
        }
        let sig = FileSignature::of(path)?;
        let key = self.cache_key(path);
        let state = self.state.lock();
        let entry = state.data.get(&key)?;
        if entry.size == sig.size && entry.mtime_ns == sig.mtime_ns {
            entry.json_data.clone()
        } else {
            None
        }
    }

    /// Records `data` for `path` under its current size and mtime.
    pub fn put_path(&self, path: &Path, data: MkvmergeMetadata) {
        if !self.enabled {
            return;
        }
        let Some(sig) = FileSignature::of(path) else {
            return;
        };
        let key = self.cache_key(path);
        let entry = InspectCacheEntry {
            size: sig.size,
            mtime_ns: sig.mtime_ns,
            json_data: Some(data),
        };
        let mut state = self.state.lock();
        if state.data.get(&key) != Some(&entry) {
            Arc::make_mut(&mut state.data).insert(key, entry);
            state.dirty = true;
            state.revision += 1;
        }
    }

    /// Writes pending changes to disk immediately.
    pub fn save(&self) {
        self.save_internal(Duration::ZERO);
    }

    /// Writes pending changes unless the previous write was under 500 ms ago.
    pub fn save_shared(&self) {
        self.save_internal(Duration::from_millis(500));
    }

    fn save_internal(&self, min_interval: Duration) {
        if !self.enabled {
            return;
        }

        let (data_snapshot, saved_revision) = {
            let state = self.state.lock();
            if !state.dirty {
                return;
            }
            if !min_interval.is_zero()
                && state
                    .last_save_ts
                    .is_some_and(|last| last.elapsed() < min_interval)
            {
                return;
            }
            (Arc::clone(&state.data), state.revision)
        };

        let parent = match self.cache_file.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                let _ = std::fs::create_dir_all(p);
                p
            }
            _ => Path::new("."),
        };

        let mut temp_file = match NamedTempFile::new_in(parent) {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    "Failed to create tempfile for inspect cache save in {}: {e}",
                    parent.display()
                );
                return;
            }
        };
        let mut writer = std::io::BufWriter::new(&mut temp_file);
        if let Err(e) = serde_json::to_writer(&mut writer, &*data_snapshot) {
            warn!(
                "Failed to stream serialize inspect cache to {}: {e}",
                parent.display()
            );
            return;
        }
        if let Err(e) = writer.flush() {
            warn!(
                "Failed to flush inspect cache stream in {}: {e}",
                parent.display()
            );
            return;
        }
        drop(writer);
        let persisted = match temp_file.persist(&self.cache_file) {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    "Failed to atomically replace inspect cache at {}: {e}",
                    self.cache_file.display()
                );
                return;
            }
        };
        // `NamedTempFile` creates files as 0600; the cache is an ordinary user file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = persisted.set_permissions(std::fs::Permissions::from_mode(0o644));
        }
        #[cfg(not(unix))]
        drop(persisted);

        let mut state = self.state.lock();
        if state.revision == saved_revision {
            state.dirty = false;
        }
        state.last_save_ts = Some(Instant::now());
    }

    pub fn clear(&self) {
        let mut state = self.state.lock();
        state.data = Arc::default();
        state.dirty = false;
        state.revision = 0;
        state.last_save_ts = None;
        let _ = std::fs::remove_file(&self.cache_file);
    }
}

impl Default for PersistentInspectCache {
    fn default() -> Self {
        Self::new(None, true)
    }
}
