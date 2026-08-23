"""
mkv_proc.py
Subprocess execution with cooperative cancellation, plus content hashing.
"""
from __future__ import annotations

import hashlib
import subprocess
import threading
import time
from contextlib import nullcontext
from pathlib import Path

SUBPROCESS_TIMEOUT = 3600

class CancelledError(BaseException):
    """Raised inside workers once cooperative cancellation has been requested.

    Derives from BaseException (like asyncio.CancelledError) so generic
    ``except Exception`` handlers cannot misreport user cancellation as a
    per-job failure.
    """


class CancelToken:
    """Thread-safe cooperative cancellation flag shared across a batch run."""

    def __init__(self) -> None:
        self._event = threading.Event()

    @property
    def cancelled(self) -> bool:
        """Whether cancellation has been requested."""
        return self._event.is_set()

    def request(self) -> None:
        """Request cancellation of every worker observing this token."""
        self._event.set()

    def check(self) -> None:
        """Raise CancelledError if cancellation was requested."""
        if self._event.is_set():
            raise CancelledError("Cancellation requested")


_TERMINATE_GRACE_SECONDS = 5.0


def _terminate_process(proc: subprocess.Popen) -> None:
    """Terminate a child process, escalating to kill after a short grace."""
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=_TERMINATE_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        proc.kill()
        try:
            proc.wait(timeout=_TERMINATE_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            pass


def run_subprocess_cancellable(
    cmd: list[str],
    *,
    cancel_token: CancelToken | None = None,
    timeout: float | None = None,
    capture_stdout: bool = True,
    poll_interval: float = 0.25,
) -> subprocess.CompletedProcess[str]:
    """Run a subprocess like run_subprocess, but observe a CancelToken.

    When cancel_token is None this delegates to run_subprocess unchanged.
    Otherwise the child is polled regularly: on cancellation the child is
    terminated (then killed after a grace period) and CancelledError is
    raised; on timeout the child is killed and subprocess.TimeoutExpired is
    raised, mirroring subprocess.run semantics.
    """
    if cancel_token is None:
        return run_subprocess(cmd, capture_stdout=capture_stdout, timeout=timeout)

    stdout_opt = subprocess.PIPE if capture_stdout else subprocess.DEVNULL
    proc = subprocess.Popen(
        cmd,
        stdout=stdout_opt,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    deadline: float | None = None
    timeout_seconds = 0.0
    if timeout is not None:
        deadline = time.monotonic() + timeout
        timeout_seconds = float(timeout)
    try:
        while True:
            if cancel_token.cancelled:
                _terminate_process(proc)
                cmd_name = cmd[0] if cmd else "subprocess"
                raise CancelledError(f"Cancellation requested during: {cmd_name}")
            try:
                stdout, stderr = proc.communicate(timeout=poll_interval)
                return subprocess.CompletedProcess(cmd, proc.returncode, stdout, stderr)
            except subprocess.TimeoutExpired:
                if deadline is not None and time.monotonic() >= deadline:
                    _terminate_process(proc)
                    raise subprocess.TimeoutExpired(cmd, timeout_seconds) from None
    finally:
        if proc.poll() is None:
            proc.kill()


def run_subprocess(cmd: list[str], *, capture_stdout: bool = True, timeout: float | None = None) -> subprocess.CompletedProcess[str]:
    """Run a subprocess with consistent UTF-8 encoding/error handling."""
    stdout_opt = subprocess.PIPE if capture_stdout else subprocess.DEVNULL
    return subprocess.run(
        cmd,
        stdout=stdout_opt,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        check=False,
    )


def acquire_semaphore(sem: threading.Semaphore | None):
    """Return the semaphore as a context manager, or a no-op if None."""
    return sem if sem is not None else nullcontext()

def hash_file(filepath: Path) -> str:
    """Calculate BLAKE2b hash of a file for content-based deduplication."""
    with filepath.open("rb") as f:
        if hasattr(hashlib, "file_digest"):
            return hashlib.file_digest(f, hashlib.blake2b).hexdigest()  # type: ignore[attr-defined]
        hasher = hashlib.blake2b()
        while chunk := f.read(65536):
            hasher.update(chunk)
        return hasher.hexdigest()
