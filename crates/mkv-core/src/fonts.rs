//! Font discovery, deduplication, and transactional promotion.
//!
//! Lifecycle: claim -> stage -> deduplicate -> promote (or release).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use blake2::{Blake2b512, Digest};
use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::{error, warn};

use crate::domain::{
    font_mime, natural_sort_key, promote_file, safe_filename, unique_file_token, FONT_EXTENSIONS,
};
use crate::scan::FileSignature;

pub(crate) const FONT_DIR_NAMES: &[&str] = &["fonts", "font", "attachments"];

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

/// Build attachment arguments for all fonts found in `fonts_dir`, serving and filling
/// `font_cache`. A `fonts_dir` that is not a directory is not cached, so the first scan made
/// once it is one sees its fonts. An existing directory is cached on its first scan, even
/// when it is empty.
pub fn scan_fonts(fonts_dir: &Path, font_cache: &mut HashMap<PathBuf, FontInfo>) -> FontInfo {
    if let Some(info) = font_cache.get(fonts_dir) {
        return info.clone();
    }

    // Intentionally do not cache a path that is not a directory: if extraction creates
    // fonts_dir later in the run, subsequent scans must discover it.
    if !fonts_dir.is_dir() {
        return FontInfo::default();
    }
    let Ok(entries) = fs::read_dir(fonts_dir) else {
        return FontInfo::default();
    };

    let mut fonts: Vec<(PathBuf, String, &'static str)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let mime = font_mime(&ext)?;
            if !path.is_file() {
                return None;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            Some((path, file_name, mime))
        })
        .collect();
    fonts.sort_by_key(|(_, file_name, _)| natural_sort_key(file_name));

    let count = fonts.len();
    let mut args = Vec::with_capacity(count * 4);
    for (path, _, mime) in fonts {
        args.push("--attachment-mime-type".to_string());
        args.push(mime.to_string());
        args.push("--attach-file".to_string());
        args.push(path.to_string_lossy().into_owned());
    }

    let info = FontInfo { args, count };
    font_cache.insert(fonts_dir.to_path_buf(), info.clone());
    info
}

/// Resolve which directory to use for fonts, in priority order:
///   1. `input_dir/fonts`, when it is a directory
///   2. the first (by path order) child directory of `input_dir` named `fonts`, `font`, or
///      `attachments`, ignoring ASCII case
///   3. `input_dir/fonts`, even though it is not a directory
///
/// An explicit fonts directory (`--fonts-dir`) outranks all of these: the caller
/// (`ReleasePreparer`) applies it before consulting this function.
pub fn find_fonts_dir(
    input_dir: &Path,
    fonts_dir_cache: &mut HashMap<PathBuf, PathBuf>,
) -> PathBuf {
    if let Some(cached) = fonts_dir_cache.get(input_dir) {
        return cached.clone();
    }
    let resolved = locate_fonts_dir(input_dir);
    fonts_dir_cache.insert(input_dir.to_path_buf(), resolved.clone());
    resolved
}

fn locate_fonts_dir(input_dir: &Path) -> PathBuf {
    let exact = input_dir.join("fonts");
    if exact.is_dir() {
        return exact;
    }

    let Ok(entries) = fs::read_dir(input_dir) else {
        return exact;
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            FONT_DIR_NAMES.iter().any(|d| name.eq_ignore_ascii_case(d))
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .min()
        .unwrap_or(exact)
}

// ─── FONT HASH MEMO ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct FontMemoEntry {
    size: u64,
    mtime_ns: u128,
    hash: String,
}

/// Persistent font path -> content hash cache, valid while a file's signature is unchanged.
#[derive(Debug)]
pub struct FontHashMemo {
    memo_file: PathBuf,
    data: HashMap<String, FontMemoEntry>,
    dirty: bool,
}

impl FontHashMemo {
    pub fn new(memo_file: PathBuf) -> Self {
        let data = Self::load(&memo_file);
        Self {
            memo_file,
            data,
            dirty: false,
        }
    }

    fn load(memo_file: &Path) -> HashMap<String, FontMemoEntry> {
        if !memo_file.exists() {
            return HashMap::new();
        }
        let content = match fs::read_to_string(memo_file) {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    "Failed to read font hash memo from {}: {e}",
                    memo_file.display()
                );
                return HashMap::new();
            }
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!(
                "Failed to parse font hash memo from {}: {e}",
                memo_file.display()
            );
            HashMap::new()
        })
    }

    pub fn get(&self, path: &Path, sig: FileSignature) -> Option<String> {
        self.data
            .get(path.to_string_lossy().as_ref())
            .filter(|entry| entry.size == sig.size && entry.mtime_ns == sig.mtime_ns)
            .map(|entry| entry.hash.clone())
    }

    pub fn put(&mut self, path: &Path, sig: FileSignature, hash: String) {
        let entry = FontMemoEntry {
            size: sig.size,
            mtime_ns: sig.mtime_ns,
            hash,
        };
        let key = path.to_string_lossy();
        if self.data.get(key.as_ref()) != Some(&entry) {
            self.data.insert(key.into_owned(), entry);
            self.dirty = true;
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }

        let parent = match self.memo_file.parent() {
            Some(p) if !p.as_os_str().is_empty() => {
                let _ = fs::create_dir_all(p);
                p
            }
            _ => Path::new("."),
        };

        // Stream straight into the temp file, as `PersistentInspectCache` does (ADR-0013
        // Decision 4). Building one `String` first would hold the entire memo in the heap
        // in addition to the file being written.
        let mut temp_file = match NamedTempFile::new_in(parent) {
            Ok(f) => f,
            Err(e) => {
                warn!(
                    "Failed to create tempfile for font hash memo save in {}: {e}",
                    parent.display()
                );
                return;
            }
        };

        {
            let mut writer = BufWriter::new(&mut temp_file);
            if let Err(e) = serde_json::to_writer(&mut writer, &self.data) {
                warn!("Failed to write font hash memo: {e}");
                return;
            }
            if let Err(e) = writer.flush() {
                warn!("Failed to flush font hash memo stream: {e}");
                return;
            }
        }

        match temp_file.persist(&self.memo_file) {
            Ok(_) => self.dirty = false,
            Err(e) => warn!(
                "Failed to atomically replace font hash memo at {}: {e}",
                self.memo_file.display()
            ),
        }
    }
}

// ─── FONT CLAIM & REGISTRY ───────────────────────────────────────────────────

/// A reservation of a sanitized font name and size, plus the staging path its bytes go to.
#[derive(Debug)]
pub struct FontClaim {
    pub staging_path: PathBuf,
    identity: (String, u64),
    promoted: bool,
    released: bool,
}

impl FontClaim {
    /// The sanitized file name the font is claimed under.
    pub fn name(&self) -> &str {
        &self.identity.0
    }

    pub fn is_promoted(&self) -> bool {
        self.promoted
    }

    pub fn is_released(&self) -> bool {
        self.released
    }
}

/// The seeded fonts of one size. They are hashed once, by the first promote of a staged font
/// of that size; a concurrent promote of the same size waits on `hashes` instead of hashing
/// them again or checking before their hashes are in.
struct SeededSizeBucket {
    fonts: Vec<(PathBuf, FileSignature)>,
    hashes: OnceLock<Vec<String>>,
}

#[derive(Default)]
struct FontRegistryState {
    claimed_hashes: HashSet<String>,
    claimed_name_size: HashSet<(String, u64)>,
    /// Seeded fonts whose hashes are not yet in `claimed_hashes`, by size. A bucket leaves
    /// the map in the critical section that inserts its hashes, so a promote that finds no
    /// bucket for its size finds those hashes instead.
    unhashed_seeded: HashMap<u64, Arc<SeededSizeBucket>>,
}

/// Drops `claim`'s (name, size) reservation and its staging file.
fn release_locked(state: &mut FontRegistryState, claim: &mut FontClaim) {
    state.claimed_name_size.remove(&claim.identity);
    let _ = fs::remove_file(&claim.staging_path);
    claim.released = true;
}

pub struct FontRegistry {
    fonts_dir: PathBuf,
    staging_dir: PathBuf,
    hash_memo: Option<Arc<Mutex<FontHashMemo>>>,
    state: Mutex<FontRegistryState>,
}

impl FontRegistry {
    pub fn new(fonts_dir: PathBuf, hash_memo: Option<Arc<Mutex<FontHashMemo>>>) -> Self {
        let staging_dir = fonts_dir.join(".font_staging");
        let registry = Self {
            fonts_dir,
            staging_dir,
            hash_memo,
            state: Mutex::new(FontRegistryState::default()),
        };
        registry.seed_existing();
        registry
    }

    /// Reserves the name and resolved size of every regular font file already in
    /// `fonts_dir`, so extraction does not re-stage them, and files each one by size for
    /// [`Self::lock_with_seeded_hashes`]. Nothing is hashed here: identical content implies
    /// identical size, so a seeded font is hashed only when a staged font of its size is
    /// promoted.
    fn seed_existing(&self) {
        let Ok(entries) = fs::read_dir(&self.fonts_dir) else {
            return;
        };

        let mut discovered_name_sizes = Vec::new();
        let mut by_size: HashMap<u64, Vec<(PathBuf, FileSignature)>> = HashMap::new();

        for entry in entries.flatten() {
            let path = entry.path();
            let is_font = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                FONT_EXTENSIONS
                    .iter()
                    .any(|f| f.strip_prefix('.').unwrap_or(f).eq_ignore_ascii_case(e))
            });
            if !is_font {
                continue;
            }

            let Some(sig) = entry
                .metadata()
                .ok()
                .and_then(|meta| FileSignature::of_dir_entry(&entry, &meta))
            else {
                continue;
            };
            let file_name = entry.file_name().to_string_lossy().into_owned();
            discovered_name_sizes.push((file_name, sig.size));
            by_size.entry(sig.size).or_default().push((path, sig));
        }

        let mut state = self.state.lock();
        state.claimed_name_size.extend(discovered_name_sizes);
        state
            .unhashed_seeded
            .extend(by_size.into_iter().map(|(size, fonts)| {
                let bucket = SeededSizeBucket {
                    fonts,
                    hashes: OnceLock::new(),
                };
                (size, Arc::new(bucket))
            }));
    }

    /// Locks the registry with every seeded font of `size` hashed into `claimed_hashes`, so a
    /// duplicate check made under the returned guard sees them all. The hashing runs without the
    /// registry lock held, and the bucket leaves the map in the critical section that inserts its
    /// hashes. Each font goes through [`Self::memoized_hash`] with its seed-time signature:
    /// without a memo or on a memo miss, a font that changed since seeding is hashed as found and
    /// one that vanished is skipped, while a memo hit returns the stored hash without opening the
    /// file.
    fn lock_with_seeded_hashes(&self, size: u64) -> MutexGuard<'_, FontRegistryState> {
        // Two statements on purpose: the temporary guard drops at the end of the first, before
        // the lock is taken again below; folding them into one `let else` would self-deadlock.
        let bucket = self.state.lock().unhashed_seeded.get(&size).cloned();
        let Some(bucket) = bucket else {
            // Never seeded, or already absorbed: `claimed_hashes` only grows.
            return self.state.lock();
        };
        // `OnceLock` runs the hashing once; a concurrent caller blocks until it is done. A
        // panicking initializer leaves the cell empty for the next caller to retry.
        let hashes = bucket.hashes.get_or_init(|| {
            bucket
                .fonts
                .iter()
                .filter_map(|(path, sig)| self.memoized_hash(path, *sig))
                .collect()
        });
        let mut state = self.state.lock();
        state.unhashed_seeded.remove(&size);
        state.claimed_hashes.extend(hashes.iter().cloned());
        state
    }

    /// Content hash of `path`, served from the memo when `sig` matches. Hashing runs without
    /// the memo lock held, since the memo is shared across registries.
    fn memoized_hash(&self, path: &Path, sig: FileSignature) -> Option<String> {
        if let Some(memo) = &self.hash_memo {
            if let Some(hash) = memo.lock().get(path, sig) {
                return Some(hash);
            }
        }
        let hash = hash_file(path).ok()?;
        if let Some(memo) = &self.hash_memo {
            memo.lock().put(path, sig, hash.clone());
        }
        Some(hash)
    }

    /// The content hashes computed so far: every promoted font's, and those of the seeded
    /// fonts whose size a promote has met. A seeded font is not hashed until a staged font of
    /// its size is promoted, so until then its hash is absent here.
    pub fn claimed_hashes(&self) -> HashSet<String> {
        self.state.lock().claimed_hashes.clone()
    }

    /// Atomically reserve a font by sanitized name and size.
    /// Returns a FontClaim with a registry-created same-filesystem staging path, or None if already claimed.
    pub fn claim(&self, name: &str, size: u64) -> Option<FontClaim> {
        let identity = (safe_filename(name), size);
        let mut state = self.state.lock();
        if state.claimed_name_size.contains(&identity) {
            return None;
        }

        let _ = fs::create_dir_all(&self.staging_dir);
        state.claimed_name_size.insert(identity.clone());
        let staging_file = format!(".tmp_{}_{}", unique_file_token(), identity.0);

        Some(FontClaim {
            staging_path: self.staging_dir.join(staging_file),
            identity,
            promoted: false,
            released: false,
        })
    }

    /// Validate, hash, deduplicate, and atomically promote a staged font file to fonts_dir.
    /// If hash is duplicate or validation fails, staging file is removed and released.
    /// Returns the final promoted destination Path, or None if duplicate or failed.
    pub fn promote(&self, claim: &mut FontClaim) -> Option<PathBuf> {
        let staged = match fs::metadata(&claim.staging_path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => hash_file(&claim.staging_path)
                .ok()
                .map(|hash| (hash, meta.len())),
            _ => None,
        };
        let Some((file_hash, size)) = staged else {
            let mut state = self.state.lock();
            release_locked(&mut state, claim);
            return None;
        };

        // Only a seeded font of the staged file's size can share its content.
        let mut state = self.lock_with_seeded_hashes(size);

        // A content duplicate keeps its (name, size) reservation, so later files carrying the
        // same attachment skip extracting it.
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
            release_locked(&mut state, claim);
            return None;
        }

        let target_path = self.vacant_target(claim.name(), &file_hash);
        if let Err(e) = promote_file(&claim.staging_path, &target_path) {
            error!("Failed to promote font '{}': {e}", claim.name());
            release_locked(&mut state, claim);
            return None;
        }

        claim.promoted = true;
        if let Some(memo) = &self.hash_memo {
            if let Some(sig) = FileSignature::of(&target_path) {
                memo.lock().put(&target_path, sig, file_hash.clone());
            }
        }
        state.claimed_hashes.insert(file_hash);

        Some(target_path)
    }

    /// `fonts_dir/name`, or when that is taken, a sibling disambiguated by the hash prefix
    /// and, if needed, a counter.
    fn vacant_target(&self, name: &str, file_hash: &str) -> PathBuf {
        let target = self.fonts_dir.join(name);
        if !target.exists() {
            return target;
        }

        let stem = target
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = target
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let hash_prefix = file_hash.get(..8).unwrap_or(file_hash);

        let mut candidate = self.fonts_dir.join(format!("{stem}_{hash_prefix}{ext}"));
        let mut counter = 1;
        while candidate.exists() {
            candidate = self
                .fonts_dir
                .join(format!("{stem}_{hash_prefix}_{counter}{ext}"));
            counter += 1;
        }
        candidate
    }

    /// Idempotently release an uncommitted claim and clean up its staging path.
    /// Never removes committed/promoted files.
    pub fn release(&self, claim: &mut FontClaim) {
        if claim.promoted || claim.released {
            return;
        }
        release_locked(&mut self.state.lock(), claim);
    }

    /// Remove staging directory if empty.
    pub fn cleanup_staging(&self) {
        let _ = fs::remove_dir(&self.staging_dir);
    }
}

/// RAII guard that releases every claim still held when it drops. Take a claim out with
/// [`FontClaimsGuard::pop`] to settle it yourself.
pub struct FontClaimsGuard<'a> {
    registry: &'a FontRegistry,
    claims: Vec<(u32, FontClaim)>,
}

impl<'a> FontClaimsGuard<'a> {
    pub fn new(registry: &'a FontRegistry) -> Self {
        Self {
            registry,
            claims: Vec::new(),
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

    pub fn claims(&self) -> &[(u32, FontClaim)] {
        &self.claims
    }
}

impl Drop for FontClaimsGuard<'_> {
    fn drop(&mut self) {
        for (_, mut claim) in self.claims.drain(..) {
            self.registry.release(&mut claim);
        }
    }
}
