"""
test_reconcile.py — Exercises Engine.reconcile() through the Python API.

Scenario: ingest two conflicting claims (Contested), then reconcile.
Assert that outcomes list is non-empty and each outcome is a
(claim_ref_string, disposition_string) pair.
"""

from __future__ import annotations

import mempill
from mempill.types import Disposition, ProvenanceLabel


def _ingest(engine: mempill.Engine, agent_id: str, predicate: str, value: str, prov: dict) -> dict:
    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": predicate,
        "value": value,
        "provenance": prov,
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    })


def test_reconcile_contested_returns_outcomes(engine: mempill.Engine, agent_id: str) -> None:
    """After a conflict, reconcile must return at least one outcome."""
    _ingest(engine, agent_id, "city", "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(engine, agent_id, "city", "Paris", ProvenanceLabel.external_first_hand())

    resp = engine.reconcile({
        "agent_id": agent_id,
        "subject_lines": [["user", "city"]],
    })

    assert "outcomes" in resp, "Reconcile response must have 'outcomes' key"
    assert len(resp["outcomes"]) >= 1, "At least one outcome must be returned after conflict"


def test_reconcile_outcomes_shape(engine: mempill.Engine, agent_id: str) -> None:
    """Each outcome must be a 2-tuple (claim_ref_str, disposition_str)."""
    import uuid

    _ingest(engine, agent_id, "city", "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(engine, agent_id, "city", "Paris", ProvenanceLabel.external_first_hand())

    resp = engine.reconcile({
        "agent_id": agent_id,
        "subject_lines": [["user", "city"]],
    })

    for outcome in resp["outcomes"]:
        claim_ref_str, disposition_str = outcome
        # claim_ref must be a valid UUID string
        uuid.UUID(claim_ref_str)
        # disposition must be a known Disposition name
        assert disposition_str in {d.value for d in Disposition}, (
            f"Unknown disposition in outcome: {disposition_str!r}"
        )


def test_reconcile_oracle_escalations_is_int(engine: mempill.Engine, agent_id: str) -> None:
    """oracle_escalations must be a non-negative integer."""
    _ingest(engine, agent_id, "name", "Alice", ProvenanceLabel.external_user_asserted())

    resp = engine.reconcile({
        "agent_id": agent_id,
        "subject_lines": [["user", "name"]],
    })

    assert isinstance(resp["oracle_escalations"], int)
    assert resp["oracle_escalations"] >= 0


def test_reconcile_no_conflict_returns_stable_outcome(engine: mempill.Engine, agent_id: str) -> None:
    """Reconciling an already-committed (non-contested) subject line succeeds."""
    _ingest(engine, agent_id, "name", "Alice", ProvenanceLabel.external_user_asserted())

    resp = engine.reconcile({
        "agent_id": agent_id,
        "subject_lines": [["user", "name"]],
    })

    assert "outcomes" in resp
    # At minimum, the reconcile must complete without raising.


def test_reconcile_multiple_subject_lines(engine: mempill.Engine, agent_id: str) -> None:
    """Reconcile accepts a list of multiple (subject, predicate) pairs."""
    _ingest(engine, agent_id, "city", "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(engine, agent_id, "age", "30", ProvenanceLabel.external_user_asserted())
    _ingest(engine, agent_id, "city", "Paris", ProvenanceLabel.external_first_hand())

    resp = engine.reconcile({
        "agent_id": agent_id,
        "subject_lines": [["user", "city"], ["user", "age"]],
    })

    assert "outcomes" in resp
    assert isinstance(resp["outcomes"], list)
