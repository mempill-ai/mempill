"""
test_audit.py — Exercises Engine.query_audit() through the Python API.

Verifies:
  - Ledger entries are created after ingest.
  - Each entry carries the correct agent_id.
  - claim_ref filter returns only matching entries.
  - LedgerEntry shape is complete (entry_id, agent_id, claim_ref, event_kind, disposition, recorded_at).

DOCUMENTED ENGINE BEHAVIOUR (not a defect, but noteworthy):
  query_audit with a non-existent claim_ref returns {"entries": []} rather than
  raising NotFoundError. The error mapping table lists ClaimNotFound → NotFoundError,
  but the audit query path appears to return an empty list for unknown refs rather
  than invoking that error path. Documented in UNREACHABLE variant list.
"""

from __future__ import annotations

import uuid

import pytest

import mempill
from mempill.types import Disposition, ProvenanceLabel


def _ingest(engine: mempill.Engine, agent_id: str, predicate: str = "city",
            value: str = "Berlin") -> dict:
    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "user",
        "predicate": predicate,
        "value": value,
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    })


class TestAuditBasic:
    def test_audit_entries_nonempty_after_ingest(self, engine: mempill.Engine, agent_id: str) -> None:
        _ingest(engine, agent_id)
        resp = engine.query_audit({"agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 50})
        assert "entries" in resp
        assert len(resp["entries"]) >= 1, "Audit must contain at least one entry after ingest"

    def test_audit_entry_agent_id_matches(self, engine: mempill.Engine, agent_id: str) -> None:
        """Ledger entries must record the agent_id that performed the ingest."""
        _ingest(engine, agent_id)
        resp = engine.query_audit({"agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 50})
        for entry in resp["entries"]:
            assert entry["agent_id"] == agent_id, (
                f"Entry agent_id {entry['agent_id']!r} != expected {agent_id!r}"
            )

    def test_audit_entry_shape(self, engine: mempill.Engine, agent_id: str) -> None:
        """Each LedgerEntry dict must have all required fields."""
        _ingest(engine, agent_id)
        resp = engine.query_audit({"agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 50})
        required_keys = {"entry_id", "agent_id", "claim_ref", "event_kind", "disposition", "recorded_at"}
        for entry in resp["entries"]:
            missing = required_keys - entry.keys()
            assert not missing, f"LedgerEntry missing keys: {missing}"

    def test_audit_entry_ids_are_uuids(self, engine: mempill.Engine, agent_id: str) -> None:
        _ingest(engine, agent_id)
        resp = engine.query_audit({"agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 50})
        for entry in resp["entries"]:
            uuid.UUID(entry["entry_id"])   # must not raise
            uuid.UUID(entry["claim_ref"])  # must not raise

    def test_audit_disposition_is_known(self, engine: mempill.Engine, agent_id: str) -> None:
        _ingest(engine, agent_id)
        resp = engine.query_audit({"agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 50})
        known = {d.value for d in Disposition}
        for entry in resp["entries"]:
            assert entry["disposition"] in known, (
                f"Unknown disposition in ledger: {entry['disposition']!r}"
            )


class TestAuditFilters:
    def test_audit_filter_by_claim_ref(self, engine: mempill.Engine, agent_id: str) -> None:
        r1 = _ingest(engine, agent_id, "city", "Berlin")
        _ingest(engine, agent_id, "name", "Alice")

        resp = engine.query_audit({
            "agent_id": agent_id,
            "claim_ref": r1["claim_ref"],
            "from_tx_time": None,
            "limit": 50,
        })
        assert all(e["claim_ref"] == r1["claim_ref"] for e in resp["entries"]), (
            "Filtered audit must return only entries for the specified claim_ref"
        )

    def test_audit_limit_respected(self, engine: mempill.Engine, agent_id: str) -> None:
        for i in range(5):
            _ingest(engine, agent_id, f"prop-{i}", f"val-{i}")

        resp = engine.query_audit({"agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 2})
        assert len(resp["entries"]) <= 2, "Audit must respect the limit parameter"

    def test_audit_nonexistent_claim_ref_returns_empty(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """
        query_audit with an unknown claim_ref returns an empty entries list.

        NOTE: The ARCHITECTURE.md error table maps ClaimNotFound → NotFoundError,
        but the audit query path does not invoke that error code for unknown
        claim_refs — it silently returns {"entries": []}. This is the actual
        engine behaviour. The ClaimNotFound variant remains unreachable from the
        audit query path via the public Python API.
        """
        resp = engine.query_audit({
            "agent_id": agent_id,
            "claim_ref": "00000000-0000-0000-0000-000000000000",
            "from_tx_time": None,
            "limit": 10,
        })
        assert resp == {"entries": []}, (
            f"Expected empty entries for unknown claim_ref, got: {resp}"
        )
