use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::thread;

use mkv_core::fonts::{find_fonts_dir, hash_file, scan_fonts, FontHashMemo, FontManager};
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
    let mgr = FontManager::new(fonts_dir.clone(), None);

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
    let mgr = FontManager::new(fonts_dir, None);

    let claim1 = mgr.claim("Arial.ttf", 100);
    assert!(claim1.is_some());

    let claim2 = mgr.claim("Arial.ttf", 100);
    assert!(claim2.is_none());
}

#[test]
fn test_claim_different_size_allowed() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontManager::new(fonts_dir, None);

    let claim1 = mgr.claim("Arial.ttf", 100);
    let claim2 = mgr.claim("Arial.ttf", 200);
    assert!(claim1.is_some());
    assert!(claim2.is_some());
}

#[test]
fn test_release_allows_reclaim() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontManager::new(fonts_dir, None);

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
    let mgr = FontManager::new(fonts_dir.clone(), None);

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
fn test_collision_safe_target_allocation() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    fs::create_dir_all(&fonts_dir).unwrap();

    let existing_font = fonts_dir.join("Arial.ttf");
    fs::write(&existing_font, b"existing content").unwrap();

    let mgr = FontManager::new(fonts_dir.clone(), None);

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

    let mgr = FontManager::new(fonts_dir, None);

    // Name and size should already be reserved
    assert!(mgr.claim("Existing.otf", size).is_none());

    // Content duplicate should also be rejected upon promote
    let mut claim = mgr.claim("Other.otf", 50).unwrap();
    fs::write(&claim.staging_path, b"pre-existing content").unwrap();
    assert!(mgr.promote(&mut claim).is_none());
}

#[test]
fn test_cleanup_staging() {
    let tmp = tempdir().unwrap();
    let fonts_dir = tmp.path().join("fonts");
    let mgr = FontManager::new(fonts_dir.clone(), None);

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
        memo.put(
            &tmp.path().join("f1.ttf"),
            100,
            12345,
            "hash123".to_string(),
        );
        memo.save();
    }

    {
        let memo = FontHashMemo::new(memo_file);
        assert_eq!(
            memo.get(&tmp.path().join("f1.ttf"), 100, 12345),
            Some("hash123".to_string())
        );
        assert_eq!(memo.get(&tmp.path().join("f1.ttf"), 100, 99999), None);
        assert_eq!(memo.get(&tmp.path().join("f2.ttf"), 100, 12345), None);
    }
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
    let mgr = Arc::new(FontManager::new(fonts_dir.clone(), None));

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
