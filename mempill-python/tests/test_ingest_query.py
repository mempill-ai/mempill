"""
test_ingest_query.py — Basic ingest→query round-trip through the Python binding.

Covers:
  - Engine.ingest_claim() returns CommittedCheap for a clean External UserAsserted claim.
  - Engine.query_memory() returns the same value.
  - Disposition string compares equal to Disposition enum member.
"""

from __future__ import annotations

import mempill
from mempill.types import Disposition, ProvenanceLabel


def test_ingest_returns_committed_cheap(engine: mempill.Engine, base_ingest: dict) -> None:
    resp = engine.ingest_claim(base_ingest)

    assert "claim_ref" in resp, "Response must contain claim_ref UUID"
    assert isinstance(resp["claim_ref"], str) and len(resp["claim_ref"]) == 36
    assert resp["disposition"] == Disposition.CommittedCheap
    assert resp["contested_with"] == []


def test_query_returns_correct_value(engine: mempill.Engine, base_ingest: dict, agent_id: str) -> None:
    engine.ingest_claim(base_ingest)

    qresp = engine.query_memory({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "city",
    })

    belief = qresp["belief"]
    assert belief is not None
    primary = belief["primary"]
    assert primary["fact"]["value"] == "Berlin"
    assert primary["fact"]["subject"] == "user"
    assert primary["fact"]["predicate"] == "city"


def test_disposition_enum_comparison(engine: mempill.Engine, base_ingest: dict) -> None:
    """Disposition(str, Enum) must compare equal to the plain string from the engine."""
    resp = engine.ingest_claim(base_ingest)
    # Both directions must hold.
    assert resp["disposition"] == "CommittedCheap"
    assert resp["disposition"] == Disposition.CommittedCheap
    assert Disposition.CommittedCheap == resp["disposition"]


def test_claim_ref_is_uuid_string(engine: mempill.Engine, base_ingest: dict) -> None:
    """ClaimRef must come back as a plain UUID string, not a wrapped object."""
    import uuid
    resp = engine.ingest_claim(base_ingest)
    # Must not raise — proves #[serde(transparent)] on ClaimRef is working.
    parsed = uuid.UUID(resp["claim_ref"])
    assert str(parsed) == resp["claim_ref"]


def test_provenance_round_trips_in_query(engine: mempill.Engine, base_ingest: dict, agent_id: str) -> None:
    """Provenance written as External/UserAsserted must read back with same type+kind."""
    engine.ingest_claim(base_ingest)
    qresp = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "city"})
    prov = qresp["belief"]["primary"]["provenance"]
    assert prov["type"] == "External"
    assert prov["kind"] == "UserAsserted"
