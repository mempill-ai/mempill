"""
test_enum_mapping.py — Verifies all 12 Disposition variants and all ProvenanceLabel
factory methods.

Ensures:
  - All 12 Disposition variants exist and their .value matches the serde string.
  - ProvenanceLabel factories return the correct wire-shape dict.
  - ProvenanceLabel wire dicts survive a full ingest → query round-trip.
"""

from __future__ import annotations

import mempill
from mempill.types import Disposition, ProvenanceLabel


# ── Disposition ───────────────────────────────────────────────────────────────

EXPECTED_DISPOSITIONS = {
    "CommittedCheap",
    "CommittedInferred",
    "QueuedForAdjudication",
    "Contested",
    "PendingConflict",
    "PendingReview",
    "PendingLowConfidence",
    "Quarantined",
    "Superseded",
    "Invalidated",
    "Reinstated",
    "Rejected",
}


class TestDispositionEnum:
    def test_all_12_variants_exist(self) -> None:
        actual = {d.value for d in Disposition}
        assert actual == EXPECTED_DISPOSITIONS, (
            f"Missing variants: {EXPECTED_DISPOSITIONS - actual}\n"
            f"Extra variants: {actual - EXPECTED_DISPOSITIONS}"
        )

    def test_exactly_12_variants(self) -> None:
        assert len(Disposition) == 12, f"Expected 12 Disposition variants, got {len(Disposition)}"

    def test_disposition_is_str_subclass(self) -> None:
        """Disposition is str,Enum so comparisons with raw engine strings work."""
        assert isinstance(Disposition.CommittedCheap, str)

    def test_each_variant_string_value_matches_name(self) -> None:
        """For each variant, .value == member name (serde unit-enum convention)."""
        for d in Disposition:
            assert d.value == d.name, (
                f"Disposition.{d.name}.value should be {d.name!r}, got {d.value!r}"
            )

    def test_disposition_comparison_with_raw_string(self) -> None:
        for d in Disposition:
            assert d == d.value, f"Disposition.{d.name} != {d.value!r}"
            assert d.value == d, f"{d.value!r} != Disposition.{d.name}"


# ── ProvenanceLabel ───────────────────────────────────────────────────────────

class TestProvenianceLabelFactories:
    def test_external_user_asserted_shape(self) -> None:
        p = ProvenanceLabel.external_user_asserted()
        assert p == {"type": "External", "kind": "UserAsserted"}

    def test_external_first_hand_shape(self) -> None:
        p = ProvenanceLabel.external_first_hand()
        assert p == {"type": "External", "kind": "ExternalFirstHand"}

    def test_recall_re_entry_shape(self) -> None:
        p = ProvenanceLabel.recall_re_entry()
        assert p == {"type": "RecallReEntry"}

    def test_model_derived_shape(self) -> None:
        p = ProvenanceLabel.model_derived()
        assert p == {"type": "ModelDerived"}

    def test_all_factories_return_dict(self) -> None:
        for factory in [
            ProvenanceLabel.external_user_asserted,
            ProvenanceLabel.external_first_hand,
            ProvenanceLabel.recall_re_entry,
            ProvenanceLabel.model_derived,
        ]:
            result = factory()
            assert isinstance(result, dict), f"{factory.__name__} must return dict"
            assert "type" in result, f"{factory.__name__} result must have 'type' key"

    def test_external_factories_have_kind_key(self) -> None:
        for factory in [
            ProvenanceLabel.external_user_asserted,
            ProvenanceLabel.external_first_hand,
        ]:
            result = factory()
            assert "kind" in result, f"{factory.__name__} result must have 'kind' key"

    def test_non_external_factories_lack_kind_key(self) -> None:
        for factory in [ProvenanceLabel.recall_re_entry, ProvenanceLabel.model_derived]:
            result = factory()
            assert "kind" not in result, (
                f"{factory.__name__} result must NOT have 'kind' key, got {result}"
            )


# ── Round-trip tests (provenance through engine) ──────────────────────────────

class TestProvenanceLabelRoundTrip:
    def _ingest(self, engine: mempill.Engine, agent_id: str, predicate: str, provenance: dict) -> dict:
        return engine.ingest_claim({
            "agent_id": agent_id,
            "subject": "user",
            "predicate": predicate,
            "value": "test-value",
            "provenance": provenance,
            "cardinality": "Functional",
            "valid_time": None,
            "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
            "criticality": "Medium",
            "derived_from": [],
        })

    def test_external_user_asserted_round_trips(self, engine: mempill.Engine, agent_id: str) -> None:
        prov = ProvenanceLabel.external_user_asserted()
        resp = self._ingest(engine, agent_id, "prop-ua", prov)
        assert resp["disposition"] == Disposition.CommittedCheap
        qr = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "prop-ua"})
        stored_prov = qr["belief"]["primary"]["provenance"]
        assert stored_prov["type"] == "External"
        assert stored_prov["kind"] == "UserAsserted"

    def test_external_first_hand_round_trips(self, engine: mempill.Engine, agent_id: str) -> None:
        prov = ProvenanceLabel.external_first_hand()
        resp = self._ingest(engine, agent_id, "prop-efh", prov)
        assert resp["disposition"] == Disposition.CommittedCheap
        qr = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "prop-efh"})
        stored_prov = qr["belief"]["primary"]["provenance"]
        assert stored_prov["type"] == "External"
        assert stored_prov["kind"] == "ExternalFirstHand"

    def test_recall_re_entry_round_trips(self, engine: mempill.Engine, agent_id: str) -> None:
        """RecallReEntry must ingest without error and round-trip its provenance type."""
        prov = ProvenanceLabel.recall_re_entry()
        resp = self._ingest(engine, agent_id, "prop-rre", prov)
        # Disposition may vary by engine policy; ensure no exception and claim_ref exists.
        assert "claim_ref" in resp
        qr = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "prop-rre"})
        stored_prov = qr["belief"]["primary"]["provenance"]
        assert stored_prov["type"] == "RecallReEntry"

    def test_model_derived_round_trips(self, engine: mempill.Engine, agent_id: str) -> None:
        prov = ProvenanceLabel.model_derived()
        resp = self._ingest(engine, agent_id, "prop-md", prov)
        assert "claim_ref" in resp
        qr = engine.query_memory({"agent_id": agent_id, "subject": "user", "predicate": "prop-md"})
        stored_prov = qr["belief"]["primary"]["provenance"]
        assert stored_prov["type"] == "ModelDerived"
