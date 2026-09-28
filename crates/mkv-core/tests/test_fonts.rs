use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use mkv_core::fonts::{
    find_fonts_dir, hash_file, scan_fonts, FontClaimsGuard, FontHashMemo, FontRegistry,
};
use mkv_core::scan::FileSignature;
use tempfile::tempdir;

#[test]
fn test_hash_file() {
    let tmp = tempdir().unwrap();
    let p = tmp.path().join("sample.txt");
    fs::write(&p, b"test").unwrap();
    let h = hash_file(&p).unwrap();
    assert_eq!(h.len(), 128);
    assert!(h.starts_with("a71079d42853dea2"));
}

#[test]
fn test_claim_and_promote() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir.clone(), None);

    let mut claim = mgr.claim("Arial.ttf", 100).expect("claim should succeed");
    assert_eq!(claim.name, "Arial.ttf");
    assert_eq!(
        claim.staging_path.parent().unwrap().file_name().unwrap(),
        ".font_staging"
    );

    fs::write(&claim.staging_path, b"unique font content 1").unwrap();

    let promoted = mgr.promote(&mut claim).expect("promote should return path");
    assert_eq!(promoted, fonts_dir.join("Arial.ttf"));
    assert_eq!(fs::read(&promoted).unwrap(), b"unique font content 1");
    assert!(!claim.staging_path.exists());
    assert!(claim.is_promoted());
}

#[test]
fn test_claim_duplicate_name_size_rejected() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let claim1 = mgr.claim("Arial.ttf", 100);
    assert!(claim1.is_some());

    let claim2 = mgr.claim("Arial.ttf", 100);
    assert!(claim2.is_none());
}

#[test]
fn test_claim_different_size_allowed() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let claim1 = mgr.claim("Arial.ttf", 100);
    let claim2 = mgr.claim("Arial.ttf", 200);
    assert!(claim1.is_some());
    assert!(claim2.is_some());
}

#[test]
fn test_release_allows_reclaim() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let mut claim1 = mgr.claim("Arial.ttf", 100).unwrap();
    fs::write(&claim1.staging_path, b"temp").unwrap();

    mgr.release(&mut claim1);
    assert!(!claim1.staging_path.exists());
    assert!(claim1.is_released());

    // Should be re-claimable now
    let claim2 = mgr.claim("Arial.ttf", 100);
    assert!(claim2.is_some());
}

#[test]
fn test_promote_hash_deduplication() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir.clone(), None);

    // First font
    let mut claim1 = mgr.claim("FontA.ttf", 100).unwrap();
    fs::write(&claim1.staging_path, b"identical font content").unwrap();
    let promoted1 = mgr.promote(&mut claim1);
    assert_eq!(promoted1, Some(fonts_dir.join("FontA.ttf")));

    // Second font with different name and size but same content
    let mut claim2 = mgr.claim("FontB.ttf", 200).unwrap();
    fs::write(&claim2.staging_path, b"identical font content").unwrap();
    let promoted2 = mgr.promote(&mut claim2);

    // Content duplicate should be deduplicated (return None, staging file cleaned up)
    assert!(promoted2.is_none());
    assert!(!claim2.staging_path.exists());
    assert!(!fonts_dir.join("FontB.ttf").exists());
}

#[test]
fn test_promote_hash_deduplication_preserves_name_size_claim() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir.clone(), None);

    // First font
    let mut claim1 = mgr.claim("FontA.ttf", 100).unwrap();
    fs::write(&claim1.staging_path, b"shared font data").unwrap();
    let promoted1 = mgr.promote(&mut claim1);
    assert_eq!(promoted1, Some(fonts_dir.join("FontA.ttf")));

    // Second font with different name/size but same content
    let mut claim2 = mgr.claim("FontB.ttf", 200).unwrap();
    fs::write(&claim2.staging_path, b"shared font data").unwrap();
    let promoted2 = mgr.promote(&mut claim2);

    // Should be deduplicated
    assert!(promoted2.is_none());
    assert!(!claim2.staging_path.exists());
    assert!(!fonts_dir.join("FontB.ttf").exists());

    // Preserves (name, size) in claimed_name_size so subsequent episodes skip extraction
    let claim3 = mgr.claim("FontB.ttf", 200);
    assert!(
        claim3.is_none(),
        "subsequent claim of hash-deduplicated font should be skipped via claimed_name_size"
    );
}

#[test]
fn test_collision_safe_target_allocation() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let existing_font = fonts_dir.join("Arial.ttf");
    fs::write(&existing_font, b"existing content").unwrap();

    let mgr = FontRegistry::new(fonts_dir.clone(), None);

    // New font with same filename but different content and size
    let mut claim = mgr.claim("Arial.ttf", 999).unwrap();
    fs::write(&claim.staging_path, b"new different content").unwrap();

    let promoted = mgr.promote(&mut claim).expect("promote should succeed");
    assert_ne!(promoted, existing_font);
    let promoted_name = promoted.file_name().unwrap().to_string_lossy();
    assert!(promoted_name.starts_with("Arial_"));
    assert!(promoted_name.ends_with(".ttf"));
    assert_eq!(fs::read(&promoted).unwrap(), b"new different content");

    // Original file is preserved untouched
    assert_eq!(fs::read(&existing_font).unwrap(), b"existing content");
}

#[test]
fn test_seed_existing_fonts() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let existing_font = fonts_dir.join("Existing.otf");
    fs::write(&existing_font, b"pre-existing content").unwrap();
    let size = fs::metadata(&existing_font).unwrap().len();

    let mgr = FontRegistry::new(fonts_dir, None);

    // Name and size should already be reserved
    assert!(mgr.claim("Existing.otf", size).is_none());

    // Content duplicate should also be rejected upon promote
    let mut claim = mgr.claim("Other.otf", 50).unwrap();
    fs::write(&claim.staging_path, b"pre-existing content").unwrap();
    assert!(mgr.promote(&mut claim).is_none());
}

/// `seed_existing` now derives each font's signature from the `DirEntry` metadata it
/// already holds instead of issuing a second `stat`. The signature it records must be
/// byte-identical to what a symlink-resolving `FileSignature::of` computes, otherwise
/// every later memo lookup would miss and fonts would be re-hashed on each run.
#[test]
fn test_seed_existing_records_memo_signature_matching_resolved_lookup() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let font = fonts_dir.join("Real.otf");
    fs::write(&font, b"real font content").unwrap();

    let memo = Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
        tmp.path().join("memo.json"),
    )));

    let _mgr = FontRegistry::new(fonts_dir, Some(Arc::clone(&memo)));

    let expected = hash_file(&font).unwrap();
    let resolved_sig = FileSignature::of(&font).unwrap();
    assert_eq!(
        memo.lock().get(&font, resolved_sig),
        Some(expected),
        "seeded signature must match a symlink-resolving FileSignature::of lookup"
    );
}

/// A symlinked font must be seeded under its *resolved* signature. `DirEntry::metadata`
/// reports the link itself, so `FileSignature::of_dir_entry` re-resolves the entry's path.
#[cfg(unix)]
#[test]
fn test_seed_existing_symlinked_font_uses_resolved_signature() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let real = fonts_dir.join("Real.otf");
    fs::write(&real, b"real font content").unwrap();
    let link = fonts_dir.join("Linked.otf");
    symlink(&real, &link).unwrap();

    let memo = Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
        tmp.path().join("memo.json"),
    )));

    let _mgr = FontRegistry::new(fonts_dir, Some(Arc::clone(&memo)));

    let expected = hash_file(&real).unwrap();
    let resolved_sig = FileSignature::of(&link).unwrap();
    let link_own_meta = fs::symlink_metadata(&link).unwrap();
    assert_ne!(
        resolved_sig,
        FileSignature::from_metadata(&link_own_meta),
        "precondition: link-own metadata must differ from the resolved signature"
    );
    assert_eq!(
        memo.lock().get(&link, resolved_sig),
        Some(expected),
        "a symlinked font must be memoized under its resolved signature"
    );
}

/// A symlinked font must be recorded in `claimed_name_size` under its *resolved* file size,
/// not the byte length of the symlink target path. Otherwise, `claim` with the real font size
/// fails to deduplicate and allows duplicate extraction staging.
#[cfg(unix)]
#[test]
fn test_seed_existing_symlinked_font_prevents_claim() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let real = fonts_dir.join("Real.otf");
    let font_data = b"simulated font payload exceeding target name length";
    fs::write(&real, font_data).unwrap();
    let link = fonts_dir.join("Linked.otf");
    symlink(&real, &link).unwrap();

    let mgr = FontRegistry::new(fonts_dir, None);

    assert!(
        mgr.claim("Linked.otf", font_data.len() as u64).is_none(),
        "symlinked font must be recognized in claimed_name_size under its real file size"
    );
}

/// A broken symlink in fonts_dir has no signature and must not be seeded into `claimed_name_size`.
#[cfg(unix)]
#[test]
fn test_seed_existing_broken_symlink_does_not_poison_claim() {
    use std::os::unix::fs::symlink;

    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let link = fonts_dir.join("Broken.otf");
    symlink(fonts_dir.join("Nonexistent.otf"), &link).unwrap();

    let mgr = FontRegistry::new(fonts_dir, None);

    assert!(
        mgr.claim("Broken.otf", 500).is_some(),
        "broken symlink must not poison claimed_name_size"
    );
}

#[test]
fn test_cleanup_staging() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir.clone(), None);

    let mut claim = mgr.claim("Test.ttf", 100).unwrap();
    fs::write(&claim.staging_path, b"abc").unwrap();
    mgr.promote(&mut claim).unwrap();

    let staging_dir = fonts_dir.join(".font_staging");
    mgr.cleanup_staging();
    assert!(!staging_dir.exists());
}

#[test]
fn test_font_hash_memo() {
    let tmp = tempdir().unwrap();
    let memo_file = tmp.path().join("font_memo.json");

    {
        let mut memo = FontHashMemo::new(memo_file.clone());
        let sig = FileSignature {
            size: 100,
            mtime_ns: 12345,
        };
        memo.put(&tmp.path().join("f1.ttf"), sig, "hash123".to_string());
        memo.save();
    }

    {
        let memo = FontHashMemo::new(memo_file);
        let sig = FileSignature {
            size: 100,
            mtime_ns: 12345,
        };
        let sig_diff_mtime = FileSignature {
            size: 100,
            mtime_ns: 99999,
        };
        assert_eq!(
            memo.get(&tmp.path().join("f1.ttf"), sig),
            Some("hash123".to_string())
        );
        assert_eq!(memo.get(&tmp.path().join("f1.ttf"), sig_diff_mtime), None);
        assert_eq!(memo.get(&tmp.path().join("f2.ttf"), sig), None);
    }
}

/// `FontHashMemo::save` streams through a `BufWriter`, so a memo small enough to fit the
/// buffer never exercises a mid-stream flush. This pins that a memo far larger than the
/// 8 KiB buffer still serializes completely and round-trips.
#[test]
fn test_font_hash_memo_large_save_round_trips_through_buffered_writer() {
    let tmp = tempdir().unwrap();
    let memo_file = tmp.path().join("font_memo.json");
    const ENTRIES: usize = 500;

    let sig = FileSignature {
        size: 100,
        mtime_ns: 12345,
    };
    {
        let mut memo = FontHashMemo::new(memo_file.clone());
        for i in 0..ENTRIES {
            let path = tmp
                .path()
                .join(format!("font-with-a-longish-name-{i:04}.ttf"));
            memo.put(&path, sig, format!("{:064x}", i));
        }
        memo.save();
    }

    assert!(
        fs::metadata(&memo_file).unwrap().len() > 8192,
        "precondition: the memo must exceed the BufWriter capacity"
    );
    let memo = FontHashMemo::new(memo_file);
    for i in 0..ENTRIES {
        let path = tmp
            .path()
            .join(format!("font-with-a-longish-name-{i:04}.ttf"));
        assert_eq!(
            memo.get(&path, sig),
            Some(format!("{:064x}", i)),
            "entry {i} did not survive the buffered save"
        );
    }
}

/// A memo with no pending changes must not rewrite its file. Comparing bytes cannot prove
/// this -- a rewrite of identical data produces identical bytes -- so compare inodes, which
/// a `persist` necessarily replaces.
#[cfg(unix)]
#[test]
fn test_font_hash_memo_save_is_noop_when_not_dirty() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempdir().unwrap();
    let memo_file = tmp.path().join("font_memo.json");
    let sig = FileSignature {
        size: 100,
        mtime_ns: 12345,
    };

    let mut memo = FontHashMemo::new(memo_file.clone());
    memo.put(&tmp.path().join("f1.ttf"), sig, "hash123".to_string());
    memo.save();
    let inode_after_first = fs::metadata(&memo_file).unwrap().ino();

    // Re-saving without a new put must leave the file untouched.
    memo.save();
    assert_eq!(
        fs::metadata(&memo_file).unwrap().ino(),
        inode_after_first,
        "a clean save must not rewrite the memo file"
    );
}

/// `FontHashMemo::save` with a bare filename (no directory component) must save to the current
/// working directory without `ENOENT`.
#[test]
fn test_font_hash_memo_save_bare_filename() {
    let filename = format!("test_bare_font_memo_{}.json", std::process::id());
    let path = PathBuf::from(&filename);
    let mut memo = FontHashMemo::new(path.clone());
    let sig = FileSignature {
        size: 100,
        mtime_ns: 12345,
    };
    memo.put(Path::new("dummy.ttf"), sig, "hash123".to_string());
    memo.save();

    let exists = path.exists();
    let _ = fs::remove_file(&path);
    assert!(exists, "saving bare filename should succeed without ENOENT");
}

#[test]
fn test_scan_fonts() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    fs::write(fonts_dir.join("b_font.ttf"), b"b").unwrap();
    fs::write(fonts_dir.join("a_font.otf"), b"a").unwrap();
    fs::write(fonts_dir.join("readme.txt"), b"ignore").unwrap();

    let info = scan_fonts(&fonts_dir, None);
    assert_eq!(info.count, 2);
    // natural sort order: a_font.otf before b_font.ttf
    assert_eq!(info.args[0], "--attachment-mime-type");
    assert_eq!(info.args[1], "application/vnd.ms-opentype");
    assert_eq!(info.args[2], "--attach-file");
    assert!(info.args[3].ends_with("a_font.otf"));
}

#[test]
fn test_find_fonts_dir() {
    let tmp = tempdir().unwrap();
    let input_dir = tmp.path().join("release");
    let fonts = input_dir.join("fonts");
    fs::create_dir_all(&fonts).unwrap();

    let mut cache = HashMap::new();
    let found = find_fonts_dir(&input_dir, None, Some(&mut cache));
    assert_eq!(found, fonts);

    // Override takes precedence
    let override_dir = tmp.path().join("custom_fonts");
    let found_override = find_fonts_dir(&input_dir, Some(&override_dir), None);
    assert_eq!(found_override, override_dir);
}

#[test]
fn test_concurrent_font_claims_and_promotions() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = Arc::new(FontRegistry::new(fonts_dir.clone(), None));

    let mut handles = Vec::new();
    for i in 0..10 {
        let mgr_clone = Arc::clone(&mgr);
        handles.push(thread::spawn(move || {
            let font_name = format!("Font_{i}.ttf");
            if let Some(mut claim) = mgr_clone.claim(&font_name, 100) {
                let content = format!("content {i}");
                fs::write(&claim.staging_path, content.as_bytes()).unwrap();
                let res = mgr_clone.promote(&mut claim);
                assert!(res.is_some());
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(mgr.claimed_hashes().len(), 10);
}

#[test]
fn test_font_claims_guard_drop_releases_and_deletes() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let staging_path_1;
    let staging_path_2;

    {
        let mut guard = FontClaimsGuard::new(&mgr);
        assert!(guard.is_empty());
        assert_eq!(guard.len(), 0);

        let claim1 = mgr.claim("GuardFont1.ttf", 500).unwrap();
        staging_path_1 = claim1.staging_path.clone();
        fs::write(&staging_path_1, b"font 1 data").unwrap();
        assert!(staging_path_1.exists());
        guard.push(1, claim1);

        let claim2 = mgr.claim("GuardFont2.otf", 700).unwrap();
        staging_path_2 = claim2.staging_path.clone();
        fs::write(&staging_path_2, b"font 2 data").unwrap();
        assert!(staging_path_2.exists());
        guard.push(2, claim2);

        assert!(!guard.is_empty());
        assert_eq!(guard.len(), 2);
        assert_eq!(guard.claims().len(), 2);

        // Name and size are reserved while claim is active
        assert!(mgr.claim("GuardFont1.ttf", 500).is_none());
        assert!(mgr.claim("GuardFont2.otf", 700).is_none());

        // Guard dropped here without calling defuse()
    }

    // After drop without defuse():
    // 1. Reservations in claimed_name_size must be released (can be re-claimed)
    let reclaim1 = mgr.claim("GuardFont1.ttf", 500);
    assert!(reclaim1.is_some());
    let reclaim2 = mgr.claim("GuardFont2.otf", 700);
    assert!(reclaim2.is_some());

    // 2. Staging files must be deleted
    assert!(!staging_path_1.exists());
    assert!(!staging_path_2.exists());
}

#[test]
fn test_font_claims_guard_defuse_preserves_claims() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let staging_path;

    {
        let mut guard = FontClaimsGuard::new(&mgr);
        let claim = mgr.claim("PreservedFont.ttf", 300).unwrap();
        staging_path = claim.staging_path.clone();
        fs::write(&staging_path, b"preserved data").unwrap();
        guard.push(1, claim);

        guard.defuse();
        // Guard dropped with defuse() called
    }

    // Staging file should still exist and reservation should NOT be released
    assert!(staging_path.exists());
    assert!(mgr.claim("PreservedFont.ttf", 300).is_none());
}

#[test]
fn test_font_claims_guard_drain_claims() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let mut guard = FontClaimsGuard::new(&mgr);
    let claim = mgr.claim("DrainedFont.ttf", 400).unwrap();
    guard.push(1, claim);

    let drained: Vec<_> = guard.drain_claims().collect();
    assert_eq!(drained.len(), 1);
    assert!(guard.is_empty());
}

#[test]
fn test_font_claims_guard_pop() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontRegistry::new(fonts_dir, None);

    let mut guard = FontClaimsGuard::new(&mgr);
    let claim = mgr.claim("PopFont.ttf", 400).unwrap();
    guard.push(1, claim);

    let popped = guard.pop();
    assert!(popped.is_some());
    assert_eq!(popped.unwrap().0, 1);
    assert!(guard.is_empty());
    assert!(guard.pop().is_none());
}
