"""
Tests for performance tuning changes: storage-based --io-jobs detection,
ffmpeg thread budgeting against effective concurrency, inspect-cache
syscall/prune semantics, and SubIndex precomputed scoring parity.
"""

import importlib
import json
import os
import stat
import sys
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

mkv_remux = importlib.import_module("mkv_remux")
mkv_runner = importlib.import_module("mkv_runner")
mkv_storage = importlib.import_module("mkv_storage")

from mkv_assets import SubIndex, _make_sub_entry  # noqa: E402
from mkv_domain import EpisodeInfo  # noqa: E402
from mkv_scan import PersistentInspectCache, _create_parsed_file  # noqa: E402

posix_only = pytest.mark.skipif(
    sys.platform == "win32",
    reason="exercises Linux-only machinery: sysfs trees, /proc mountinfo, os.major",
)


@posix_only
def test_device_key_uses_rdev_for_block_nodes():
    """Block special files must report their own device (st_rdev), not the
    device of the filesystem hosting /dev — otherwise backing-device fallback
    resolves to a devtmpfs id with no meaningful sysfs queue."""
    block_key = None
    for candidate in Path("/dev").iterdir():
        try:
            if stat.S_ISBLK(os.stat(candidate).st_mode):
                st = os.stat(candidate)
                expected = f"{os.major(st.st_rdev)}:{os.minor(st.st_rdev)}"
                block_key = mkv_storage._device_key(candidate)
                assert block_key == expected
                break
        except OSError:
            continue
    if block_key is None:
        pytest.skip("no block device node available in this environment")


def _runtime(**overrides):
    from mkv_scan import PersistentInspectCache as PC
    base = dict(
        workers=16,
        dry_run=True,
        io_semaphore=None,
        persistent_cache=PC(enabled=False),
    )
    base.update(overrides)
    return mkv_runner.CliRuntime(**base)


# ─── CliRuntime.effective_concurrency ─────────────────────────────────────────
def test_effective_concurrency_defaults_to_workers():
    assert _runtime(workers=16, io_jobs=None).effective_concurrency == 16


def test_effective_concurrency_capped_by_io_jobs():
    assert _runtime(workers=16, io_jobs=4).effective_concurrency == 4


def test_effective_concurrency_never_exceeds_workers():
    assert _runtime(workers=2, io_jobs=8).effective_concurrency == 2


# ─── ffmpeg thread budget ─────────────────────────────────────────────────────
def test_explicit_ffmpeg_threads_flag_wins(monkeypatch):
    seen = {}
    monkeypatch.setattr(mkv_remux, "default_ffmpeg_threads", lambda n: seen.setdefault("n", n))
    runtime = _runtime(workers=16, io_jobs=4)
    assert mkv_remux._effective_ffmpeg_threads(3, runtime) == 3
    assert seen == {}  # auto budget never consulted


def test_auto_ffmpeg_threads_divides_by_effective_concurrency(monkeypatch):
    seen = {}
    monkeypatch.setattr(mkv_remux, "default_ffmpeg_threads", lambda n: seen.setdefault("n", n))
    runtime = _runtime(workers=16, io_jobs=4)
    mkv_remux._effective_ffmpeg_threads(None, runtime)
    assert seen["n"] == 4


# ─── storage classification ───────────────────────────────────────────────────
@pytest.mark.parametrize("klass,expected", [
    ("ssd", mkv_storage.SSD_IO_JOBS),
    ("hdd", mkv_storage.HDD_IO_JOBS),
    ("network", mkv_storage.HDD_IO_JOBS),
    ("unknown", mkv_storage.HDD_IO_JOBS),
])
def test_default_io_jobs_by_storage_class(monkeypatch, klass, expected):
    monkeypatch.setattr(mkv_storage, "storage_class", lambda p: klass)
    assert mkv_storage.default_io_jobs(Path("/anywhere")) == expected


def test_resolve_io_jobs_flag_wins(monkeypatch):
    monkeypatch.setattr(mkv_storage, "default_io_jobs", lambda p: 99)
    assert mkv_storage.resolve_io_jobs(3, Path("/x")) == 3
    assert mkv_storage.resolve_io_jobs(None, Path("/x")) == 99


def _clear_storage_caches():
    mkv_storage._windows_class_cached.cache_clear()
    mkv_storage._DEV_CLASS_CACHE.clear()


@posix_only
def test_storage_class_detection_runs_once_per_filesystem(monkeypatch, tmp_path):
    """All paths on one backing filesystem share a single detection pass."""
    _clear_storage_caches()
    calls = []

    def counting_rotational(dev_id, sysfs_block=None):
        calls.append(dev_id)
        return "ssd"

    monkeypatch.setattr(mkv_storage, "_rotational_from_sysfs", counting_rotational)
    monkeypatch.setattr(mkv_storage.sys, "platform", "linux")
    sub = tmp_path / "sub"
    sub.mkdir()
    (tmp_path / "x.mkv").write_bytes(b"x")  # real files share one backing st_dev
    (sub / "y.mkv").write_bytes(b"y")
    mkv_storage.storage_class(tmp_path / "x.mkv")
    mkv_storage.storage_class(sub / "y.mkv")
    assert len(calls) == 1
    _clear_storage_caches()


def test_storage_class_distinguishes_separate_filesystems(tmp_path, monkeypatch):
    """Regression: classification must key on the backing device, not the
    drive anchor — on POSIX every path shares anchor '/', so multi-mount
    boxes (e.g. / on SSD, /mnt/hdd on HDD) would collapse to one answer."""
    _clear_storage_caches()
    calls = []

    def fake_device_key(path):
        return "8:0" if str(path).startswith(str(tmp_path / "ssd_root")) else "259:1"

    def fake_rotational(dev_id, sysfs_block=None):
        calls.append(dev_id)
        return "hdd" if dev_id == "259:1" else "ssd"

    ssd_dir = tmp_path / "ssd_root" / "deep"
    hdd_dir = tmp_path / "hdd_root" / "deep"
    for d in (ssd_dir, hdd_dir):
        d.mkdir(parents=True)

    monkeypatch.setattr(mkv_storage, "_device_key", fake_device_key)
    monkeypatch.setattr(mkv_storage, "_rotational_from_sysfs", fake_rotational)
    monkeypatch.setattr(mkv_storage.sys, "platform", "linux")

    assert mkv_storage.storage_class(ssd_dir) == "ssd"
    assert mkv_storage.storage_class(hdd_dir) == "hdd"
    # Repeats stay memoized per backing device.
    mkv_storage.storage_class(ssd_dir)
    mkv_storage.storage_class(hdd_dir)
    assert len(calls) == 2
    _clear_storage_caches()


def test_windows_class_cached_per_drive_root(monkeypatch, tmp_path):
    calls = []

    def counting_windows(path):
        calls.append(str(path))
        return "ssd"

    monkeypatch.setattr(mkv_storage, "_windows_storage_class", counting_windows)
    monkeypatch.setattr(mkv_storage.sys, "platform", "win32")
    try:
        first = mkv_storage.storage_class(tmp_path)
        second = mkv_storage.storage_class(tmp_path / "nested")
        assert first == second == "ssd"
        assert len(calls) == 1
    finally:
        _clear_storage_caches()


@posix_only
def test_linux_class_network_fs_by_mount_prefix(tmp_path, monkeypatch):
    """A network fstype on the covering mount classifies as network before
    any sysfs lookup happens."""
    _clear_storage_caches()
    video = tmp_path / "video.mkv"
    video.write_bytes(b"x")
    monkeypatch.setattr(
        mkv_storage, "_linux_mount_info",
        lambda target: ("nfs4", "server:/export"),
    )

    def fail_rotational(dev_id, sysfs_block=None):
        raise AssertionError("network check must short-circuit sysfs walk")

    monkeypatch.setattr(mkv_storage, "_rotational_from_sysfs", fail_rotational)
    monkeypatch.setattr(mkv_storage.sys, "platform", "linux")
    try:
        assert mkv_storage.storage_class(video) == "network"
    finally:
        _clear_storage_caches()


@posix_only
def test_rotational_walk_ssd_partition_under_disk(tmp_path):
    """Partition dev resolves (via symlink) into its disk dir owning queue/."""
    sysfs = tmp_path / "sys" / "dev" / "block"
    disk = sysfs / "8:0"
    part = disk / "sda1"
    part.mkdir(parents=True)
    (disk / "queue").mkdir()
    (disk / "queue" / "rotational").write_text("0")
    (sysfs / "8:1").symlink_to(part)

    assert mkv_storage._rotational_from_sysfs("8:1", sysfs) == "ssd"


@posix_only
def test_rotational_walk_hdd(tmp_path):
    sysfs = tmp_path / "sys" / "dev" / "block"
    disk = sysfs / "8:0"
    (disk / "queue").mkdir(parents=True)
    (disk / "queue" / "rotational").write_text("1")

    assert mkv_storage._rotational_from_sysfs("8:0", sysfs) == "hdd"


@posix_only
def test_rotational_walk_descends_into_dm_slaves(tmp_path):
    sysfs = tmp_path / "block"
    dm = sysfs / "253:0"
    sda = sysfs / "sda"
    (dm / "slaves").mkdir(parents=True)
    (sda / "queue").mkdir(parents=True)
    (sda / "queue" / "rotational").write_text("0")
    (dm / "slaves" / "sda2").symlink_to(sda)
    assert mkv_storage._rotational_from_sysfs("253:0", sysfs) == "ssd"
def test_linux_class_btrfs_anon_device_falls_back_to_mount_source(tmp_path, monkeypatch):
    """btrfs subvolume paths carry an anonymous st_dev absent from both sysfs
    and mountinfo; classification must consult the covering mount's backing
    device instead."""
    _clear_storage_caches()
    fake_backing = tmp_path / "dev" / "sda3"
    fake_backing.parent.mkdir()
    fake_backing.touch()

    monkeypatch.setattr(mkv_storage, "_device_key", lambda p: "8:3" if str(p) == str(fake_backing) else "0:51")
    monkeypatch.setattr(
        mkv_storage, "_linux_mount_info",
        lambda target: ("btrfs", f"{fake_backing}[/@home]"),
    )

    def fake_rotational(dev_id, sysfs_block=None):
        # The anon id has no sysfs queue; only the backing device resolves.
        return None if dev_id == "0:51" else "ssd"

    monkeypatch.setattr(mkv_storage, "_rotational_from_sysfs", fake_rotational)
    monkeypatch.setattr(mkv_storage.sys, "platform", "linux")
    video = tmp_path / "video.mkv"
    video.write_bytes(b"x")
    try:
        assert mkv_storage.storage_class(video) == "ssd"
    finally:
        _clear_storage_caches()


@posix_only
def test_rotational_walk_unknown_when_sysfs_missing(tmp_path):
    assert mkv_storage._rotational_from_sysfs("8:1", tmp_path / "empty") is None


def test_storage_class_unknown_when_stat_fails(tmp_path, monkeypatch):
    monkeypatch.setattr(mkv_storage.sys, "platform", "linux")
    monkeypatch.setattr(mkv_storage, "_device_key", lambda target: None)
    assert mkv_storage.storage_class(tmp_path / "missing") == "unknown"


def test_windows_storage_class_unknown_without_windll(tmp_path, monkeypatch):
    import ctypes
    monkeypatch.delattr(ctypes, "windll", raising=False)
    # On non-Windows there is no windll attribute at all; the helper must
    # degrade to 'unknown' rather than raise.
    assert mkv_storage._windows_storage_class(tmp_path) == "unknown"


def test_build_cli_runtime_carries_io_jobs():
    runtime = mkv_runner.build_cli_runtime(
        workers=4, dry_run=True, no_cache=True, io_jobs=3,
    )
    assert runtime.io_jobs == 3
    runtime_disabled = mkv_runner.build_cli_runtime(
        workers=4, dry_run=True, no_cache=True, io_jobs=0,
    )
    assert runtime_disabled.io_jobs is None
    assert runtime_disabled.io_semaphore is None


# ─── PersistentInspectCache syscall/prune semantics ──────────────────────────
def test_cache_key_resolved_once(tmp_path):
    cache_file = tmp_path / "cache.json"
    cache = PersistentInspectCache(cache_file=cache_file)
    video = tmp_path / "video.mkv"
    video.write_bytes(b"data")
    cache.put(video, {"id": 1})
    cache.get(video)
    raw_key = str(video)
    assert cache._resolved_keys[raw_key] == str(video.resolve())


def test_debounced_save_skips_prune_full_save_prunes(tmp_path):
    cache_file = tmp_path / "cache.json"
    cache = PersistentInspectCache(cache_file=cache_file)
    live = tmp_path / "live.mkv"
    dead = tmp_path / "dead.mkv"
    live.write_bytes(b"a")
    dead.write_bytes(b"b")
    cache.put(live, {"id": 1})
    cache.put(dead, {"id": 2})
    cache.save(prune=False)

    on_disk = json.loads(cache_file.read_text())
    dead_key = str(dead.resolve())
    live_key = str(live.resolve())
    assert dead_key in on_disk  # intermediate save does no stat pass
    assert live_key in on_disk

    # Full save prunes entries whose source vanished. A write must be
    # pending for pruning to run (saves skip entirely when not dirty).
    dead.unlink()
    cache.put(live, {"id": 1})
    cache.save()
    on_disk = json.loads(cache_file.read_text())
    assert dead_key not in on_disk
    assert live_key in on_disk


def test_debounced_save_interval_skip_stays_dirty(tmp_path):
    cache_file = tmp_path / "cache.json"
    cache = PersistentInspectCache(cache_file=cache_file)
    video = tmp_path / "video.mkv"
    video.write_bytes(b"a")
    cache.put(video, {"id": 1})
    cache.save(min_interval=60.0)  # first write goes through

    snapshot = cache_file.read_text()
    cache.put(video, {"id": 9})  # dirty again
    cache.save(min_interval=60.0, prune=False)  # within interval -> skipped
    assert cache._dirty is True
    assert cache_file.read_text() == snapshot

    cache.save()  # final flush ignores the interval
    assert json.loads(cache_file.read_text())[str(video.resolve())]["json_data"] == {"id": 9}
    assert cache._dirty is False


# ─── SubIndex precomputed scoring parity ──────────────────────────────────────
def test_sub_entry_precomputes_language_and_scores(tmp_path):
    sub = _create_parsed_file(tmp_path / "Show - 01 [IND] - dialogue.ass")
    entry = _make_sub_entry(sub)
    assert entry.lang == "ind"
    assert entry.is_dialogue is True
    assert entry.category_score > 0


def test_sub_index_lookup_parity_after_precompute(tmp_path):
    index = SubIndex()
    local_ind = _create_parsed_file(tmp_path / "Show - 01 [IND].ass")
    other_eng_srt = _create_parsed_file(tmp_path / "Show - 01 [ENG].srt")
    unparsed = _create_parsed_file(tmp_path / "readme.txt.ass")

    index.add(other_eng_srt)
    index.add(local_ind)
    index.add(unparsed)

    parsed = EpisodeInfo("", "E", "01")
    # Local directory wins over higher-scoring distant candidates.
    assert index.lookup(parsed, local_dir=tmp_path) == (local_ind.path, "ind")
    # Without locality bias the Indonesian ASS candidate outranks English SRT.
    assert index.lookup(parsed) == (local_ind.path, "ind")
    # Duplicate adds are ignored.
    index.add(local_ind)
    assert index.indexed_count == 2
    # Unparsed files surface in all_subtitles but not lookups.
    assert unparsed.path in index.all_subtitles
    assert len(index.all_subtitles) == 3


def test_sub_index_counts_use_cached_langs(tmp_path):
    index = SubIndex()
    index.add(_create_parsed_file(tmp_path / "Show - 01 [IND].ass"))
    index.add(_create_parsed_file(tmp_path / "Show - 02 [IND].ass"))
    index.add(_create_parsed_file(tmp_path / "Show - 03 [ENG].ass"))
    assert index.primary_count == 2
    assert index.fallback_count == 1
