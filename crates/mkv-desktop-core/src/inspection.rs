//! The Inspection Coordinator: Directory Inspections for the Desktop Core actor. The `Inspector`
//! port resolves a typed path to its Directory Identity and inspects a directory; the
//! `EngineInspector` adapter does both with `mkv-core`, and tests script a fake. The in-flight
//! registry lets the scans of one key share one inspection.
//!
//! One Directory Inspection runs at a time across both modes: `ScanState::begin` cancels the one
//! before it, so a scan of another directory, in either view, fails a scan still inspecting. Two
//! scans of the same key share one inspection instead, so the Remux and Extract views can both
//! show one directory.

use crate::control::{InspectCacheState, ScanState};
use crate::job::display_canonical_path;
use mkv_core::manifest::InspectionManifest;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

/// What the actor needs to scan a directory. Both run off the actor.
pub trait Inspector: Send + Sync + 'static {
    /// The Directory Identity of `path`, or why it has none.
    fn identity(&self, path: String) -> impl Future<Output = Result<String, String>> + Send;

    /// The Inspection Manifest of the directory `identity` names, bypassing the inspect cache
    /// when `no_cache`.
    fn inspect(
        &self,
        identity: String,
        no_cache: bool,
    ) -> impl Future<Output = Result<InspectionManifest, String>> + Send;
}

/// Canonicalizes `path` on the blocking pool and pairs the result with its Directory Identity:
/// the display form of the canonical path, which is the only spelling the UI compares
/// directories by.
pub async fn canonicalize_directory(path: String) -> Result<(PathBuf, String), String> {
    let dir = tokio::task::spawn_blocking(move || std::fs::canonicalize(path))
        .await
        .map_err(|e| format!("Async runtime error: {e}"))?
        .map_err(|e| format!("Invalid directory path: {e}"))?;
    let identity = display_canonical_path(&dir.to_string_lossy());
    Ok((dir, identity))
}

/// The `Inspector` of the app: `mkv-core`'s manifest over the session inspect cache, registered
/// in the one `ScanState` that Stop and Close tear down.
pub struct EngineInspector {
    pub scan: Arc<ScanState>,
    pub cache: InspectCacheState,
}

impl Inspector for EngineInspector {
    async fn identity(&self, path: String) -> Result<String, String> {
        canonicalize_directory(path)
            .await
            .map(|(_, identity)| identity)
    }

    async fn inspect(
        &self,
        identity: String,
        no_cache: bool,
    ) -> Result<InspectionManifest, String> {
        // A closing app refuses before it touches the path. Canonicalizing runs before
        // `ScanState::begin` takes the start lock, whose guard cannot be held across this await.
        self.scan.ensure_accepting_starts()?;
        let (dir, identity) = canonicalize_directory(identity).await?;
        let (_registration, cancel_token) = self.scan.begin(&dir)?;
        let cache = if no_cache {
            None
        } else {
            Some(self.cache.shared().await?)
        };
        let mut manifest =
            mkv_core::manifest::generate_inspection_manifest(&dir, cache, Some(&cancel_token))
                .await?;
        manifest
            .extra
            .insert("canonical_dir".to_string(), identity.into());
        Ok(manifest)
    }
}

/// The key two scans share an inspection by: the Directory Identity and whether the inspect cache
/// is bypassed.
pub type InspectionKey = (String, bool);

/// The in-flight registry: every running inspection with the scans waiting on it (`W`). A scan
/// joins the running inspection of its key; a forced scan always starts a new one, which later
/// scans of the key then join.
pub struct InFlight<W> {
    next_id: u64,
    /// The inspection a new scan of each key joins.
    current: HashMap<InspectionKey, u64>,
    waiting: HashMap<u64, (InspectionKey, Vec<W>)>,
}

impl<W> Default for InFlight<W> {
    fn default() -> Self {
        Self {
            next_id: 1,
            current: HashMap::new(),
            waiting: HashMap::new(),
        }
    }
}

impl<W> InFlight<W> {
    /// Adds `waiter` to the running inspection of `key`, unless `force` or none runs. Returns the
    /// ID of the inspection the caller must then start, or `None` when it joined one.
    pub fn join(&mut self, key: InspectionKey, force: bool, waiter: W) -> Option<u64> {
        if !force {
            if let Some(id) = self.current.get(&key) {
                self.waiting.get_mut(id).unwrap().1.push(waiter);
                return None;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.current.insert(key.clone(), id);
        self.waiting.insert(id, (key, vec![waiter]));
        Some(id)
    }

    /// Ends inspection `id` and returns its waiters. Its key stays with a newer inspection.
    pub fn settle(&mut self, id: u64) -> Vec<W> {
        let Some((key, waiters)) = self.waiting.remove(&id) else {
            return Vec::new();
        };
        if self.current.get(&key) == Some(&id) {
            self.current.remove(&key);
        }
        waiters
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> InspectionKey {
        (path.to_string(), false)
    }

    #[test]
    fn test_scans_share_only_the_current_same_key_inspection() {
        let mut in_flight = InFlight::default();
        let first = in_flight.join(key("/release"), false, "first").unwrap();
        assert_eq!(in_flight.join(key("/release"), false, "shared"), None);
        assert_eq!(in_flight.settle(first), ["first", "shared"]);

        // Once settled, the next scan inspects again.
        let replacement = in_flight.join(key("/release"), false, "next").unwrap();
        assert_ne!(replacement, first);
        assert_eq!(in_flight.settle(replacement), ["next"]);
    }

    #[test]
    fn test_settling_one_key_keeps_another_keys_inspection_shared() {
        let mut in_flight = InFlight::default();
        let a = in_flight.join(key("/a"), false, "a").unwrap();
        // Another key between two same-key scans does not split them.
        let b = in_flight.join(key("/b"), false, "b").unwrap();
        assert_eq!(in_flight.join(key("/a"), false, "a2"), None);
        assert_eq!(in_flight.settle(a), ["a", "a2"]);
        assert_eq!(in_flight.join(key("/b"), false, "b2"), None);
        assert_eq!(in_flight.settle(b), ["b", "b2"]);
    }

    #[test]
    fn test_a_forced_scan_never_shares_a_running_inspection() {
        let mut in_flight = InFlight::default();
        let browse = in_flight.join(key("/release"), false, "browse").unwrap();
        let forced = in_flight.join(key("/release"), true, "forced").unwrap();
        assert_ne!(browse, forced);
        // A later scan joins the forced one.
        assert_eq!(in_flight.join(key("/release"), false, "later"), None);
        assert_eq!(in_flight.settle(forced), ["forced", "later"]);
        assert_eq!(in_flight.settle(browse), ["browse"]);
    }

    #[test]
    fn test_no_cache_splits_the_key() {
        let mut in_flight = InFlight::default();
        let cached = in_flight.join(key("/release"), false, "cached").unwrap();
        let bypass = in_flight
            .join(("/release".into(), true), false, "bypass")
            .unwrap();
        assert_ne!(cached, bypass);
        assert_eq!(
            in_flight.join(("/release".into(), true), false, "bypass2"),
            None
        );
        assert_eq!(in_flight.settle(bypass), ["bypass", "bypass2"]);
    }

    #[test]
    fn test_an_older_inspection_settling_first_cannot_evict_the_newer_forced_one() {
        let mut in_flight = InFlight::default();
        let older = in_flight.join(key("/release"), false, "older").unwrap();
        let forced = in_flight.join(key("/release"), true, "forced").unwrap();
        assert_eq!(in_flight.settle(older), ["older"]);
        assert_eq!(in_flight.join(key("/release"), false, "joins"), None);
        assert_eq!(in_flight.settle(forced), ["forced", "joins"]);
        // A settled inspection has no one left to answer.
        assert!(in_flight.settle(forced).is_empty());
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn engine() -> EngineInspector {
        EngineInspector {
            scan: Arc::default(),
            cache: InspectCacheState::default(),
        }
    }

    #[test]
    fn test_the_identity_is_the_display_form_of_the_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        // A dotted spelling of the directory.
        let spelled = tmp.path().join(".");

        let identity = block_on(engine().identity(spelled.to_string_lossy().into_owned())).unwrap();

        // The identity names the same real directory, in the display form.
        assert_eq!(
            std::fs::canonicalize(&identity).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
        assert!(!identity.starts_with(r"\\?\"), "{identity}");
        // Only Windows has a verbatim prefix to strip, so elsewhere the canonical path is the
        // identity.
        #[cfg(not(windows))]
        assert_eq!(
            identity,
            std::fs::canonicalize(tmp.path()).unwrap().to_string_lossy()
        );
    }

    #[test]
    fn test_the_manifest_reports_the_identity_as_its_canonical_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let spelled = tmp.path().join(".").to_string_lossy().into_owned();
        let inspector = engine();

        let manifest = block_on(inspector.inspect(spelled.clone(), true)).unwrap();
        let identity = block_on(inspector.identity(spelled)).unwrap();

        assert_eq!(
            manifest.extra["canonical_dir"].as_str(),
            Some(identity.as_str())
        );
        // The registration ended with the inspection.
        assert!(!inspector.scan.is_active());
    }

    #[cfg(unix)]
    #[test]
    fn test_the_identity_of_a_symlink_is_its_target() {
        let target = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();

        let identity = block_on(engine().identity(link.to_string_lossy().into_owned())).unwrap();

        let expected = std::fs::canonicalize(target.path()).unwrap();
        assert_eq!(identity, expected.to_string_lossy());
    }

    #[test]
    fn test_a_missing_path_has_no_identity_and_no_inspection() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing").to_string_lossy().into_owned();
        let inspector = engine();

        let error = block_on(inspector.identity(missing.clone())).unwrap_err();
        assert!(error.starts_with("Invalid directory path: "), "{error}");
        let error = block_on(inspector.inspect(missing, true)).unwrap_err();
        assert!(error.starts_with("Invalid directory path: "), "{error}");
    }
}
