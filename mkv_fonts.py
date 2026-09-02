"""
mkv_fonts.py
Font discovery plus the thread-safe transactional FontManager:
claim -> stage -> deduplicate -> promote (or release).
"""
from __future__ import annotations

import json
import logging
import os
import stat
import tempfile
import threading
import uuid
from dataclasses import dataclass
from pathlib import Path

from mkv_domain import FONT_EXTENSIONS, FONT_MIME, natural_sort_key, safe_filename
from mkv_proc import hash_file

FONT_DIR_NAMES = frozenset({"fonts", "font", "attachments"})

logger = logging.getLogger("mkv_automation")

@dataclass(frozen=True)
class FontInfo:
    args:  tuple[str, ...]
    count: int


def scan_fonts(fonts_dir: Path) -> FontInfo:
    """Scan a fonts directory and build mkvmerge attachment arguments."""
    font_args: list[str] = []
    count = 0
    if not fonts_dir.is_dir():
        return FontInfo(args=(), count=0)

    try:
        entries = []
        with os.scandir(str(fonts_dir)) as it:
            for entry in it:
                if entry.is_file():
                    entries.append(entry)
        entries.sort(key=lambda e: natural_sort_key(e.name))
        for entry in entries:
            suffix = Path(entry.name).suffix.lower()
            mime = FONT_MIME.get(suffix)
            if not mime:
                continue
            font_args += ["--attachment-mime-type", mime, "--attach-file", entry.path]
            count += 1
    except OSError:
        pass
    return FontInfo(args=tuple(font_args), count=count)

def find_fonts_dir(input_dir: Path, override: Path | None = None, fonts_dir_cache: dict[Path, Path] | None = None) -> Path:
    """
    Resolve which directory to use for fonts.
    Priority:
      1. override (e.g. from --fonts-dir)
      2. input_dir / "fonts"
      3. Any top-level directory matching fonts/font/attachments
      4. Falls back to input_dir / "fonts"
    """
    if override is not None:
        return override

    if fonts_dir_cache is not None and input_dir in fonts_dir_cache:
        return fonts_dir_cache[input_dir]

    def _store(path: Path) -> Path:
        if fonts_dir_cache is not None:
            fonts_dir_cache[input_dir] = path
        return path

    exact = input_dir / "fonts"
    if exact.is_dir():
        return _store(exact)

    if input_dir.is_dir():
        for entry in sorted(input_dir.iterdir()):
            if entry.is_dir() and entry.name.lower() in FONT_DIR_NAMES:
                return _store(entry)

    return _store(exact)

# ─── FONT MANAGER (Thread-safe deduplication & atomic promotion) ─────────────
class FontHashMemo:
    """Persistent blake2b memo for font files keyed by (path, size, mtime_ns).

    Lets repeated runs skip re-reading and re-hashing unchanged fonts during
    seeding. Not thread-safe; intended for single-threaded seed passes.
    """

    def __init__(self, memo_file: Path) -> None:
        self.memo_file = memo_file
        self._data: dict[str, dict] = {}
        self._dirty = False
        self._load()

    def _load(self) -> None:
        if not self.memo_file.exists():
            return
        try:
            with open(self.memo_file, "r", encoding="utf-8") as f:
                loaded = json.load(f)
            if isinstance(loaded, dict):
                self._data = loaded
        except (OSError, json.JSONDecodeError, TypeError, ValueError) as e:
            logger.warning(f"Failed to load font hash memo from {self.memo_file}: {e}")
            self._data = {}

    def get(self, path: Path, st: os.stat_result) -> str | None:
        entry = self._data.get(str(path))
        if (
            isinstance(entry, dict)
            and entry.get("size") == st.st_size
            and entry.get("mtime_ns") == st.st_mtime_ns
        ):
            digest = entry.get("hash")
            if isinstance(digest, str):
                return digest
        return None

    def put(self, path: Path, st: os.stat_result, digest: str) -> None:
        entry = {"size": st.st_size, "mtime_ns": st.st_mtime_ns, "hash": digest}
        key = str(path)
        if self._data.get(key) != entry:
            self._data[key] = entry
            self._dirty = True

    def save(self) -> None:
        if not self._dirty:
            return
        temp_name = ""
        try:
            self.memo_file.parent.mkdir(parents=True, exist_ok=True)
            try:
                with tempfile.NamedTemporaryFile(
                    "w", dir=self.memo_file.parent, encoding="utf-8", delete=False
                ) as temp_file:
                    temp_name = temp_file.name
                    json.dump(self._data, temp_file)
                if self.memo_file.exists():
                    os.chmod(temp_name, stat.S_IMODE(self.memo_file.stat().st_mode))
                else:
                    os.chmod(temp_name, 0o644)
                Path(temp_name).replace(self.memo_file)
            except BaseException:
                if temp_name:
                    Path(temp_name).unlink(missing_ok=True)
                raise
            self._dirty = False
        except (OSError, TypeError, ValueError) as e:
            logger.warning(f"Failed to save font hash memo to {self.memo_file}: {e}")


@dataclass
class FontClaim:
    name: str
    size: int
    staging_path: Path
    identity: tuple[str, int]
    _promoted: bool = False
    _released: bool = False


class FontManager:
    """
    Thread-safe manager configured with a fonts destination directory for font extraction,
    hash/name deduplication, collision safety, atomic promotion, and safe rollback.
    """

    def __init__(self, fonts_dir: Path, hash_memo: FontHashMemo | None = None) -> None:
        self.fonts_dir = fonts_dir
        self._hash_memo = hash_memo
        self._claimed_hashes: set[str] = set()
        self._claimed_name_size: set[tuple[str, int]] = set()
        self._staging_dir = self.fonts_dir / ".font_staging"
        self._lock = threading.Lock()
        self._seed_existing()

    def _seed_existing(self) -> None:
        if not self.fonts_dir.is_dir():
            return
        try:
            with os.scandir(str(self.fonts_dir)) as it:
                for entry in it:
                    try:
                        if not entry.is_file():
                            continue
                        p = Path(entry.path)
                        if p.suffix.lower() not in FONT_EXTENSIONS:
                            continue
                        st = entry.stat()
                        self._claimed_name_size.add((entry.name, st.st_size))
                        if st.st_size > 0:
                            file_hash = None
                            if self._hash_memo is not None:
                                file_hash = self._hash_memo.get(p, st)
                            if file_hash is None:
                                file_hash = hash_file(p)
                                if self._hash_memo is not None:
                                    self._hash_memo.put(p, st, file_hash)
                            self._claimed_hashes.add(file_hash)
                    except OSError:
                        pass
        except OSError:
            pass

    @property
    def claimed_hashes(self) -> set[str]:
        with self._lock:
            return set(self._claimed_hashes)

    def claim(self, name: str, size: int) -> FontClaim | None:
        """
        Atomically reserve a font by sanitized name and size.
        Returns a FontClaim with a manager-created same-filesystem staging path, or None if already claimed.
        """
        safe_name = safe_filename(name)
        key = (safe_name, size)
        with self._lock:
            if key in self._claimed_name_size:
                return None
            self._staging_dir.mkdir(parents=True, exist_ok=True)
            self._claimed_name_size.add(key)
            unique_id = uuid.uuid4().hex
            staging_path = self._staging_dir / f".tmp_{unique_id}_{safe_name}"

        return FontClaim(
            name=safe_name,
            size=size,
            staging_path=staging_path,
            identity=key,
        )

    def promote(self, claim: FontClaim) -> Path | None:
        """
        Validate, hash, deduplicate, and atomically promote a staged font file to fonts_dir.
        If hash is duplicate or validation fails, staging file is removed and released.
        Returns the final promoted destination Path, or None if duplicate or failed.
        """
        if claim._promoted:
            return None

        def _abandon_locked() -> None:
            try:
                claim.staging_path.unlink(missing_ok=True)
            except OSError:
                pass
            self._claimed_name_size.discard(claim.identity)
            claim._released = True

        try:
            if not claim.staging_path.is_file() or claim.staging_path.stat().st_size == 0:
                self.release(claim)
                return None
            file_hash = hash_file(claim.staging_path)
        except OSError:
            self.release(claim)
            return None

        with self._lock:
            if file_hash in self._claimed_hashes:
                _abandon_locked()
                return None

            try:
                self.fonts_dir.mkdir(parents=True, exist_ok=True)
                target_path = self.fonts_dir / claim.name
                if target_path.exists():
                    stem = target_path.stem
                    suffix = target_path.suffix
                    target_path = self.fonts_dir / f"{stem}_{file_hash[:8]}{suffix}"
                    counter = 1
                    while target_path.exists():
                        target_path = self.fonts_dir / f"{stem}_{file_hash[:8]}_{counter}{suffix}"
                        counter += 1
                claim.staging_path.replace(target_path)
                self._claimed_hashes.add(file_hash)
                claim._promoted = True
                return target_path
            except OSError as e:
                logger.error("Failed to promote font '%s': %s", claim.staging_path.name, e)
                _abandon_locked()
                return None

    def release(self, claim: FontClaim | None) -> None:
        """
        Idempotently release an uncommitted claim and clean up its staging path.
        Never removes committed/promoted files.
        """
        if claim is None or claim._promoted or claim._released:
            return

        with self._lock:
            self._claimed_name_size.discard(claim.identity)
            try:
                claim.staging_path.unlink(missing_ok=True)
            except OSError:
                pass
            claim._released = True

    def cleanup_staging(self) -> None:
        """Remove staging directory if empty."""
        with self._lock:
            if self._staging_dir.is_dir():
                try:
                    self._staging_dir.rmdir()
                except OSError:
                    pass
