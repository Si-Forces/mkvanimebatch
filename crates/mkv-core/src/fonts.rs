//! Font discovery, deduplication, and transactional promotion.
//!
//! Lifecycle: claim -> stage -> deduplicate -> promote (or release).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blake2::{Blake2b512, Digest};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::{error, warn};

use crate::domain::{font_mime, natural_sort_key, safe_filename, FONT_EXTENSIONS};
use crate::scan::FileSignature;

pub const FONT_DIR_NAMES: &[&str] = &["fonts", "font", "attachments"];

/// Calculate BLAKE2b-512 hash of a file for content-based deduplication.
pub fn hash_file(filepath: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(filepath)?;
    let mut hasher = Blake2b512::new();
    let mut buffer = [0u8; 65536];

    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let result = hasher.finalize();
    Ok(format!("{result:x}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FontInfo {
    pub args: Vec<String>,
    pub count: usize,
}

/// Build attachment arguments for all fonts found in `fonts_dir`.
pub fn scan_fonts(
    fonts_dir: &Path,
    mut font_cache: Option<&mut HashMap<PathBuf, FontInfo>>,
) -> FontInfo {
    if let Some(cache) = &font_cache {
        if let Some(info) = cache.get(fonts_dir) {
            return info.clone();
        }
    }

    let mut font_args = Vec::new();
    let mut count = 0;

    // Intentionally do not cache negative (non-existent) results: if extraction creates
    // fonts_dir later in the run, subsequent scans must discover it.
    if !fonts_dir.is_dir() {
        return FontInfo {
            args: Vec::new(),
            count: 0,
        };
    }

    let Ok(entries) = fs::read_dir(fonts_dir) else {
        return FontInfo {
            args: font_args,
            count,
        };
    };

    let mut valid_entries: Vec<(PathBuf, String, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            if let Some(mime) = font_mime(&ext) {
                let file_name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                valid_entries.push((path, file_name, mime.to_string()));
            }
        }
    }

    valid_entries.sort_by_key(|a| natural_sort_key(&a.1));

    for (path, _, mime) in valid_entries {
        font_args.push("--attachment-mime-type".to_string());
        font_args.push(mime);
        font_args.push("--attach-file".to_string());
        font_args.push(path.to_string_lossy().into_owned());
        count += 1;
    }

    let info = FontInfo {
        args: font_args,
        count,
    };
    if let Some(cache) = &mut font_cache {
        cache.insert(fonts_dir.to_path_buf(), info.clone());
    }
    info
}

/// Resolve which directory to use for fonts.
/// Priority:
///   1. override_dir (e.g. from --fonts-dir)
///   2. input_dir / "fonts"
///   3. Any top-level directory matching fonts/font/attachments
///   4. Falls back to input_dir / "fonts"
pub fn find_fonts_dir(
    input_dir: &Path,
    override_dir: Option<&Path>,
    mut fonts_dir_cache: Option<&mut HashMap<PathBuf, PathBuf>>,
) -> PathBuf {
    if let Some(over) = override_dir {
        return over.to_path_buf();
    }

    if let Some(cache) = &fonts_dir_cache {
        if let Some(cached) = cache.get(input_dir) {
            return cached.clone();
        }
    }

    let exact = input_dir.join("fonts");
    if exact.is_dir() {
        if let Some(cache) = &mut fonts_dir_cache {
            cache.insert(input_dir.to_path_buf(), exact.clone());
        }
        return exact;
    }

    if input_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(input_dir) {
            let mut dirs: Vec<PathBuf> = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if FONT_DIR_NAMES.contains(&name.as_str()) {
                        dirs.push(path);
                    }
                }
            }
            dirs.sort();
            if let Some(first) = dirs.into_iter().next() {
                if let Some(cache) = &mut fonts_dir_cache {
                    cache.insert(input_dir.to_path_buf(), first.clone());
                }
                return first;
            }
        }
    }

    if let Some(cache) = &mut fonts_dir_cache {
        cache.insert(input_dir.to_path_buf(), exact.clone());
    }
    exact
}

// ─── FONT HASH MEMO ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FontMemoEntry {
    pub size: u64,
    pub mtime_ns: u128,
    pub hash: String,
}

#[derive(Debug)]
pub struct FontHashMemo {
    pub memo_file: PathBuf,
    data: HashMap<String, FontMemoEntry>,
    dirty: bool,
}

impl FontHashMemo {
    pub fn new(memo_file: PathBuf) -> Self {
        let mut memo = Self {
            memo_file,
            data: HashMap::new(),
            dirty: false,
        };
        memo.load();
        memo
    }

    fn load(&mut self) {
        if !self.memo_file.exists() {
            return;
        }
        match fs::read_to_string(&self.memo_file) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(loaded) => self.data = loaded,
                Err(e) => {
                    warn!(
                        "Failed to parse font hash memo from {}: {}",
                        self.memo_file.display(),
                        e
                    );
                    self.data.clear();
                }
            },
            Err(e) => {
                warn!(
                    "Failed to read font hash memo from {}: {}",
                    self.memo_file.display(),
                    e
                );
                self.data.clear();
            }
        }
    }

    pub fn get(&self, path: &Path, sig: FileSignature) -> Option<String> {
        let key = path.to_string_lossy();
        if let Some(entry) = self.data.get(key.as_ref()) {
            if entry.size == sig.size && entry.mtime_ns == sig.mtime_ns {
                return Some(entry.hash.clone());
            }
        }
        None
    }

    pub fn put(&mut self, path: &Path, sig: FileSignature, hash: String) {
        let entry = FontMemoEntry {
            size: sig.size,
            mtime_ns: sig.mtime_ns,
            hash,
        };
        let key = path.to_string_lossy().into_owned();
        if self.data.get(&key) != Some(&entry) {
            self.data.insert(key, entry);
            self.dirty = true;
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }

        let parent = self.memo_file.parent().unwrap_or_else(|| Path::new("."));
        let _ = fs::create_dir_all(parent);

        let serialized = match serde_json::to_string(&self.data) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to serialize font hash memo: {e}");
                return;
            }
        };

        if let Ok(mut temp_file) = NamedTempFile::new_in(parent) {
            if temp_file.write_all(serialized.as_bytes()).is_ok() && temp_file.flush().is_ok() {
                if temp_file.persist(&self.memo_file).is_ok() {
                    self.dirty = false;
                } else {
                    warn!(
                        "Failed to atomically replace font hash memo at {}",
                        self.memo_file.display()
                    );
                }
            }
        }
    }
}

// ─── FONT CLAIM & MANAGER ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FontClaim {
    pub name: String,
    pub size: u64,
    pub staging_path: PathBuf,
    pub identity: (String, u64),
    promoted: bool,
    released: bool,
}

impl FontClaim {
    pub fn is_promoted(&self) -> bool {
        self.promoted
    }

    pub fn is_released(&self) -> bool {
        self.released
    }
}

#[derive(Default)]
struct FontRegistryState {
    claimed_hashes: HashSet<String>,
    claimed_name_size: HashSet<(String, u64)>,
}

pub struct FontRegistry {
    pub fonts_dir: PathBuf,
    hash_memo: Option<Arc<Mutex<FontHashMemo>>>,
    state: Mutex<FontRegistryState>,
    staging_dir: PathBuf,
}

impl FontRegistry {
    pub fn new(fonts_dir: PathBuf, hash_memo: Option<Arc<Mutex<FontHashMemo>>>) -> Self {
        let staging_dir = fonts_dir.join(".font_staging");
        let manager = Self {
            fonts_dir,
            hash_memo,
            state: Mutex::new(FontRegistryState::default()),
            staging_dir,
        };
        manager.seed_existing();
        manager
    }

    fn seed_existing(&self) {
        if !self.fonts_dir.is_dir() {
            return;
        }

        let Ok(entries) = fs::read_dir(&self.fonts_dir) else {
            return;
        };

        let mut discovered_name_sizes = Vec::new();
        let mut discovered_hashes = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            if !FONT_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }

            if let Ok(meta) = entry.metadata() {
                let size = meta.len();
                let file_name = entry.file_name().to_string_lossy().into_owned();
                discovered_name_sizes.push((file_name, size));

                if let Some(sig) = FileSignature::of(&path) {
                    let mut file_hash = None;
                    if let Some(memo_arc) = &self.hash_memo {
                        let memo = memo_arc.lock();
                        file_hash = memo.get(&path, sig);
                    }

                    if file_hash.is_none() {
                        if let Ok(h) = hash_file(&path) {
                            if let Some(memo_arc) = &self.hash_memo {
                                let mut memo = memo_arc.lock();
                                memo.put(&path, sig, h.clone());
                            }
                            file_hash = Some(h);
                        }
                    }
                    if let Some(h) = file_hash {
                        discovered_hashes.push(h);
                    }
                }
            }
        }

        let mut state = self.state.lock();
        state.claimed_name_size.extend(discovered_name_sizes);
        state.claimed_hashes.extend(discovered_hashes);
    }

    pub fn claimed_hashes(&self) -> HashSet<String> {
        self.state.lock().claimed_hashes.clone()
    }

    /// Atomically reserve a font by sanitized name and size.
    /// Returns a FontClaim with a manager-created same-filesystem staging path, or None if already claimed.
    pub fn claim(&self, name: &str, size: u64) -> Option<FontClaim> {
        let safe_name = safe_filename(name);
        let key = (safe_name.clone(), size);
        let mut state = self.state.lock();
        if state.claimed_name_size.contains(&key) {
            return None;
        }

        let _ = fs::create_dir_all(&self.staging_dir);
        state.claimed_name_size.insert(key.clone());

        let unique_id = crate::domain::unique_file_token();
        let staging_path = self
            .staging_dir
            .join(format!(".tmp_{unique_id}_{safe_name}"));

        Some(FontClaim {
            name: safe_name,
            size,
            staging_path,
            identity: key,
            promoted: false,
            released: false,
        })
    }

    /// Validate, hash, deduplicate, and atomically promote a staged font file to fonts_dir.
    /// If hash is duplicate or validation fails, staging file is removed and released.
    /// Returns the final promoted destination Path, or None if duplicate or failed.
    pub fn promote(&self, claim: &mut FontClaim) -> Option<PathBuf> {
        let cleanup_and_release = |claim: &mut FontClaim, state: &mut FontRegistryState| {
            let _ = fs::remove_file(&claim.staging_path);
            state.claimed_name_size.remove(&claim.identity);
            claim.released = true;
        };

        let meta = match fs::metadata(&claim.staging_path) {
            Ok(m) => m,
            Err(_) => {
                let mut state = self.state.lock();
                cleanup_and_release(claim, &mut state);
                return None;
            }
        };

        if !meta.is_file() || meta.len() == 0 {
            let mut state = self.state.lock();
            cleanup_and_release(claim, &mut state);
            return None;
        }

        let file_hash = match hash_file(&claim.staging_path) {
            Ok(h) => h,
            Err(_) => {
                let mut state = self.state.lock();
                cleanup_and_release(claim, &mut state);
                return None;
            }
        };

        let mut state = self.state.lock();

        if state.claimed_hashes.contains(&file_hash) {
            let _ = fs::remove_file(&claim.staging_path);
            claim.released = true;
            return None;
        }

        if let Err(e) = fs::create_dir_all(&self.fonts_dir) {
            error!(
                "Failed to create fonts dir '{}': {e}",
                self.fonts_dir.display()
            );
            cleanup_and_release(claim, &mut state);
            return None;
        }

        let mut target_path = self.fonts_dir.join(&claim.name);
        if target_path.exists() {
            let stem = target_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = target_path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();

            let hash_prefix = if file_hash.len() >= 8 {
                &file_hash[..8]
            } else {
                &file_hash
            };
            target_path = self.fonts_dir.join(format!("{stem}_{hash_prefix}{ext}"));

            let mut counter = 1;
            while target_path.exists() {
                target_path = self
                    .fonts_dir
                    .join(format!("{stem}_{hash_prefix}_{counter}{ext}"));
                counter += 1;
            }
        }

        if let Err(e) = crate::domain::promote_file(&claim.staging_path, &target_path) {
            error!("Failed to promote font '{}': {e}", claim.name);
            cleanup_and_release(claim, &mut state);
            return None;
        }

        state.claimed_hashes.insert(file_hash.clone());
        claim.promoted = true;

        if let Some(memo_arc) = &self.hash_memo {
            if let Some(sig) = FileSignature::of(&target_path) {
                let mut memo = memo_arc.lock();
                memo.put(&target_path, sig, file_hash);
            }
        }

        Some(target_path)
    }

    /// Idempotently release an uncommitted claim and clean up its staging path.
    /// Never removes committed/promoted files.
    pub fn release(&self, claim: &mut FontClaim) {
        if claim.promoted || claim.released {
            return;
        }

        let mut state = self.state.lock();
        state.claimed_name_size.remove(&claim.identity);
        let _ = fs::remove_file(&claim.staging_path);
        claim.released = true;
    }

    /// Remove staging directory if empty.
    pub fn cleanup_staging(&self) {
        if self.staging_dir.is_dir() {
            let _ = fs::remove_dir(&self.staging_dir);
        }
    }
}

/// RAII guard that holds uncommitted font claims and releases them on drop unless defused.
pub struct FontClaimsGuard<'a> {
    registry: &'a FontRegistry,
    claims: Vec<(u32, FontClaim)>,
    defused: bool,
}

impl<'a> FontClaimsGuard<'a> {
    pub fn new(registry: &'a FontRegistry) -> Self {
        Self {
            registry,
            claims: Vec::new(),
            defused: false,
        }
    }

    pub fn push(&mut self, track_id: u32, claim: FontClaim) {
        self.claims.push((track_id, claim));
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    pub fn pop(&mut self) -> Option<(u32, FontClaim)> {
        self.claims.pop()
    }

    pub fn len(&self) -> usize {
        self.claims.len()
    }

    pub fn claims(&self) -> &[(u32, FontClaim)] {
        &self.claims
    }

    pub fn drain_claims(&mut self) -> std::vec::Drain<'_, (u32, FontClaim)> {
        self.claims.drain(..)
    }

    pub fn defuse(&mut self) {
        self.defused = true;
    }
}

impl<'a> Drop for FontClaimsGuard<'a> {
    fn drop(&mut self) {
        if !self.defused {
            for (_, mut claim) in self.claims.drain(..) {
                self.registry.release(&mut claim);
            }
        }
    }
}
