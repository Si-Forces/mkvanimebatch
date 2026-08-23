"""
mkv_events.py
Structured @event: sidecar output, logging configuration, and item identity.
"""
from __future__ import annotations

import json
import logging
import sys
from functools import lru_cache
from pathlib import Path

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
