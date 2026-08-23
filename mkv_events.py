"""
mkv_events.py
Structured @event: sidecar output, logging configuration, and item identity.

The event vocabulary and wire shapes are fixed by the Sidecar Contract
(docs/adr/0001-sidecar-contract-and-structured-planning.md). Emit events
through the named emitters below; emit_event is the single low-level
protocol primitive they all delegate to (and the seam tests capture).
"""
from __future__ import annotations

import json
import logging
import sys
from functools import lru_cache
from pathlib import Path
from typing import Literal

# Item Outcome: the closed set of terminal statuses an item_complete event
# may carry. Planning-time outcomes ("planned", "skipped" in --plan/dry-run
# mode) are emitted WITHOUT a preceding item_start; consumers must treat
# every outcome here as terminal regardless (ADR-0001 addendum).
OUTCOME_COMPLETED = "completed"
OUTCOME_SKIPPED = "skipped"
OUTCOME_ERROR = "error"
OUTCOME_CANCELLED = "cancelled"
OUTCOME_PLANNED = "planned"

ItemOutcome = Literal[
    "completed", "skipped", "error", "cancelled", "planned"
]

_EVENTS_ENABLED: bool = False


def set_events_enabled(enabled: bool = True) -> None:
    """Enable or disable machine-readable structured @event: output."""
    global _EVENTS_ENABLED
    _EVENTS_ENABLED = enabled


def is_events_enabled() -> bool:
    """Return whether structured event output is currently enabled."""
    return _EVENTS_ENABLED


@lru_cache(maxsize=16384)
def get_item_id(video_path: Path, root_input_dir: Path) -> str:
    """Derive root-relative POSIX path ID for sidecar contract events and manifest items."""
    try:
        return video_path.resolve().relative_to(root_input_dir.resolve()).as_posix()
    except ValueError:
        try:
            return video_path.relative_to(root_input_dir).as_posix()
        except ValueError:
            return video_path.name

class BelowLevelFilter(logging.Filter):
    """Filter log records whose level is strictly below a specified level."""
    def __init__(self, below_level: int):
        super().__init__()
        self.below_level = below_level

    def filter(self, record: logging.LogRecord) -> bool:
        return record.levelno < self.below_level


def setup_logging(level: int = logging.INFO, *, stderr_only: bool = False) -> None:
    """Configure system-wide logging with standard output and error splits."""
    root_logger = logging.getLogger()
    root_logger.setLevel(level)

    for handler in list(root_logger.handlers):
        root_logger.removeHandler(handler)
        handler.close()

    log_format = logging.Formatter("%(asctime)s [%(levelname)s] %(message)s")

    def _stream_handler(stream, handler_level: int) -> logging.StreamHandler:
        handler = logging.StreamHandler(stream)
        handler.setLevel(handler_level)
        handler.setFormatter(log_format)
        return handler

    if stderr_only:
        root_logger.addHandler(_stream_handler(sys.stderr, level))
    else:
        stdout_handler = _stream_handler(sys.stdout, level)
        stdout_handler.addFilter(BelowLevelFilter(logging.ERROR))
        root_logger.addHandler(stdout_handler)
        root_logger.addHandler(_stream_handler(sys.stderr, logging.ERROR))


def emit_event(event_type: str, *, item_id: str | None = None, **payload) -> None:
    """Emit a machine-readable structured JSON-line to stdout for sidecar consumers when enabled."""
    if not _EVENTS_ENABLED:
        return
    ev = {"version": 1, "type": event_type}
    if item_id is not None:
        ev["id"] = item_id
    elif "id" in payload:
        ev["id"] = payload.pop("id")
    ev.update(payload)
    msg = json.dumps(ev, ensure_ascii=False)
    sys.stdout.write(f"@event: {msg}\n")
    sys.stdout.flush()


def emit_item_start(item_id: str, file: str) -> None:
    """Emit item_start for a job beginning execution."""
    emit_event("item_start", item_id=item_id, file=file)


def emit_item_complete(
    item_id: str,
    file: str,
    outcome: ItemOutcome,
    *,
    reason: str | None = None,
    error: str | None = None,
) -> None:
    """Emit a terminal item_complete carrying one Item Outcome.

    Skips and cancellations carry reason; errors carry error. Planning-time
    outcomes (planned, dry-run skipped) are emitted without a prior
    item_start — that asymmetry is contract-visible and intentional.
    """
    extra: dict[str, str] = {}
    if reason is not None:
        extra["reason"] = reason
    if error is not None:
        extra["error"] = error
    emit_event("item_complete", item_id=item_id, file=file, status=outcome, **extra)


def emit_batch_start(total: int, input_dir: str) -> None:
    """Emit batch_start for the beginning of a batch run."""
    emit_event("batch_start", total=total, input_dir=input_dir)


def emit_batch_complete(*, success: bool, cancelled: bool) -> None:
    """Emit batch_complete with overall outcome; cancelled implies success=False upstream."""
    emit_event("batch_complete", success=success, cancelled=cancelled)
