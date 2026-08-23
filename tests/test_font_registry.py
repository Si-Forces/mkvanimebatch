"""
Tests for FontManager thread-safety, transaction lifecycle, deduplication, collision safety, and rollback.
"""
import importlib
import sys
import threading
from pathlib import Path

import pytest

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

