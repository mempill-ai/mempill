"""
test_contested.py — Re-proves v0.1 B11/HeavyPath through the Python stack.

Two conflicting External claims on the same (subject, predicate, Functional):
  - Claim A: External/UserAsserted  → CommittedCheap (first writer wins cheap path)
  - Claim B: External/ExternalFirstHand (different value) → Contested, contested_with=[A]
  - query_memory → belief.status == "Contested"
  - Both values are surfaced in primary + belief; NOT a silent single-pick.
"""

from __future__ import annotations

import mempill
from mempill.types import Disposition, ProvenanceLabel


def _ingest(engine: mempill.Engine, agent_id: str, value: str, provenance: dict) -> dict:
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


def test_second_claim_is_contested(engine: mempill.Engine, agent_id: str) -> None:
    r1 = _ingest(engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    assert r1["disposition"] == Disposition.CommittedCheap, "First claim must commit cheap"

    r2 = _ingest(engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())
    assert r2["disposition"] == Disposition.Contested, (
        f"Conflicting External claim must be Contested, got {r2['disposition']!r}"
    )
    assert r1["claim_ref"] in r2["contested_with"], (
        "contested_with must reference the first claim"
    )


def test_contested_claim_ref_list_nonempty(engine: mempill.Engine, agent_id: str) -> None:
    _ingest(engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    r2 = _ingest(engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())
    assert len(r2["contested_with"]) >= 1, "contested_with must be non-empty on conflict"


def test_query_belief_status_is_contested(engine: mempill.Engine, agent_id: str) -> None:
    _ingest(engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    _ingest(engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    qresp = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "city"})
    belief = qresp["belief"]
    assert belief["status"] == "Contested", (
        f"Belief status must be Contested after conflict, got {belief['status']!r}"
    )


def test_both_values_surfaced_not_silently_picked(engine: mempill.Engine, agent_id: str) -> None:
    """Ensures the engine does NOT silently resolve to one value — both must be present."""
    r1 = _ingest(engine, agent_id, "Berlin", ProvenanceLabel.external_user_asserted())
    r2 = _ingest(engine, agent_id, "Paris", ProvenanceLabel.external_first_hand())

    qresp = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "city"})
    belief = qresp["belief"]

    # For a Contested belief the projection sets primary=None and places BOTH conflicting
    # claims in alternatives (no silent pick). primary must be absent.
    assert belief["primary"] is None, (
        "Contested belief must NOT have a primary — that would be a silent pick"
    )

    # The contested disposition must still be present.
    assert belief["status"] == "Contested", "Contested status must survive query"

    # Both values must appear in alternatives — this is the headline guarantee.
    alt_values = [alt["fact"]["value"] for alt in belief["alternatives"]]
    assert "Berlin" in alt_values, f"'Berlin' missing from alternatives: {alt_values}"
    assert "Paris" in alt_values, f"'Paris' missing from alternatives: {alt_values}"

    # Both claim_refs must be traceable: ingest r2 references r1 as contested.
    assert r2["claim_ref"] != r1["claim_ref"], "Two distinct claims must have distinct refs"
