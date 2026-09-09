"""
test_joan_diag_reproduction.py — TASK-33-W3-LIB: end-to-end Python reproduction of the
Joan/Linda/John/Sam/Diane CEO-succession shape from DIAG_joan_history.md §1 and
DIAG_silent_succession.md §3 (the acme-corp / ceo real-demo defect that motivated this task).

Scope note: the ORIGINAL demo ledger (DIAG_joan_history.md §1) is a real production trace
spanning two sessions and containing entries produced by the OLD (pre-fix) engine's silent
supersession bug (DIAG_silent_succession (ii)) plus duplicate near-identical re-assertions
that are demo-tool artifacts, not engine behavior. This test reproduces the SAME people,
the SAME subject/predicate ("acme-corp"/"ceo"), and the SAME valid-time windows/precisions
(day/day/day/month/month for Diane/Linda/John/Sam/Joan respectively, matching the ground-
truth table) for the claims that define the bug shape, driven through the FIXED engine's
real ingest/oracle flow — asserting the CORRECT output, not replaying the old buggy ledger.
Diane and Sam are genuine oracle-resolved predecessors/challengers, exercising the full
mixed-history shape named in the task (real supersession + a genuinely pending Contested
overlap coexisting on the same subject-line).

Uses the RAW `engine.ingest_claim()` dict path (not the `remember()` ergonomic helper) so
`start_granularity` / `end_granularity` are set explicitly — `remember()` does not yet
propagate granularity on write (a pre-existing, separately-tracked gap noted in
test_history.py's `_ingest_with_granularity` helper, which this test mirrors).

Deliberate ordering choice: John is ingested BEFORE Joan (Linda -> John forms a clean
non-overlapping succession first; Joan then arrives and overlaps Linda specifically). This
is the exact shape independently reproduced and verified in the Rust conformance/QA suites
(`overlapping_noncurrent_chain_member_is_conflict` in mempill-sqlite,
`hist_overlap_marks_both_entries_contested_no_narrowing` in mempill-core's conformance
harness). The original demo's historical ingest order (Joan before John) reflects the OLD
engine's now-removed 2-claim-only succession check and cannot be faithfully replayed
against the fixed engine.
"""

from __future__ import annotations

import uuid

import mempill
from mempill import History, history, recall
from mempill._mempill import PyOracleEngine
from mempill.types import ProvenanceLabel


class RecordingOracle:
    """Records every adjudication request; returns a stable handle UUID per request."""

    def __init__(self) -> None:
        self.requests: dict[str, dict] = {}

    def request_adjudication(self, agent_id: str, request: dict) -> str:
        handle_id = str(uuid.uuid4())
        self.requests[handle_id] = request
        return handle_id

    def handle_for_challenger(self, value: str) -> str:
        """Find the pending adjudication handle whose challenger value matches."""
        for handle_id, req in self.requests.items():
            if req["challenger"]["fact"]["value"] == value:
                return handle_id
        raise KeyError(f"no pending adjudication request for challenger value {value!r}")


AGENT = "joan-diag-agent"
SUBJECT = "acme-corp"
PREDICATE = "ceo"


def _ingest(
    engine: PyOracleEngine,
    value: str,
    *,
    start: str,
    start_gran: str,
    end: str | None = None,
    end_gran: str | None = None,
) -> dict:
    vt: dict = {"start": start, "start_granularity": start_gran, "valid_time_confidence": 0.9}
    if end is not None:
        vt["end"] = end
        vt["end_granularity"] = end_gran

    return engine.ingest_claim({
        "agent_id": AGENT,
        "subject": SUBJECT,
        "predicate": PREDICATE,
        "value": value,
        "provenance": {"type": "External", "kind": "UserAsserted"},
        "cardinality": "Functional",
        "valid_time": vt,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.9},
        "criticality": "Medium",
        "derived_from": [],
    })


def _submit(engine: PyOracleEngine, handle_id: str, verdict: str) -> dict:
    return engine.submit_adjudication({
        "handle_id": handle_id,
        "verdict": verdict,
        "evidence_provenance": ProvenanceLabel.external_user_asserted(),
    })


def test_joan_linda_john_diane_sam_reproduction() -> None:
    """Full reproduction: Diane -> Linda (real oracle supersession), Linda -> John (clean
    trusted succession, no supersession needed), Sam (loses to Linda via Deny), Joan
    (overlaps Linda -> stays pending QueuedForAdjudication, never resolved).
    """
    oracle = RecordingOracle()
    engine: PyOracleEngine = mempill.open_oracle_in_memory(oracle)

    # ── Diane: first write — day precision, open-ended ────────────────────────
    _ingest(engine, "Diane", start="2021-04-01T00:00:00Z", start_gran="day")

    # ── Linda: overlaps Diane's open-ended window -> QueuedForAdjudication ────
    r_linda = _ingest(
        engine, "Linda",
        start="2024-09-23T00:00:00Z", start_gran="day",
        end="2026-01-24T00:00:00Z", end_gran="day",
    )
    assert r_linda["disposition"] == "QueuedForAdjudication", (
        "Linda overlaps Diane's open-ended window -> must queue for adjudication"
    )
    # Real oracle resolution: Diane is genuinely superseded by Linda.
    handle_diane_linda = oracle.handle_for_challenger("Linda")
    outcome = _submit(engine, handle_diane_linda, "Affirm")
    assert outcome["disposition"] == "CommittedCheap", "Affirm must commit Linda"

    r_diane = recall(engine, AGENT, SUBJECT, PREDICATE)
    assert r_diane.value == "Linda", "Linda must now be the recalled belief"

    # ── Sam: overlaps Linda's tail -> QueuedForAdjudication, then Deny (Sam loses) ──
    r_sam = _ingest(engine, "Sam", start="2025-12-01T00:00:00Z", start_gran="month")
    assert r_sam["disposition"] == "QueuedForAdjudication", "Sam overlaps Linda's tail -> queued"
    handle_sam = oracle.handle_for_challenger("Sam")
    _submit(engine, handle_sam, "Deny")

    # ── John: [2026-01-24, ∞) — Linda's own end EXACTLY matches John's start ──
    # A genuine non-overlapping trusted succession against Linda (the only OTHER raw-live
    # claim at this point — Sam was Denied, not live) -> clean CommittedCheap, no oracle.
    r_john = _ingest(engine, "John", start="2026-01-24T00:00:00Z", start_gran="day")
    assert r_john["disposition"] == "CommittedCheap", (
        "John forms a clean trusted succession against Linda -> CommittedCheap, no escalation"
    )

    # Sanity check (I7 companion invariant): in this clean, unconflicted intermediate state,
    # history has EXACTLY one Current entry (John) — never two silently co-current entries.
    h_before_joan = history(engine, AGENT, SUBJECT, PREDICATE)
    current_before = [e for e in h_before_joan if e.status == "Current"]
    assert len(current_before) == 1, (
        f"exactly one Current entry expected before Joan arrives, got {current_before!r}"
    )
    assert current_before[0].value == "John"

    # ── Joan: [2024-09, 2025-11) — overlaps Linda (NOT John) ──────────────────
    # Non-overlapping against John alone, but genuinely overlaps Linda (the non-current
    # chain member) -> the N-wide succession check (DIAG_silent_succession fix) must
    # catch this: QueuedForAdjudication, never a silently cheap-pathed Succession.
    r_joan = _ingest(
        engine, "Joan",
        start="2024-09-01T00:00:00Z", start_gran="month",
        end="2025-11-01T00:00:00Z", end_gran="month",
    )
    assert r_joan["disposition"] == "QueuedForAdjudication", (
        "Joan overlaps Linda (non-current chain member) -> MUST queue for adjudication, "
        "never a silently cheap-pathed Succession (I7)"
    )
    # Never resolved (matches the real demo trace: still pending).

    # ── query_memory: Contested, no silent primary ────────────────────────────
    r_final = recall(engine, AGENT, SUBJECT, PREDICATE)
    assert r_final.status == "Contested", f"expected Contested, got {r_final.status!r}"
    assert r_final.value is None, "no silent primary while genuinely contested"

    # ── history(): Joan's own explicit end must survive at Month precision ───
    h = history(engine, AGENT, SUBJECT, PREDICATE)
    assert isinstance(h, History)

    joan_entries = [e for e in h if e.value == "Joan"]
    assert len(joan_entries) == 1
    joan = joan_entries[0]
    assert joan.valid_until_display == "2025-11", (
        f"Joan's own explicit end (2025-11) must never be discarded/fabricated, "
        f"got valid_until_display={joan.valid_until_display!r}"
    )
    assert joan.valid_until_granularity == "month"

    # ── Contested surfaces on every raw-live overlapping entry ───────────────
    linda_entries = [e for e in h if e.value == "Linda"]
    john_entries = [e for e in h if e.value == "John"]
    assert len(linda_entries) == 1 and len(john_entries) == 1
    assert linda_entries[0].status == "Contested", "Linda (raw-live, overlapped by Joan) must be Contested"
    assert joan.status == "Contested", "Joan must be Contested"
    assert john_entries[0].status == "Contested", (
        "John is also raw-live on a structurally has_conflict subject-line -> Contested "
        "(the single fold-wide conflict signal, I7 — no entry is silently marked Current "
        "while the line is genuinely contested)"
    )

    # No entry is silently marked Current while the line is contested (I7).
    assert all(e.status != "Current" for e in h), (
        "no entry may be Current while the subject-line is genuinely Contested"
    )

    # Diane and Sam retain their real oracle-resolved dispositions (unaffected by the
    # later, unrelated Joan/Linda/John conflict on the same subject-line).
    diane_entries = [e for e in h if e.value == "Diane"]
    sam_entries = [e for e in h if e.value == "Sam"]
    assert diane_entries[0].status == "Superseded", "Diane was genuinely Bound via Affirm"
    assert sam_entries[0].status != "Current", "Sam lost via Deny — never Current"

    # ── No zero-length windows anywhere in the timeline ───────────────────────
    for e in h:
        if e.valid_from is not None and e.valid_until is not None:
            assert e.valid_from != e.valid_until, (
                f"zero-length window for {e.value!r}: valid_from == valid_until == {e.valid_from!r}"
            )

    # ── query_history agrees with query_memory on conflict ───────────────────
    # query_memory: Contested / no primary. query_history: no Current entry anywhere on
    # this subject-line. Both signals are derived from the SAME fold.has_conflict (I8).
    assert r_final.status == "Contested"
    assert h.current() is None, (
        "history().current() must agree with recall(): no primary while genuinely contested"
    )
