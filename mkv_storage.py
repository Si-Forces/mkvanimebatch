"""
mkv_storage.py
Backing-store classification used to pick --io-jobs defaults.

Classifies the filesystem behind a path as 'ssd', 'hdd', 'network', or
'unknown' and maps it to a concurrent-mkvmerge job count. Results are
memoized per backing device (POSIX) or drive root (Windows): sequential
remux streams scale well on SSD/NVMe but thrash spinning disks past two.
"""
from __future__ import annotations

import ctypes
import os
import stat
import struct
import sys
from functools import lru_cache
from pathlib import Path

SSD_IO_JOBS = 4
HDD_IO_JOBS = 2
UNKNOWN_IO_JOBS = HDD_IO_JOBS

_NETWORK_FS_TYPES = frozenset({"nfs", "nfs4", "cifs", "smbfs", "sshfs", "ncpfs", "afs", "9p"})

# Real sysfs root; tests may point this at a fixture tree.
_SYSFS_BLOCK = Path("/sys/dev/block")


def resolve_io_jobs(flag_value: int | None, input_dir: Path) -> int:
    """Combine an explicit --io-jobs flag with storage auto-detection."""
    return flag_value if flag_value is not None else default_io_jobs(input_dir)


def default_io_jobs(path: Path | str) -> int:
    """Concurrent-mkvmerge default for the given input tree's storage."""
    return SSD_IO_JOBS if storage_class(path) == "ssd" else UNKNOWN_IO_JOBS


def storage_class(path: Path | str) -> str:
    """Best-effort backing-store classification of a path: 'ssd', 'hdd',
    'network', or 'unknown'. Memoized per backing filesystem device."""
    p = Path(path)
    try:
        resolved = p.resolve()
    except OSError:
        return "unknown"
    if sys.platform == "win32":
        return _windows_class_cached(str(resolved.anchor or resolved))
    dev_id = _device_key(resolved)
    if dev_id is None:
        # Path cannot be stat'ed at all — nothing to classify or memoize.
        return "unknown"
    cls = _DEV_CLASS_CACHE.get(dev_id)
    if cls is None:
        cls = _classify_linux(resolved, dev_id)
        _DEV_CLASS_CACHE[dev_id] = cls
    return cls


@lru_cache(maxsize=64)
def _windows_class_cached(drive_root: str) -> str:
    return _windows_storage_class(Path(drive_root))


def _classify_linux(target: Path, dev_id: str) -> str:
    """Classify one POSIX filesystem instance ('major:minor' + a sample path).

    dev_id keys the cache so every path on the same filesystem shares one
    answer while multi-mount boxes (e.g. / on SSD, /mnt/hdd on HDD) classify
    independently. Mount resolution goes through the longest path prefix so
    btrfs subvolume anonymous devices (absent from mountinfo) resolve to
    their mount's backing disk.
    """
    fstype, source = _linux_mount_info(str(target))
    if fstype in _NETWORK_FS_TYPES:
        return "network"
    result = _rotational_from_sysfs(dev_id)
    if result is None and source:
        # Anonymous-device filesystems (e.g. multi-device btrfs) have no
        # sysfs queue of their own; consult the mount's backing device.
        backing = Path(source.split("[", 1)[0])
        if backing.exists():
            result = _rotational_from_sysfs(_device_key(backing))
    return result if result is not None else "unknown"


# Classification per backing device id; bounded by the number of distinct
# filesystems a run touches. Plain dict under GIL like other module caches.
_DEV_CLASS_CACHE: dict[str, str] = {}


def _device_key(path: Path) -> str | None:
    """'major:minor' device id for path, or None when unavailable.

    Block special files report their own device via st_rdev; everything else
    uses st_dev (the filesystem device they live on).
    """
    try:
        st = os.stat(path)
    except OSError:
        return None
    if stat.S_ISBLK(st.st_mode):
        return f"{os.major(st.st_rdev)}:{os.minor(st.st_rdev)}"
    return f"{os.major(st.st_dev)}:{os.minor(st.st_dev)}"


def _rotational_from_sysfs(dev_id: str | None, sysfs_block: Path | None = None) -> str | None:
    """Walk sysfs from a 'major:minor' id down to the physical disk's
    queue/rotational flag: 'hdd', 'ssd', or None when unresolvable.

    Handles partitions (ascend to parent disk) and virtual layers like
    LVM/dm (descend into the first slave). sysfs_block overrides _SYSFS_BLOCK
    for tests.
    """
    if not dev_id:
        return None
    block_root = sysfs_block if sysfs_block is not None else _SYSFS_BLOCK
    try:
        dev = (block_root / dev_id).resolve()
        for _ in range(16):
            flag = dev / "queue" / "rotational"
            if flag.is_file():
                return "hdd" if flag.read_text().strip() == "1" else "ssd"
            slaves_dir = dev / "slaves"
            if slaves_dir.is_dir():
                slaves = sorted(entry.name for entry in slaves_dir.iterdir())
                if slaves:
                    dev = (slaves_dir / slaves[0]).resolve()
                    continue
            if dev.parent == dev:
                return None
            dev = dev.parent
    except OSError:
        return None
    return None


def _linux_mount_info(target: str) -> tuple[str | None, str | None]:
    """(fstype, source device) of the longest-prefix mount covering target."""
    best_type: str | None = None
    best_source: str | None = None
    best_len = -1
    try:
        with open("/proc/self/mountinfo", encoding="utf-8") as fh:
            for line in fh:
                head, sep, tail = line.partition(" - ")
                if not sep:
                    continue
                fields = head.split()
                tail_fields = tail.split(maxsplit=2)
                if len(fields) < 5 or len(tail_fields) < 2:
                    continue
                mount_point = fields[4]
                if target == mount_point or target.startswith(mount_point.rstrip("/") + "/"):
                    if len(mount_point) > best_len:
                        best_len = len(mount_point)
                        best_type = tail_fields[0]
                        best_source = tail_fields[1]
    except OSError:
        return None, None
    return best_type, best_source


def _windows_storage_class(path: Path) -> str:
    """Classify path's drive via the seek-penalty IOCTL; no admin rights needed.

    Per winioctl.h, DEVICE_SEEK_PENALTY_DESCRIPTOR is {Version DWORD,
    Size DWORD, IncursSeekPenalty BOOLEAN}, so the flag lands at byte 8.
    """
    try:
        windll = getattr(ctypes, "windll", None)
        if windll is None:
            return "unknown"
        anchor = path.resolve().anchor
        if not anchor:
            return "unknown"
        drive = anchor.rstrip("\\/")
        volume = f"\\\\.\\{drive}"
        QUERY_ONLY_ACCESS = 0
        FILE_SHARE_READ_WRITE = 0x1 | 0x2
        OPEN_EXISTING = 3
        IOCTL_STORAGE_QUERY_PROPERTY = 0x2D1400
        StorageDeviceSeekPenaltyProperty = 7
        PropertyStandardQuery = 0

        kernel32 = windll.kernel32
        kernel32.CreateFileW.restype = ctypes.c_void_p
        handle = kernel32.CreateFileW(
            volume, QUERY_ONLY_ACCESS, FILE_SHARE_READ_WRITE, None, OPEN_EXISTING, 0, None
        )
        if not handle or handle == ctypes.c_void_p(-1).value:
            return "unknown"
        try:
            query = struct.pack("II", StorageDeviceSeekPenaltyProperty, PropertyStandardQuery)
            descriptor = ctypes.create_string_buffer(16)
            returned = ctypes.c_ulong()
            ok = kernel32.DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                query,
                len(query),
                descriptor,
                len(descriptor),
                ctypes.byref(returned),
                None,
            )
            reported_size = struct.unpack_from("<I", descriptor.raw, 4)[0]
            if not ok or returned.value < 9 or reported_size < 9:
                return "unknown"
            incurs_seek_penalty = descriptor.raw[8]
            return "hdd" if incurs_seek_penalty else "ssd"
        finally:
            windll.kernel32.CloseHandle(handle)
    except Exception:
        return "unknown"
