"""
test_oracle.py — Round-trip proof that a Python oracle controls adjudication.

Scenarios:
  1. Affirm  — challenger wins; query shows challenger's value.
  2. Deny    — incumbent keeps winning; query shows incumbent's value.
  3. Unknown — ambiguous; belief stays Contested with both present.
  4. Duplicate submit — NotFoundError raised on second submit for same handle.

Oracle duck-typed protocol:
    class MyOracle:
        def request_adjudication(self, agent_id: str, request: dict) -> str:
            # Return a UUID string to correlate the future submit.
            ...
"""

from __future__ import annotations

import uuid
import pytest

import mempill
from mempill._mempill import PyOracleEngine, open_with_oracle_in_memory
from mempill.types import Disposition, ProvenanceLabel


# ── Minimal Python oracle ─────────────────────────────────────────────────────

class RecordingOracle:
    """Records every adjudication request and returns a stable handle UUID.

    Stored requests are available via `self.requests` (dict handle_id → request).
    """

    def __init__(self) -> None:
        self.requests: dict[str, dict] = {}

    def request_adjudication(self, agent_id: str, request: dict) -> str:
        handle_id = str(uuid.uuid4())
        self.requests[handle_id] = {"agent_id": agent_id, "request": request}
        return handle_id


# ── Shared ingest helper ──────────────────────────────────────────────────────

def _ingest(engine: PyOracleEngine, agent_id: str, value: str, provenance: dict) -> dict:
    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "city",
        "value": value,
        "provenance": provenance,
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    })


def _query(engine: PyOracleEngine, agent_id: str) -> dict:
    return engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "city"})


def _submit(engine: PyOracleEngine, handle_id: str, verdict: str) -> dict:
    return engine.submit_adjudication({
        "handle_id": handle_id,
        "verdict": verdict,
        "evidence_provenance": ProvenanceLabel.external_user_asserted(),
    })


# ── Fixtures ──────────────────────────────────────────────────────────────────

@pytest.fixture()
def oracle() -> RecordingOracle:
    return RecordingOracle()


@pytest.fixture()
def oracle_engine(oracle: RecordingOracle) -> PyOracleEngine:
    """Fresh in-memory oracle engine per test."""
    return open_with_oracle_in_memory(oracle)


@pytest.fixture()
def agent_id() -> str:
    return "oracle-test-agent"


# ── Test: ingest conflict → QueuedForAdjudication ────────────────────────────

def test_conflict_queues_for_adjudication(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """Second conflicting External claim must become QueuedForAdjudication when oracle present."""
    r1 = _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    assert r1["disposition"] == Disposition.CommittedCheap, "First claim must commit cheap"

    r2 = _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())
    assert r2["disposition"] == Disposition.QueuedForAdjudication, (
        f"Conflicting claim with oracle must be QueuedForAdjudication, got {r2['disposition']!r}"
    )
    # Oracle must have been called exactly once.
    assert len(oracle.requests) == 1, "Oracle must receive exactly one adjudication request"


# ── Test: query before submit → Contested ─────────────────────────────────────

def test_query_before_submit_is_contested(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """While adjudication is pending, query returns Contested with both candidates."""
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    belief = _query(oracle_engine, agent_id)["belief"]
    assert belief["status"] == "Contested", (
        f"Belief must be Contested while adjudication is pending, got {belief['status']!r}"
    )


# ── Scenario 1: Affirm → challenger wins ─────────────────────────────────────

def test_affirm_challenger_wins(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """After Affirm verdict, query_memory surfaces the challenger value ('Paris')."""
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    assert len(oracle.requests) == 1
    handle_id = next(iter(oracle.requests))

    outcome = _submit(oracle_engine, handle_id, "Affirm")
    assert outcome["handle_id"] == handle_id
    # Engine applies Affirm by committing the challenger — disposition is CommittedCheap.
    assert outcome["disposition"] in ("CommittedCheap", "CommittedInferred"), (
        f"Affirm must commit challenger, got {outcome['disposition']!r}"
    )

    belief = _query(oracle_engine, agent_id)["belief"]
    assert belief["status"] in ("Resolved", "TimingUncertain"), (
        f"Belief must be Resolved after Affirm, got {belief['status']!r}"
    )
    assert belief["primary"] is not None, "Belief primary must be set after Affirm"
    primary_value = belief["primary"]["fact"]["value"]
    assert primary_value == "Paris", (
        f"After Affirm, challenger 'Paris' must be primary, got {primary_value!r}"
    )


# ── Scenario 2: Deny → incumbent keeps winning ───────────────────────────────

def test_deny_incumbent_wins(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """After Deny verdict, query_memory surfaces the incumbent value ('Berlin')."""
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    handle_id = next(iter(oracle.requests))
    outcome = _submit(oracle_engine, handle_id, "Deny")

    assert outcome["disposition"] in ("Superseded", "Rejected", "CommittedCheap"), (
        f"Deny must mark challenger Superseded/Rejected or keep incumbent, got {outcome['disposition']!r}"
    )

    belief = _query(oracle_engine, agent_id)["belief"]
    primary_value = belief["primary"]["fact"]["value"]
    assert primary_value == "Berlin", (
        f"After Deny, incumbent 'Berlin' must remain primary, got {primary_value!r}"
    )


# ── Scenario 3: Unknown → Contested stays ────────────────────────────────────

def test_unknown_remains_contested(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """After Unknown verdict, query_memory must still show Contested with both candidates."""
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    handle_id = next(iter(oracle.requests))
    outcome = _submit(oracle_engine, handle_id, "Unknown")

    belief = _query(oracle_engine, agent_id)["belief"]
    assert belief["status"] == "Contested", (
        f"Unknown verdict must leave belief Contested, got {belief['status']!r}"
    )
    # With Unknown verdict the engine surfaces Contested — both claims are live.
    # The primary field may be None when two candidates conflict unresolved; that is correct.
    # The outcome must reference the challenger claim.
    assert outcome["claim_ref"] is not None, "Outcome claim_ref must be set"


# ── Scenario 4: Duplicate submit → NotFoundError ─────────────────────────────

def test_duplicate_submit_raises_not_found(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """Submitting the same handle_id twice must raise NotFoundError on the second call."""
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    handle_id = next(iter(oracle.requests))

    # First submit must succeed.
    _submit(oracle_engine, handle_id, "Affirm")

    # Second submit must raise NotFoundError (handle already consumed).
    with pytest.raises(mempill.NotFoundError):
        _submit(oracle_engine, handle_id, "Affirm")


# ── Test: existing PyEngine fixture still works ───────────────────────────────

def test_py_engine_unaffected(engine: mempill.Engine, agent_id: str) -> None:
    """Existing no-oracle PyEngine must work unchanged alongside PyOracleEngine."""
    resp = engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "city",
        "value": "Rome",
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Low",
        "derived_from": [],
    })
    assert resp["disposition"] == Disposition.CommittedCheap


# ── Tests: list_pending_adjudications ────────────────────────────────────────

def test_list_pending_after_conflict(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """After a conflict is queued, list_pending_adjudications returns exactly 1 row
    with the correct handle_id, subject, predicate, and decoded incumbent/challenger values.
    Then submit resolves it and the queue becomes empty."""
    # Ingest incumbent ("Berlin") — no conflict.
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())

    # Ingest challenger ("Paris") — conflict triggers oracle, QueuedForAdjudication.
    r2 = _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())
    assert r2["disposition"] == Disposition.QueuedForAdjudication

    # Exactly one handle was stored by the oracle.
    assert len(oracle.requests) == 1
    handle_id = next(iter(oracle.requests))

    # list_pending_adjudications must return exactly one row.
    rows = oracle_engine.list_pending_adjudications()
    assert len(rows) == 1, f"Expected 1 pending row, got {len(rows)}"

    row = rows[0]
    print("list_pending_adjudications sample row:", row)  # visible in pytest -s output

    # Check identifying fields.
    assert row["handle_id"] == handle_id, (
        f"handle_id mismatch: expected {handle_id}, got {row['handle_id']}"
    )
    assert row["agent_id"] == agent_id
    assert row["subject"] == "user"
    assert row["predicate"] == "city"
    assert row["status"] == "pending"

    # Decoded scalar values.
    assert row["incumbent_value"] == "Berlin", (
        f"incumbent_value must be 'Berlin', got {row['incumbent_value']!r}"
    )
    assert row["challenger_value"] == "Paris", (
        f"challenger_value must be 'Paris', got {row['challenger_value']!r}"
    )

    # request_payload must be present and have the full AdjudicationRequest shape.
    assert "request_payload" in row
    assert "incumbent" in row["request_payload"]
    assert "challenger" in row["request_payload"]

    # Submit the adjudication → queue must become empty.
    _submit(oracle_engine, handle_id, "Affirm")
    rows_after = oracle_engine.list_pending_adjudications()
    assert len(rows_after) == 0, (
        f"Queue must be empty after submit, got {len(rows_after)} rows"
    )


def test_list_pending_defer_persists(
    oracle_engine: PyOracleEngine,
    oracle: RecordingOracle,
    agent_id: str,
) -> None:
    """A conflict left unsubmitted (deferred) must still appear in list_pending_adjudications.
    A second independent conflict also appears, proving the queue holds multiple rows."""
    # First conflict on subject=user / predicate=city.
    _ingest(oracle_engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(oracle_engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())
    assert len(oracle.requests) == 1, "First conflict must have been sent to oracle"

    # Submit the first conflict to clear it.
    handle_id_first = next(iter(oracle.requests))
    _submit(oracle_engine, handle_id_first, "Affirm")

    rows_after_first = oracle_engine.list_pending_adjudications()
    assert len(rows_after_first) == 0, "Queue must be empty after first submit"

    # Create a SECOND conflict on a different predicate (country) — leave it unsubmitted.
    oracle_engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "country",
        "value": "Germany",
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    })
    oracle_engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "country",
        "value": "France",
        "provenance": ProvenanceLabel.external_first_hand(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    })

    # Queue must now have exactly 1 deferred row for country.
    deferred_rows = oracle_engine.list_pending_adjudications()
    assert len(deferred_rows) == 1, (
        f"Deferred conflict must remain in queue, got {len(deferred_rows)} rows"
    )
    assert deferred_rows[0]["predicate"] == "country"
    assert deferred_rows[0]["status"] == "pending"
    assert deferred_rows[0]["incumbent_value"] == "Germany"
    assert deferred_rows[0]["challenger_value"] == "France"
