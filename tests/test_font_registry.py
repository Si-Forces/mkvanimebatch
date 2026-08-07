"""
Tests for FontRegistry thread-safety.

mkv_common.FontRegistry   — hash-based + name/size dedup
mkv_common_fast.FontRegistry — name/size dedup only

The two registries have different interfaces, so they are tested separately
rather than through the parametrized `mod` fixture.
"""
import threading
import importlib
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

mkv_common      = importlib.import_module("mkv_common")
mkv_common_fast = importlib.import_module("mkv_common_fast")


# ─────────────────────────────────────────────────────────────────────────────
# mkv_common.FontRegistry  (hash + name/size)
# ─────────────────────────────────────────────────────────────────────────────

class TestFontRegistryNormal:
    """Tests for the standard FontRegistry in mkv_common."""

    def _reg(self, existing=None):
        return mkv_common.FontRegistry(existing=existing)

    def test_name_size_claim_first_time(self):
        reg = self._reg()
        assert reg.try_claim_name_size("font.ttf", 1234) is True

    def test_name_size_claim_second_time_fails(self):
        reg = self._reg()
        reg.try_claim_name_size("font.ttf", 1234)
        assert reg.try_claim_name_size("font.ttf", 1234) is False

    def test_different_size_same_name_allowed(self):
        reg = self._reg()
        assert reg.try_claim_name_size("font.ttf", 1234) is True
        assert reg.try_claim_name_size("font.ttf", 5678) is True

    def test_hash_claim_first_time(self):
        reg = self._reg()
        assert reg.try_claim_hash("abc123") is True

    def test_hash_claim_duplicate_fails(self):
        reg = self._reg()
        reg.try_claim_hash("abc123")
        assert reg.try_claim_hash("abc123") is False

    def test_hash_claim_different_hashes_allowed(self):
        reg = self._reg()
        assert reg.try_claim_hash("abc123") is True
        assert reg.try_claim_hash("def456") is True

    def test_release_name_size_allows_reclaim(self):
        reg = self._reg()
        reg.try_claim_name_size("font.ttf", 1234)
        reg.release_name_size("font.ttf", 1234)
        assert reg.try_claim_name_size("font.ttf", 1234) is True

    def test_existing_hashes_pre_seeded(self):
        existing = {"deadbeef"}
        reg = self._reg(existing=existing)
        # Pre-seeded hash must be rejected
        assert reg.try_claim_hash("deadbeef") is False
        # New hash must be accepted
        assert reg.try_claim_hash("cafebabe") is True

    def test_claimed_hashes_property(self):
        reg = self._reg()
        reg.try_claim_hash("aaa")
        reg.try_claim_hash("bbb")
        assert {"aaa", "bbb"}.issubset(reg.claimed_hashes)

    def test_thread_safety_name_size(self):
        """Only one thread should win the first claim for a given key."""
        reg = self._reg()
        wins = []
        lock = threading.Lock()

        def claim():
            result = reg.try_claim_name_size("shared.ttf", 999)
            if result:
                with lock:
                    wins.append(1)

        threads = [threading.Thread(target=claim) for _ in range(50)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(wins) == 1, f"Expected exactly 1 winner, got {len(wins)}"

    def test_thread_safety_hash(self):
        """Only one thread should win the first hash claim."""
        reg = self._reg()
        wins = []
        lock = threading.Lock()

        def claim():
            result = reg.try_claim_hash("sharedHash")
            if result:
                with lock:
                    wins.append(1)

        threads = [threading.Thread(target=claim) for _ in range(50)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(wins) == 1, f"Expected exactly 1 winner, got {len(wins)}"


# ─────────────────────────────────────────────────────────────────────────────
# mkv_common_fast.FontRegistry  (name/size only, no hash)
# ─────────────────────────────────────────────────────────────────────────────

