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
