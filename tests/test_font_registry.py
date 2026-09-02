"""
Tests for FontManager thread-safety, transaction lifecycle, deduplication, collision safety, and rollback.
"""
import sys
import threading
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from mkv_fonts import FontClaim, FontManager


class TestFontManagerLifecycle:
    """Tests for the FontManager claim -> promote -> release lifecycle."""

    def test_claim_and_promote(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        claim = mgr.claim("Arial.ttf", 100)
        assert claim is not None
        assert isinstance(claim, FontClaim)
        assert claim.name == "Arial.ttf"
        assert claim.staging_path.parent.name == ".font_staging"

        # Staging file is created by worker/mkvextract
        claim.staging_path.write_bytes(b"unique font content 1")

        promoted = mgr.promote(claim)
        assert promoted is not None
        assert promoted == fonts_dir / "Arial.ttf"
        assert promoted.read_bytes() == b"unique font content 1"
        assert not claim.staging_path.exists()

    def test_claim_duplicate_name_size_rejected(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        claim1 = mgr.claim("Arial.ttf", 100)
        assert claim1 is not None

        claim2 = mgr.claim("Arial.ttf", 100)
        assert claim2 is None

    def test_claim_different_size_allowed(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        claim1 = mgr.claim("Arial.ttf", 100)
        claim2 = mgr.claim("Arial.ttf", 200)
        assert claim1 is not None
        assert claim2 is not None

    def test_release_allows_reclaim(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        claim1 = mgr.claim("Arial.ttf", 100)
        assert claim1 is not None
        claim1.staging_path.write_bytes(b"temp")

        mgr.release(claim1)
        assert not claim1.staging_path.exists()

        # Should be re-claimable now
        claim2 = mgr.claim("Arial.ttf", 100)
        assert claim2 is not None

    def test_promote_hash_deduplication(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        # First font
        claim1 = mgr.claim("FontA.ttf", 100)
        assert claim1 is not None
        claim1.staging_path.write_bytes(b"identical font content")
        promoted1 = mgr.promote(claim1)
        assert promoted1 == fonts_dir / "FontA.ttf"

        # Second font with different name and size but same content
        claim2 = mgr.claim("FontB.ttf", 200)
        assert claim2 is not None
        claim2.staging_path.write_bytes(b"identical font content")
        promoted2 = mgr.promote(claim2)

        # Content duplicate should be deduplicated (return None, staging file cleaned up)
        assert promoted2 is None
        assert not claim2.staging_path.exists()
        assert not (fonts_dir / "FontB.ttf").exists()

    def test_collision_safe_target_allocation(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        fonts_dir.mkdir(parents=True)
        existing_font = fonts_dir / "Arial.ttf"
        existing_font.write_bytes(b"existing content")

        mgr = FontManager(fonts_dir)

        # New font with same filename but different content and size
        claim = mgr.claim("Arial.ttf", 999)
        assert claim is not None
        claim.staging_path.write_bytes(b"new different content")

        promoted = mgr.promote(claim)
        assert promoted is not None
        assert promoted != existing_font
        assert promoted.name.startswith("Arial_")
        assert promoted.name.endswith(".ttf")
        assert promoted.read_bytes() == b"new different content"
        # Original file is preserved untouched
        assert existing_font.read_bytes() == b"existing content"

    def test_seed_existing_fonts(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        fonts_dir.mkdir(parents=True)
        existing_font = fonts_dir / "Existing.otf"
        existing_font.write_bytes(b"pre-existing content")

        mgr = FontManager(fonts_dir)

        # Name and size should already be reserved
        assert mgr.claim("Existing.otf", len(b"pre-existing content")) is None

        # Content hash should already be known
        claim = mgr.claim("DifferentName.otf", 500)
        assert claim is not None
        claim.staging_path.write_bytes(b"pre-existing content")
        assert mgr.promote(claim) is None

    def test_empty_staging_file_promotion_fails(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        claim = mgr.claim("Empty.ttf", 0)
        assert claim is not None
        claim.staging_path.write_bytes(b"")  # 0 bytes

        assert mgr.promote(claim) is None
        assert not claim.staging_path.exists()

    def test_thread_safety_claim(self, tmp_path):
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        claims = []
        lock = threading.Lock()

        def try_claim():
            c = mgr.claim("Concurrent.ttf", 123)
            if c is not None:
                with lock:
                    claims.append(c)

        threads = [threading.Thread(target=try_claim) for _ in range(50)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(claims) == 1

    def test_shared_manager_hammered_across_threads(self, tmp_path):
        """One manager shared by many threads (as in the global extraction
        queue): concurrent claims of identical and distinct fonts must yield
        no lost and no duplicate promotions."""
        fonts_dir = tmp_path / "fonts"
        mgr = FontManager(fonts_dir)

        n_threads = 8
        contents = {f"Font{i}.ttf": f"font-bytes-{i}".encode() * 4 for i in range(6)}
        dup_bytes = b"duplicate-font-bytes"

        promoted = []
        lock = threading.Lock()
        barrier = threading.Barrier(n_threads)

        def worker(thread_id):
            barrier.wait(timeout=10)
            # Identical fonts: every thread races for the same (name, size).
            for name, data in contents.items():
                claim = mgr.claim(name, len(data))
                if claim is None:
                    continue
                claim.staging_path.write_bytes(data)
                path = mgr.promote(claim)
                if path is not None:
                    with lock:
                        promoted.append(path)
            # Distinct names with identical bytes: promote must dedup by hash.
            claim = mgr.claim(f"Dup{thread_id}.ttf", len(dup_bytes))
            if claim is not None:
                claim.staging_path.write_bytes(dup_bytes)
                path = mgr.promote(claim)
                if path is not None:
                    with lock:
                        promoted.append(path)

        threads = [threading.Thread(target=worker, args=(t,)) for t in range(n_threads)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # One promotion per unique content: 6 distinct fonts + 1 dup winner.
        assert len(promoted) == len(contents) + 1
        on_disk = [p for p in fonts_dir.iterdir() if p.is_file()]
        assert sorted(p.name for p in on_disk) == sorted(p.name for p in promoted)
        assert len({p.read_bytes() for p in on_disk}) == len(contents) + 1

        # Losing claims cleaned their staging files, so staging is removable.
        mgr.cleanup_staging()
        assert not (fonts_dir / ".font_staging").exists()


def test_font_hash_memo_save_permissions(tmp_path):
    import os
    import stat

    from mkv_fonts import FontHashMemo

    memo_path = tmp_path / "sub" / "memo.json"
    memo = FontHashMemo(memo_path)
    dummy_file = tmp_path / "dummy.ttf"
    dummy_file.write_bytes(b"test font")
    memo.put(dummy_file, dummy_file.stat(), "hash123")
    memo.save()

    assert memo_path.exists()
    if sys.platform != "win32":
        mode = stat.S_IMODE(memo_path.stat().st_mode)
        assert mode == 0o644

        # Overwrite existing memo with custom permission (e.g. 0o640)
        os.chmod(memo_path, 0o640)
        memo.put(dummy_file, dummy_file.stat(), "hash456")
        memo.save()
        mode_after = stat.S_IMODE(memo_path.stat().st_mode)
        assert mode_after == 0o640
