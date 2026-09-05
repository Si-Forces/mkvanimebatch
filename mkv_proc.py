"""
mkv_proc.py
Subprocess execution with cooperative cancellation, plus content hashing.
"""
from __future__ import annotations

import hashlib
import subprocess
import threading
import time
import weakref
from collections.abc import Callable, Generator, Iterable, Iterator
from concurrent.futures import Future, ThreadPoolExecutor, wait
from contextlib import contextmanager
from pathlib import Path
from queue import Empty, SimpleQueue
from typing import TypeVar

SUBPROCESS_TIMEOUT = 3600

T = TypeVar("T")
R = TypeVar("R")

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

    cancel_token.check()
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
            _terminate_process(proc)

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


@contextmanager
def acquire_semaphore(
    sem: threading.Semaphore | None,
    cancel_token: CancelToken | None = None,
):
    """Acquire a concurrency permit while continuing to observe cancellation."""
    if cancel_token is not None:
        cancel_token.check()
    if sem is None:
        yield
        return

    if cancel_token is None:
        sem.acquire()
        try:
            yield
        finally:
            sem.release()
        return

    while True:
        cancel_token.check()
        if sem.acquire(timeout=0.25):
            try:
                yield
            finally:
                sem.release()
            return


def hash_file(filepath: Path) -> str:
    """Calculate BLAKE2b hash of a file for content-based deduplication."""
    with filepath.open("rb") as f:
        if hasattr(hashlib, "file_digest"):
            return hashlib.file_digest(f, hashlib.blake2b).hexdigest()  # type: ignore[attr-defined]
        hasher = hashlib.blake2b()
        while chunk := f.read(65536):
            hasher.update(chunk)
        return hasher.hexdigest()


@contextmanager
def submit_bounded(
    pool: ThreadPoolExecutor,
    fn: Callable[[T], R],
    items: Iterable[T],
    *,
    max_pending: int | None = None,
    cancel_token: CancelToken | None = None,
) -> Iterator[Iterator[tuple[T, Future[R]]]]:
    """Bound submission and yield completions; context exit cancels/drains remaining work.

    Cancellation stops refill but yields every submitted outcome before raising.
    Callers handle item outcomes, including items never submitted, after context exit.
    """
    # ThreadPoolExecutor has no public worker-count accessor; keep this dependency here.
    limit = 2 * pool._max_workers if max_pending is None else max_pending
    if limit < 1:
        raise ValueError("max_pending must be positive")
    pending: dict[Future[R], T] = {}
    ready: SimpleQueue[Future[R]] = SimpleQueue()
    ready_ref = weakref.ref(ready)

    def completed(future: Future[R]) -> None:
        queue = ready_ref()
        if queue is not None:
            queue.put(future)

    def completions() -> Generator[tuple[T, Future[R]], None, None]:
        iterator = iter(items)
        exhausted = False
        while True:
            while not exhausted and len(pending) < limit and not (cancel_token and cancel_token.cancelled):
                try:
                    item = next(iterator)
                except StopIteration:
                    exhausted = True
                    break
                if cancel_token and cancel_token.cancelled:
                    break
                future = pool.submit(fn, item)
                pending[future] = item
                # Callbacks also notify for plain CANCELLED futures, unlike wait().
                future.add_done_callback(completed)
                del item, future
            if cancel_token and cancel_token.cancelled:
                for future in pending:
                    future.cancel()
            if not pending:
                if cancel_token:
                    cancel_token.check()
                return
            try:
                future = ready.get(timeout=0.25)
            except Empty:
                continue
            yield pending.pop(future), future
            del future

    iterator = completions()
    try:
        yield iterator
    finally:
        iterator.close()
        # Exclude successfully cancelled futures: wait() may otherwise await the
        # executor acknowledging a cancellation behind unrelated running work.
        running = [future for future in pending if not future.cancel()]
        if running:
            wait(running)
        pending.clear()
