"""Bounded scheduling, explicit cleanup, and terminal outcomes without media tools."""
import gc
import threading
import weakref
from concurrent.futures import CancelledError as FutureCancelledError
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path

import pytest

import mkv_events
import mkv_remux_job
import mkv_scan
from mkv_proc import CancelledError, CancelToken, submit_bounded
from mkv_remux_job import JobResult, PlannedJob, RemuxExecutionContext, execute_jobs


def test_submission_bound_and_result_release(monkeypatch):
    class Payload:
        pass

    submitted = consumed = 0
    references = []
    with ThreadPoolExecutor(max_workers=2) as pool:
        original = pool.submit

        def submit(fn, item):
            nonlocal submitted
            submitted += 1
            assert submitted - consumed <= 4
            return original(fn, item)

        monkeypatch.setattr(pool, "submit", submit)
        with submit_bounded(pool, lambda _: Payload(), range(40)) as completed:
            for _, future in completed:
                references.append(weakref.ref(future.result()))
                consumed += 1
                del future
                gc.collect()
                # At most the current yield is still owned by the helper.
                assert sum(ref() is not None for ref in references) <= 1
        assert submitted == consumed == 40


@pytest.mark.parametrize("exit_mode", ["break", "cancel", "error"])
def test_early_exit_cancels_queued_and_drains_running_before_pool_shutdown(monkeypatch, exit_mode):
    running = threading.Event()
    release = threading.Event()
    drained = threading.Event()
    token = CancelToken()
    futures = []

    def worker(item):
        if item == 0:
            assert running.wait(5)
        elif item == 1:
            running.set()
            assert release.wait(5)
            drained.set()
        else:
            pytest.fail("queued task should never run")
        return item

    with ThreadPoolExecutor(max_workers=2) as pool:
        original = pool.submit

        def submit(fn, item):
            # Deliberately plain CANCELLED futures, with no executor notification.
            future = original(fn, item) if item < 2 else Future()
            if item >= 2:
                future.add_done_callback(lambda f: release.set() if f.cancelled() else None)
            futures.append(future)
            return future

        monkeypatch.setattr(pool, "submit", submit)
        caught = False
        try:
            try:
                with submit_bounded(pool, worker, range(20), cancel_token=token) as completed:
                    assert next(completed)[0] == 0
                    if exit_mode == "error":
                        raise ValueError("consumer failed")
                    if exit_mode == "cancel":
                        token.request()
                        with pytest.raises(CancelledError):
                            for _, future in completed:
                                try:
                                    future.result()
                                except FutureCancelledError:
                                    pass
            except ValueError as exc:
                caught = True
                assert exit_mode == "error" and str(exc) == "consumer failed"
            assert caught == (exit_mode == "error")
            assert drained.is_set()
            assert all(f.done() for f in futures)
            assert len(futures) <= 5
            assert all(f.cancelled() for f in futures[2:])
        finally:
            release.set()


def test_abandoned_results_released_even_if_caller_keeps_yielded_future():
    class Payload:
        pass

    references = []
    second_finished = threading.Event()
    with ThreadPoolExecutor(max_workers=1) as pool:
        def worker(_):
            result = Payload()
            references.append(weakref.ref(result))
            if len(references) == 2:
                second_finished.set()
            return result

        with submit_bounded(pool, worker, range(4)) as completed:
            _, retained = next(completed)
            assert second_finished.wait(5)
        gc.collect()
        assert sum(ref() is not None for ref in references) == 1
        assert isinstance(retained.result(), Payload)


def test_inspection_worker_cancellation_without_token_stops_refill(monkeypatch):
    probed = []

    def probe(path, **_kwargs):
        probed.append(path)
        raise CancelledError("probe cancelled")

    monkeypatch.setattr(mkv_scan, "run_mkvmerge_json", probe)
    with ThreadPoolExecutor(max_workers=1) as pool:
        with pytest.raises(CancelledError):
            mkv_scan.inspect_videos([Path(f"{i}.mkv") for i in range(20)], pool)
    assert 1 <= len(probed) <= 2


@pytest.mark.parametrize("pre_cancelled", [False, True])
def test_remux_cancellation_preserves_outcomes_and_stops_submission(monkeypatch, pre_cancelled):
    token = CancelToken()
    if pre_cancelled:
        token.request()
    events = []
    mkv_events.set_events_enabled(True)
    monkeypatch.setattr(mkv_events, "emit_event", lambda event, **data: events.append((event, data)))
    jobs = [PlannedJob(Path(f"{i}.mkv"), f"{i}.mkv", str(i), Path(f"out/{i}.mkv"),
                       None, "", "und", None, None, item_id=str(i)) for i in range(20)]
    invoked = []

    def worker(job, **_kwargs):
        invoked.append(job.item_id)
        if job.item_id == "0":
            mkv_events.emit_item_complete(job.item_id, job.video_name, "completed")
            return JobResult()
        if job.item_id == "1":
            token.request()
            raise RuntimeError("crash during cancellation")
        return JobResult(cancelled=True)

    monkeypatch.setattr(mkv_remux_job, "process_job", worker)
    with ThreadPoolExecutor(max_workers=1) as pool:
        context = RemuxExecutionContext("192k", 1, None, 5, 0, token, 1)
        with pytest.raises(CancelledError):
            execute_jobs(jobs, pool, {}, context)
    terminal = [data for event, data in events if event == "item_complete"]
    assert len(terminal) == len(jobs)
    statuses = {data["item_id"]: data["status"] for data in terminal}
    assert len(statuses) == len(jobs)
    if pre_cancelled:
        assert not invoked
        assert set(statuses.values()) == {"cancelled"}
    else:
        assert len(invoked) <= 3
        assert statuses["0"] == "completed"
        assert statuses["1"] == "error"
        assert all(statuses[str(i)] == "cancelled" for i in range(2, 20))
