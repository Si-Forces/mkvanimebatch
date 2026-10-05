use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use mkv_core::fonts::{
    find_fonts_dir, hash_file, scan_fonts, FontClaimsGuard, FontHashMemo, FontRegistry,
};
use mkv_core::scan::FileSignature;
use tempfile::{tempdir, TempDir};

const SIG: FileSignature = FileSignature {
    size: 100,
    mtime_ns: 12345,
};

/// A registry over `<tmp>/fonts`, which does not exist yet.
fn fresh_registry() -> (TempDir, PathBuf, FontRegistry) {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let registry = FontRegistry::new(fonts_dir.clone(), None);
    (tmp, fonts_dir, registry)
}

/// An existing, empty `<tmp>/fonts` to populate before constructing a registry over it.
fn existing_fonts_dir() -> (TempDir, PathBuf) {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();
    (tmp, fonts_dir)
}

fn shared_memo(tmp: &TempDir) -> Arc<parking_lot::Mutex<FontHashMemo>> {
    Arc::new(parking_lot::Mutex::new(FontHashMemo::new(
        tmp.path().join("memo.json"),
    )))
}

/// Claims `name` at the length of `content`, stages `content`, and promotes it.
fn stage_and_promote(registry: &FontRegistry, name: &str, content: &[u8]) -> Option<PathBuf> {
    let mut claim = registry.claim(name, content.len() as u64)?;
    fs::write(&claim.staging_path, content).unwrap();
    registry.promote(&mut claim)
}

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
    let (_tmp, fonts_dir, mgr) = fresh_registry();

    let mut claim = mgr.claim("Arial.ttf", 100).expect("claim should succeed");
    assert_eq!(claim.name(), "Arial.ttf");
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
    let (_tmp, _fonts_dir, mgr) = fresh_registry();

    assert!(mgr.claim("Arial.ttf", 100).is_some());
    assert!(mgr.claim("Arial.ttf", 100).is_none());
}

#[test]
fn test_claim_different_size_allowed() {
    let (_tmp, _fonts_dir, mgr) = fresh_registry();

    assert!(mgr.claim("Arial.ttf", 100).is_some());
    assert!(mgr.claim("Arial.ttf", 200).is_some());
}

#[test]
fn test_release_allows_reclaim() {
    let (_tmp, _fonts_dir, mgr) = fresh_registry();

    let mut claim1 = mgr.claim("Arial.ttf", 100).unwrap();
    fs::write(&claim1.staging_path, b"temp").unwrap();

    mgr.release(&mut claim1);
    assert!(!claim1.staging_path.exists());
    assert!(claim1.is_released());

    assert!(mgr.claim("Arial.ttf", 100).is_some());
}

#[test]
fn test_promote_hash_deduplication_preserves_name_size_claim() {
    let (_tmp, fonts_dir, mgr) = fresh_registry();

    let mut claim1 = mgr.claim("FontA.ttf", 100).unwrap();
    fs::write(&claim1.staging_path, b"shared font data").unwrap();
    assert_eq!(mgr.promote(&mut claim1), Some(fonts_dir.join("FontA.ttf")));

    // Different name and size, identical content.
    let mut claim2 = mgr.claim("FontB.ttf", 200).unwrap();
    fs::write(&claim2.staging_path, b"shared font data").unwrap();
    assert!(mgr.promote(&mut claim2).is_none());
    assert!(!claim2.staging_path.exists());
    assert!(!fonts_dir.join("FontB.ttf").exists());

    assert!(
        mgr.claim("FontB.ttf", 200).is_none(),
        "subsequent claim of hash-deduplicated font should be skipped via claimed_name_size"
    );
}

#[test]
fn test_collision_safe_target_allocation() {
    let (_tmp, fonts_dir) = existing_fonts_dir();
    let existing_font = fonts_dir.join("Arial.ttf");
    fs::write(&existing_font, b"existing content").unwrap();

    let mgr = FontRegistry::new(fonts_dir, None);

    // Same filename, different content and size.
    let mut claim = mgr.claim("Arial.ttf", 999).unwrap();
    fs::write(&claim.staging_path, b"new different content").unwrap();

    let promoted = mgr.promote(&mut claim).expect("promote should succeed");
    assert_ne!(promoted, existing_font);
    let promoted_name = promoted.file_name().unwrap().to_string_lossy();
    assert!(promoted_name.starts_with("Arial_"));
    assert!(promoted_name.ends_with(".ttf"));
    assert_eq!(fs::read(&promoted).unwrap(), b"new different content");

    assert_eq!(fs::read(&existing_font).unwrap(), b"existing content");
}

#[test]
fn test_seed_existing_fonts() {
    let (_tmp, fonts_dir) = existing_fonts_dir();
    let existing_font = fonts_dir.join("Existing.otf");
    fs::write(&existing_font, b"pre-existing content").unwrap();
    let size = fs::metadata(&existing_font).unwrap().len();

    let mgr = FontRegistry::new(fonts_dir, None);

    assert!(mgr.claim("Existing.otf", size).is_none());

    let mut claim = mgr.claim("Other.otf", 50).unwrap();
    fs::write(&claim.staging_path, b"pre-existing content").unwrap();
    assert!(mgr.promote(&mut claim).is_none());
}

/// Seeding reads metadata only (#33). A seeded font is hashed, through the memo, only when a
/// staged font of its size is promoted; fonts of other sizes stay unhashed.
#[test]
fn test_seed_existing_hashes_only_the_promoted_size() {
    let (tmp, fonts_dir) = existing_fonts_dir();
    let small = fonts_dir.join("Small.otf");
    fs::write(&small, b"small font").unwrap();
    let large = fonts_dir.join("Large.otf");
    fs::write(&large, b"a larger font payload").unwrap();
    let small_sig = FileSignature::of(&small).unwrap();
    let large_sig = FileSignature::of(&large).unwrap();

    let memo = shared_memo(&tmp);
    let mgr = FontRegistry::new(fonts_dir, Some(Arc::clone(&memo)));
    assert_eq!(
        memo.lock().get(&small, small_sig),
        None,
        "seeding must not hash"
    );
    assert_eq!(
        memo.lock().get(&large, large_sig),
        None,
        "seeding must not hash"
    );

    // Same size as Small.otf, different name and content.
    assert!(stage_and_promote(&mgr, "Incoming.otf", b"other font").is_some());

    let small_hash = hash_file(&small).unwrap();
    assert_eq!(memo.lock().get(&small, small_sig), Some(small_hash.clone()));
    assert_eq!(
        memo.lock().get(&large, large_sig),
        None,
        "a seeded font of another size must stay unhashed"
    );
    let claimed = mgr.claimed_hashes();
    assert_eq!(claimed.len(), 2, "Small.otf's hash and the promoted font's");
    assert!(claimed.contains(&small_hash));
}

/// Concurrent promotes of one size share a single hashing of that size's seeded fonts, and
/// none of them checks for duplicates before those hashes are in.
#[test]
fn test_concurrent_promotes_of_a_seeded_size_all_find_their_duplicates() {
    const THREADS: usize = 8;
    let (_tmp, fonts_dir) = existing_fonts_dir();
    let contents: [&[u8]; 2] = [b"seeded content A", b"seeded content B"];
    fs::write(fonts_dir.join("A.otf"), contents[0]).unwrap();
    fs::write(fonts_dir.join("B.otf"), contents[1]).unwrap();

    let mgr = Arc::new(FontRegistry::new(fonts_dir.clone(), None));
    let barrier = Arc::new(std::sync::Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|i| {
            let mgr = Arc::clone(&mgr);
            let barrier = Arc::clone(&barrier);
            let content = contents[i % 2];
            thread::spawn(move || {
                let mut claim = mgr.claim(&format!("Copy_{i}.otf"), 16).unwrap();
                fs::write(&claim.staging_path, content).unwrap();
                barrier.wait();
                mgr.promote(&mut claim)
            })
        })
        .collect();

    for h in handles {
        assert_eq!(h.join().unwrap(), None, "a seeded duplicate was promoted");
    }
    assert_eq!(mgr.claimed_hashes().len(), 2);
}

/// `seed_existing` derives each font's signature from the `DirEntry` metadata it already
/// holds, and a promote of the same size later hashes the font under it. That signature must
/// equal what a symlink-resolving `FileSignature::of` computes, otherwise every later memo
/// lookup would miss and fonts would be re-hashed on each run.
#[test]
fn test_seed_existing_records_memo_signature_matching_resolved_lookup() {
    let (tmp, fonts_dir) = existing_fonts_dir();
    let font = fonts_dir.join("Real.otf");
    fs::write(&font, b"real font content").unwrap();

    let memo = shared_memo(&tmp);
    let mgr = FontRegistry::new(fonts_dir, Some(Arc::clone(&memo)));
    // Seeded fonts are hashed lazily (#33): promote one of the same size to hash it.
    assert!(stage_and_promote(&mgr, "Other.otf", b"fake font content").is_some());

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

    let (tmp, fonts_dir) = existing_fonts_dir();
    let real = fonts_dir.join("Real.otf");
    fs::write(&real, b"real font content").unwrap();
    let link = fonts_dir.join("Linked.otf");
    symlink(&real, &link).unwrap();

    let memo = shared_memo(&tmp);
    let mgr = FontRegistry::new(fonts_dir, Some(Arc::clone(&memo)));
    // Seeded fonts are hashed lazily (#33): promote one of the same size to hash them.
    assert!(stage_and_promote(&mgr, "Other.otf", b"fake font content").is_some());

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

    let (_tmp, fonts_dir) = existing_fonts_dir();
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

    let (_tmp, fonts_dir) = existing_fonts_dir();
    let link = fonts_dir.join("Broken.otf");
    symlink(fonts_dir.join("Nonexistent.otf"), &link).unwrap();

    let mgr = FontRegistry::new(fonts_dir, None);

    assert!(
        mgr.claim("Broken.otf", 500).is_some(),
        "broken symlink must not poison claimed_name_size"
    );
}

/// A directory carrying a font extension, and a symlink to one, are not fonts. Neither may
/// enter `claimed_name_size`, and the directory's own `Metadata::len()` is the size that
/// would poison it — so a claim of exactly that name and length must still be granted.
#[cfg(unix)]
#[test]
fn test_seed_existing_directory_named_like_a_font_does_not_poison_claim() {
    use std::os::unix::fs::symlink;

    let (_tmp, fonts_dir) = existing_fonts_dir();
    let real_dir = fonts_dir.join("RealDir.otf");
    fs::create_dir_all(&real_dir).unwrap();
    let link = fonts_dir.join("Linked.otf");
    symlink(&real_dir, &link).unwrap();

    let mgr = FontRegistry::new(fonts_dir, None);

    let dir_len = fs::metadata(&real_dir).unwrap().len();
    assert!(
        mgr.claim("Linked.otf", dir_len).is_some(),
        "a symlink to a directory must not deny a claim of its name and the directory's byte length"
    );
    assert!(
        mgr.claim("RealDir.otf", dir_len).is_some(),
        "a directory must not deny a claim of its name and its own byte length"
    );
}

#[test]
fn test_cleanup_staging() {
    let (_tmp, fonts_dir, mgr) = fresh_registry();

    let mut claim = mgr.claim("Test.ttf", 100).unwrap();
    fs::write(&claim.staging_path, b"abc").unwrap();
    mgr.promote(&mut claim).unwrap();

    mgr.cleanup_staging();
    assert!(!fonts_dir.join(".font_staging").exists());
}

#[test]
fn test_font_hash_memo() {
    let tmp = tempdir().unwrap();
    let memo_file = tmp.path().join("font_memo.json");

    {
        let mut memo = FontHashMemo::new(memo_file.clone());
        memo.put(&tmp.path().join("f1.ttf"), SIG, "hash123".to_string());
        memo.save();
    }

    let memo = FontHashMemo::new(memo_file);
    let sig_diff_mtime = FileSignature {
        mtime_ns: 99999,
        ..SIG
    };
    assert_eq!(
        memo.get(&tmp.path().join("f1.ttf"), SIG),
        Some("hash123".to_string())
    );
    assert_eq!(memo.get(&tmp.path().join("f1.ttf"), sig_diff_mtime), None);
    assert_eq!(memo.get(&tmp.path().join("f2.ttf"), SIG), None);
}

/// `FontHashMemo::save` streams through a `BufWriter`, so a memo small enough to fit the
/// buffer never exercises a mid-stream flush. This pins that a memo far larger than the
/// 8 KiB buffer still serializes completely and round-trips.
#[test]
fn test_font_hash_memo_large_save_round_trips_through_buffered_writer() {
    const ENTRIES: usize = 500;
    let tmp = tempdir().unwrap();
    let memo_file = tmp.path().join("font_memo.json");
    let font_path = |i: usize| {
        tmp.path()
            .join(format!("font-with-a-longish-name-{i:04}.ttf"))
    };

    {
        let mut memo = FontHashMemo::new(memo_file.clone());
        for i in 0..ENTRIES {
            memo.put(&font_path(i), SIG, format!("{i:064x}"));
        }
        memo.save();
    }

    assert!(
        fs::metadata(&memo_file).unwrap().len() > 8192,
        "precondition: the memo must exceed the BufWriter capacity"
    );
    let memo = FontHashMemo::new(memo_file);
    for i in 0..ENTRIES {
        assert_eq!(
            memo.get(&font_path(i), SIG),
            Some(format!("{i:064x}")),
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

    let mut memo = FontHashMemo::new(memo_file.clone());
    memo.put(&tmp.path().join("f1.ttf"), SIG, "hash123".to_string());
    memo.save();
    let inode_after_first = fs::metadata(&memo_file).unwrap().ino();

    memo.save();
    assert_eq!(
        fs::metadata(&memo_file).unwrap().ino(),
        inode_after_first,
        "a clean save must not rewrite the memo file"
    );
}

/// Removes the memo on every exit path, including a failed assertion, so a red test never
/// strands a file in the crate directory.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// `FontHashMemo::save` with a bare filename (no directory component) must save to the current
/// working directory without `ENOENT`.
#[test]
fn test_font_hash_memo_save_bare_filename() {
    let path = PathBuf::from(format!("test_bare_font_memo_{}.json", std::process::id()));
    let _cleanup = RemoveOnDrop(path.clone());
    let mut memo = FontHashMemo::new(path.clone());
    memo.put(Path::new("dummy.ttf"), SIG, "hash123".to_string());
    memo.save();

    assert!(
        path.exists(),
        "saving bare filename should succeed without ENOENT"
    );
}

#[test]
fn test_scan_fonts() {
    let (_tmp, fonts_dir) = existing_fonts_dir();
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
    // An exact `fonts` directory beats an alternative name.
    fs::create_dir_all(input_dir.join("attachments")).unwrap();

    let mut cache = HashMap::new();
    assert_eq!(find_fonts_dir(&input_dir, None, Some(&mut cache)), fonts);

    let override_dir = tmp.path().join("custom_fonts");
    assert_eq!(
        find_fonts_dir(&input_dir, Some(&override_dir), None),
        override_dir
    );

    // Without `fonts`, alternative names match regardless of ASCII case, and a same-named
    // file is skipped even though it sorts first.
    let alt_dir = tmp.path().join("alt");
    fs::create_dir_all(alt_dir.join("FONT")).unwrap();
    fs::write(alt_dir.join("Attachments"), b"a file, not a directory").unwrap();
    assert_eq!(find_fonts_dir(&alt_dir, None, None), alt_dir.join("FONT"));

    // With no candidate at all, `fonts` is returned even though it does not exist.
    let bare_dir = tmp.path().join("bare");
    fs::create_dir_all(&bare_dir).unwrap();
    assert_eq!(
        find_fonts_dir(&bare_dir, None, None),
        bare_dir.join("fonts")
    );
}

#[test]
fn test_concurrent_font_claims_and_promotions() {
    let tmp = tempdir().unwrap();
    let mgr = Arc::new(FontRegistry::new(tmp.path().join("fonts"), None));

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let mgr = Arc::clone(&mgr);
            thread::spawn(move || {
                let mut claim = mgr.claim(&format!("Font_{i}.ttf"), 100).unwrap();
                fs::write(&claim.staging_path, format!("content {i}")).unwrap();
                assert!(mgr.promote(&mut claim).is_some());
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(mgr.claimed_hashes().len(), 10);
}

#[test]
fn test_font_claims_guard_drop_releases_and_deletes() {
    let (_tmp, _fonts_dir, mgr) = fresh_registry();

    let staging_path_1;
    let staging_path_2;

    {
        let mut guard = FontClaimsGuard::new(&mgr);

        let claim1 = mgr.claim("GuardFont1.ttf", 500).unwrap();
        staging_path_1 = claim1.staging_path.clone();
        fs::write(&staging_path_1, b"font 1 data").unwrap();
        guard.push(1, claim1);

        let claim2 = mgr.claim("GuardFont2.otf", 700).unwrap();
        staging_path_2 = claim2.staging_path.clone();
        fs::write(&staging_path_2, b"font 2 data").unwrap();
        guard.push(2, claim2);

        // Reserved while the guard holds the claims.
        assert!(mgr.claim("GuardFont1.ttf", 500).is_none());
        assert!(mgr.claim("GuardFont2.otf", 700).is_none());
    }

    assert!(mgr.claim("GuardFont1.ttf", 500).is_some());
    assert!(mgr.claim("GuardFont2.otf", 700).is_some());
    assert!(!staging_path_1.exists());
    assert!(!staging_path_2.exists());
}

#[test]
fn test_font_claims_guard_keeps_claims_taken_out_before_drop() {
    let (_tmp, _fonts_dir, mgr) = fresh_registry();

    let (_, claim) = {
        let mut guard = FontClaimsGuard::new(&mgr);
        let claim = mgr.claim("PoppedFont.ttf", 300).unwrap();
        fs::write(&claim.staging_path, b"popped data").unwrap();
        guard.push(1, claim);
        guard.pop().unwrap()
    };

    assert!(
        mgr.claim("PoppedFont.ttf", 300).is_none(),
        "dropping the guard must not release a claim already popped from it"
    );
    assert!(claim.staging_path.exists());
}
