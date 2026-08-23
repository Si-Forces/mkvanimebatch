"""
mkv_runner.py
Shared command-line scaffolding for the mkvanimebatch tools: common argument
definitions, inspect-cache lifecycle, and the batch directory-iteration loop.
"""
from __future__ import annotations

import argparse
import logging
import os
import signal
import sys
import threading
from collections import defaultdict
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

from mkv_domain import (
    DEFAULT_WORKER_MULTIPLIER,
    MAX_DEFAULT_WORKERS,
    natural_sort_key,
)
from mkv_proc import CancelToken
from mkv_scan import (
    ParsedFile,
    PersistentInspectCache,
    scan_mkv_files,
)

logger = logging.getLogger("mkv_automation")

__all__ = [
    "SCRIPT_DIR",
    "CliRuntime",
    "add_shared_args",
    "build_cli_runtime",
    "collect_release_dirs",
    "default_worker_count",
    "handle_clear_cache_flag",
    "install_cancel_handlers",
    "run_batch",
]

# Both CLI entrypoints live beside this module, so this resolves to the same
# directory they used to compute independently.
SCRIPT_DIR = Path(__file__).resolve().parent


def default_worker_count() -> int:
    return min((os.cpu_count() or 4) * DEFAULT_WORKER_MULTIPLIER, MAX_DEFAULT_WORKERS)


def add_shared_args(parser: argparse.ArgumentParser) -> None:
    """Register the flags every mkvanimebatch CLI accepts."""
    parser.add_argument("--workers", type=int, default=default_worker_count(),
                        choices=range(1, 65),
                        metavar="[1-64]",
                        help="Number of concurrent files to process.")
    parser.add_argument("--input-dir", type=str, default=None,
                        help="Directory containing MKV files (defaults to script directory).")
    parser.add_argument("--no-cache", action="store_true",
                        help="Bypass persistent inspection cache.")
    parser.add_argument("--clear-cache", action="store_true",
                        help="Clear persistent inspection cache and exit.")
    parser.add_argument("--events", action="store_true",
                        help="Emit structured @event: JSON-line events on stdout.")


def handle_clear_cache_flag(clear_cache: bool) -> None:
    """Implement --clear-cache: wipe the shared inspect cache, then exit."""
    if not clear_cache:
        return
    PersistentInspectCache().clear()
    logger.info("Persistent inspection cache cleared.")
    sys.exit(0)


@dataclass
class CliRuntime:
    """Infrastructure shared across one whole CLI batch run."""

    workers:          int
    sub_offset:       float
    dry_run:          bool
    io_semaphore:     threading.Semaphore | None
    persistent_cache: PersistentInspectCache
    cancel_token:     CancelToken = field(default_factory=CancelToken)


def install_cancel_handlers(cancel_token: CancelToken) -> None:
    """Route SIGINT/SIGTERM into the shared cancellation token.

    Once installed, Ctrl+C no longer raises KeyboardInterrupt in the main
    thread; workers observe the token and unwind cooperatively instead.
    """
    def _handler(signum, frame):
        cancel_token.request()

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, _handler)
        except (OSError, ValueError):
            pass


def build_cli_runtime(
    *,
    workers: int,
    sub_offset: float,
    dry_run: bool,
    no_cache: bool,
    io_jobs: int,
) -> CliRuntime:
    """Assemble the runtime from already-validated CLI arguments."""
    return CliRuntime(
        workers=workers,
        sub_offset=sub_offset,
        dry_run=dry_run,
        io_semaphore=threading.BoundedSemaphore(io_jobs) if io_jobs > 0 else None,
        persistent_cache=PersistentInspectCache(enabled=not no_cache),
    )


def collect_release_dirs(
    root_input_dir: Path,
    *,
    ignore_paths: set[Path] | None = None,
) -> dict[Path, list[ParsedFile]]:
    """Recursively scan for MKVs grouped by release directory, in natural order.

    Exits with status 1 (and an error log) when nothing is found.
    """
    all_mkvs = scan_mkv_files(root_input_dir, ignore_paths=ignore_paths)
    if not all_mkvs:
        logger.error("No MKV files found.")
        sys.exit(1)

    grouped: dict[Path, list[ParsedFile]] = defaultdict(list)
    for mkv in all_mkvs:
        grouped[mkv.path.parent].append(mkv)

    return {d: grouped[d] for d in sorted(grouped.keys(), key=natural_sort_key)}


DirectoryProcessor = Callable[[ThreadPoolExecutor, Path, list[ParsedFile]], bool]


def run_batch(
    runtime: CliRuntime,
    mkvs_by_dir: dict[Path, list[ParsedFile]],
    process_directory: DirectoryProcessor,
) -> bool:
    """Process every release directory through one shared worker pool.

    Prints the per-directory banner, delegates each directory to
    `process_directory(pool, mkv_dir, dir_mkvs)`, and debounces persistent-cache
    saves between directories plus one final flush. Remaining directories are
    skipped once cancellation is requested. Returns True only when every
    directory succeeded without cancellation; emitting batch events stays the
    caller's responsibility so post-loop work can run before batch_complete.
    """
    token = runtime.cancel_token
    overall_success = True

    with ThreadPoolExecutor(max_workers=runtime.workers) as pool:
        for mkv_dir, dir_mkvs in mkvs_by_dir.items():
            if token.cancelled:
                logger.warning("Cancellation requested — skipping remaining director(ies).")
                break

            logger.info("")
            logger.info("=" * 60)
            logger.info("Processing directory: %s", mkv_dir)
            logger.info("=" * 60)

            if not process_directory(pool, mkv_dir, dir_mkvs):
                overall_success = False

            runtime.persistent_cache.save(min_interval=60.0)

    runtime.persistent_cache.save()
    return overall_success and not token.cancelled
