"""
test_thread_safety.py — GIL release validation + concurrent write correctness.

=== GIL RELEASE (py.detach) — VERIFIED GREEN ===
Sequential calls from different threads (thread A calls, joins, then thread B calls)
all complete successfully. This confirms py.detach() IS correctly placed in every
PyEngine method — the GIL is released before block_on().

=== DEFECT-THREAD-1 FIXED ===
Previously, two Python threads calling engine.ingest_claim() concurrently with
DIFFERENT agent_ids would fail with StorageError("a transaction is already open").

Root cause: per-agent locks were keyed by agent_id; two different agents could both
enter begin_atomic() on the shared SQLite connection simultaneously.

Fix applied (engine_handle.rs): a store-level tokio::sync::Mutex<()> is acquired
BEFORE the per-agent lock in both ingest_claim and reconcile. This serializes all
writes at the store level while keeping reads (query_memory, query_audit) fully
concurrent and lock-free.

=== TEST STRUCTURE ===
Tests are split:
  1. TestGILRelease — verify GIL release (no deadlock, errors are MempillError)
  2. TestConcurrentWriteCorrectness — DEFECT-THREAD-1 fixed: concurrent cross-agent
     writes must ALL succeed (no StorageErrors)
  3. TestSequentialCrossThread — verify sequential cross-thread use works
"""

from __future__ import annotations

import threading
import time

import pytest

import mempill
from mempill import StorageError, MempillError
from mempill.types import ProvenanceLabel

DEADLOCK_TIMEOUT_SECONDS = 10
THREAD_COUNT = 4


def _make_req(agent_id: str, index: int, predicate: str = "city") -> dict:
    return {
        "agent_id": agent_id,
        "subject": f"user-{index}",
        "predicate": predicate,
        "value": f"val-{index}",
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    }


def _run_threads_barrier(
    engine: mempill.Engine,
    requests: list[dict],
) -> tuple[list[dict], list[Exception], float]:
    """Run all requests concurrently (Barrier ensures simultaneous entry into Rust)."""
    barrier = threading.Barrier(len(requests))
    results: list[dict | None] = [None] * len(requests)
    errors: list[Exception | None] = [None] * len(requests)

    def worker(idx: int, req: dict) -> None:
        barrier.wait()
        try:
            results[idx] = engine.ingest_claim(req)
        except Exception as exc:  # noqa: BLE001
            errors[idx] = exc

    threads = [
        threading.Thread(target=worker, args=(i, req), daemon=True)
        for i, req in enumerate(requests)
    ]
    t0 = time.monotonic()
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=DEADLOCK_TIMEOUT_SECONDS)
    return (
        [r for r in results if r is not None],
        [e for e in errors if e is not None],
        time.monotonic() - t0,
    )


# ── GIL release tests (must pass) ────────────────────────────────────────────

class TestGILRelease:
    """Verify py.detach() is present: no deadlock even under concurrent entry."""

    def test_concurrent_ingest_completes_no_deadlock(self) -> None:
        """
        Spawn THREAD_COUNT threads all entering ingest_claim simultaneously.
        The GIL MUST be released (py.detach) — otherwise this deadlocks.

        Expected: completes within DEADLOCK_TIMEOUT_SECONDS regardless of errors.
        Note: StorageError from concurrent SQLite transactions is EXPECTED here
        (DEFECT-THREAD-1) but NO deadlock is acceptable.
        """
        engine = mempill.open_in_memory()
        requests = [_make_req(f"agent-{i}", i) for i in range(THREAD_COUNT)]

        _, _, elapsed = _run_threads_barrier(engine, requests)

        assert elapsed < DEADLOCK_TIMEOUT_SECONDS, (
            f"DEADLOCK DETECTED: concurrent ingest did not complete within "
            f"{DEADLOCK_TIMEOUT_SECONDS}s — py.detach() is likely missing from "
            f"one or more PyEngine methods. Elapsed: {elapsed:.2f}s"
        )

    def test_concurrent_ingest_errors_are_mempill_errors(self) -> None:
        """Any exception from concurrent ingest must be a MempillError, never a raw Python crash.

        After DEFECT-THREAD-1 fix, all concurrent writes should succeed (no errors).
        This test still validates that IF any error occurs it is a MempillError —
        and also asserts all writes succeed (zero errors expected post-fix).
        """
        engine = mempill.open_in_memory()
        requests = [_make_req(f"agent-{i}", i) for i in range(THREAD_COUNT)]

        successes, errors, elapsed = _run_threads_barrier(engine, requests)

        assert elapsed < DEADLOCK_TIMEOUT_SECONDS, "DEADLOCK: exceeded timeout"

        unexpected = [e for e in errors if not isinstance(e, MempillError)]
        assert not unexpected, (
            f"Concurrent ingest raised non-MempillError exceptions: {unexpected}"
        )
        assert len(successes) == THREAD_COUNT, (
            f"DEFECT-THREAD-1 fix: all {THREAD_COUNT} concurrent writes must succeed, "
            f"got {len(successes)} successes and errors: {errors}"
        )


# ── Concurrent write correctness (DEFECT-THREAD-1 fixed) ─────────────────────

class TestConcurrentWriteCorrectness:
    """
    Verifies DEFECT-THREAD-1 fix: concurrent writes from different agent_ids
    must ALL succeed (serialize at the store level, not error).

    Fix: store_write_lock (Arc<tokio::sync::Mutex<()>>) in EngineHandle serializes
    all writes across agents before per-agent lock acquisition. Reads remain
    fully concurrent and lock-free.
    """

    def test_concurrent_writes_different_agent_ids_all_succeed(self) -> None:
        """
        DEFECT-THREAD-1 fix verification: concurrent writes with different agent_ids
        must ALL succeed — no StorageErrors from concurrent transactions.

        Previously, 3 of THREAD_COUNT writes would fail with "transaction is already open".
        After the store_write_lock fix, all writes serialize and complete successfully.
        """
        engine = mempill.open_in_memory()
        requests = [_make_req(f"agent-{i}", i) for i in range(THREAD_COUNT)]

        successes, errors, elapsed = _run_threads_barrier(engine, requests)

        assert elapsed < DEADLOCK_TIMEOUT_SECONDS, "DEADLOCK: exceeded timeout"

        storage_errors = [e for e in errors if isinstance(e, StorageError)]
        assert len(storage_errors) == 0, (
            f"DEFECT-THREAD-1: concurrent writes with different agent_ids must not "
            f"produce StorageErrors after store_write_lock fix. Got: {storage_errors}"
        )
        assert len(successes) == THREAD_COUNT, (
            f"All {THREAD_COUNT} concurrent writes must succeed, "
            f"got {len(successes)} successes and {len(errors)} errors: {errors}"
        )

    def test_concurrent_writes_same_agent_id_all_succeed(self) -> None:
        """Same-agent concurrent writes must also serialize and all succeed."""
        engine = mempill.open_in_memory()
        # All writes use the same agent_id but different subjects (no conflict).
        requests = [_make_req("agent-same", i, predicate=f"pred-{i}") for i in range(THREAD_COUNT)]

        successes, errors, elapsed = _run_threads_barrier(engine, requests)

        assert elapsed < DEADLOCK_TIMEOUT_SECONDS, "DEADLOCK: exceeded timeout"
        assert not errors, f"Same-agent concurrent writes must all succeed, got errors: {errors}"
        assert len(successes) == THREAD_COUNT, (
            f"Expected {THREAD_COUNT} results, got {len(successes)}"
        )


# ── Sequential cross-thread tests (must pass) ─────────────────────────────────

class TestSequentialCrossThread:
    """
    Sequential use from different threads must work correctly.
    This validates that the GIL release does not corrupt internal state
    and that the engine is safe for serial thread handoff.
    """

    def test_sequential_different_threads_all_succeed(self) -> None:
        """Thread A finishes completely before Thread B starts — both must succeed."""
        engine = mempill.open_in_memory()
        results = []
        errors = []

        def worker(i: int) -> None:
            req = _make_req(f"agent-seq-{i}", i)
            try:
                results.append(engine.ingest_claim(req))
            except Exception as exc:  # noqa: BLE001
                errors.append(exc)

        # Run threads sequentially (join each before starting next)
        for i in range(THREAD_COUNT):
            t = threading.Thread(target=worker, args=(i,))
            t.start()
            t.join(timeout=DEADLOCK_TIMEOUT_SECONDS)
            assert not t.is_alive(), f"Thread {i} is still alive — deadlock?"

        assert len(errors) == 0, f"Sequential cross-thread writes failed: {errors}"
        assert len(results) == THREAD_COUNT, (
            f"Expected {THREAD_COUNT} results, got {len(results)}"
        )

    def test_query_from_different_thread_than_writer(self) -> None:
        """Write from one thread, read from another — must succeed."""
        engine = mempill.open_in_memory()
        claim_ref_box: list[str] = []
        write_errors: list[Exception] = []
        read_results: list[dict] = []
        read_errors: list[Exception] = []

        def writer() -> None:
            req = _make_req("agent-rw", 0, predicate="city")
            try:
                r = engine.ingest_claim(req)
                claim_ref_box.append(r["claim_ref"])
            except Exception as exc:  # noqa: BLE001
                write_errors.append(exc)

        def reader() -> None:
            try:
                r = engine.query_memory({
                    "agent_id": "agent-rw",
                    "subject": "user-0",
                    "predicate": "city",
                })
                read_results.append(r)
            except Exception as exc:  # noqa: BLE001
                read_errors.append(exc)

        t1 = threading.Thread(target=writer)
        t1.start()
        t1.join(timeout=DEADLOCK_TIMEOUT_SECONDS)
        assert not t1.is_alive(), "Writer thread deadlocked"
        assert not write_errors, f"Write failed: {write_errors}"

        t2 = threading.Thread(target=reader)
        t2.start()
        t2.join(timeout=DEADLOCK_TIMEOUT_SECONDS)
        assert not t2.is_alive(), "Reader thread deadlocked"
        assert not read_errors, f"Read failed: {read_errors}"
        assert read_results, "No read results returned"
