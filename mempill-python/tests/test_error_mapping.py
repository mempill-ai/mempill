"""
test_error_mapping.py — Verifies that each reachable MemError variant maps to the
correct Python exception subclass.

Reachable from the Python public API:
  - MissingProvenance       → ValidationError  (omit provenance key)
  - MalformedFact           → ValidationError  (bad field value, e.g. invalid cardinality)
  - IncoherentTemporalWindow→ ValidationError  (start > end in valid_time)
  - StorageError            → StorageError     (bad path to open())

NOT reachable (documented):
  WriteLockContention, SpawnBlocking, AtomicCommitViolation, MonotonicityViolation,
  BeliefCacheInconsistency, OracleError, AdjudicationHandleNotFound, ConfigurationError,
  WriteAuthorityViolation, Persistence, PragmaInitFailed, UnknownAgentId.

AUDIT PATH NOTE:
  The error table maps ClaimNotFound → NotFoundError, but query_audit with a
  non-existent claim_ref returns {"entries": []} rather than raising NotFoundError.
  This means ClaimNotFound is not reachable from the audit path in v0.2.
"""

from __future__ import annotations

import pytest

import mempill
from mempill import (
    MempillError,
    ValidationError,
    NotFoundError,
    StorageError,
    ConfigError,
    InternalError,
    ConflictError,
)
from mempill.types import ProvenanceLabel


# ── Helpers ───────────────────────────────────────────────────────────────────

def _base_req(agent_id: str) -> dict:
    return {
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "x",
        "value": "v",
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    }


# ── Exception hierarchy tests ─────────────────────────────────────────────────

class TestExceptionHierarchy:
    """All leaf exceptions must subclass MempillError."""

    def test_validation_error_is_mempill_error(self) -> None:
        assert issubclass(ValidationError, MempillError)

    def test_not_found_error_is_mempill_error(self) -> None:
        assert issubclass(NotFoundError, MempillError)

    def test_storage_error_is_mempill_error(self) -> None:
        assert issubclass(StorageError, MempillError)

    def test_config_error_is_mempill_error(self) -> None:
        assert issubclass(ConfigError, MempillError)

    def test_internal_error_is_mempill_error(self) -> None:
        assert issubclass(InternalError, MempillError)

    def test_conflict_error_is_mempill_error(self) -> None:
        assert issubclass(ConflictError, MempillError)

    def test_mempill_error_is_exception(self) -> None:
        assert issubclass(MempillError, Exception)


# ── Reachable error triggers ──────────────────────────────────────────────────

class TestReachableErrors:
    def test_missing_provenance_raises_validation_error(self, engine: mempill.Engine, agent_id: str) -> None:
        """MemError::MissingProvenance → ValidationError (DC-1 check)."""
        req = {
            "agent_id": agent_id,
            "subject": "user",
            "predicate": "city",
            "value": "Berlin",
            # provenance key deliberately omitted
            "cardinality": "Functional",
            "valid_time": None,
            "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
            "criticality": "Medium",
            "derived_from": [],
        }
        with pytest.raises(ValidationError) as exc_info:
            engine.ingest_claim(req)
        assert "provenance" in str(exc_info.value).lower(), (
            f"Error message should mention provenance, got: {exc_info.value}"
        )

    def test_incoherent_temporal_window_raises_validation_error(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """MemError::IncoherentTemporalWindow → ValidationError (start > end)."""
        req = _base_req(agent_id)
        req["valid_time"] = {
            "start": "2030-01-01T00:00:00Z",
            "end": "2020-01-01T00:00:00Z",  # end before start
        }
        with pytest.raises(ValidationError):
            engine.ingest_claim(req)

    def test_invalid_cardinality_raises_validation_error(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Malformed field in request body → ValidationError (bad deserialization)."""
        req = _base_req(agent_id)
        req["cardinality"] = "NotARealCardinality"
        with pytest.raises(ValidationError):
            engine.ingest_claim(req)

    def test_storage_error_on_invalid_path(self) -> None:
        """mempill.open() with unwritable/invalid path → StorageError."""
        with pytest.raises(StorageError):
            mempill.open("/nonexistent_root_dir_mempill/cannot_create.db")

    def test_audit_unknown_claim_ref_returns_empty_not_raises(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """
        Actual engine behaviour: query_audit with unknown claim_ref returns {"entries": []}
        rather than raising NotFoundError.

        This differs from the ARCHITECTURE.md error table which maps ClaimNotFound →
        NotFoundError. The audit query path does not invoke the ClaimNotFound error code
        for unknown refs — it returns an empty result instead.

        Impact: ClaimNotFound (→ NotFoundError) is unreachable from the audit query path.
        Added to the unreachable variant list below.
        """
        fake_uuid = "00000000-0000-0000-0000-000000000000"
        resp = engine.query_audit({
            "agent_id": agent_id,
            "claim_ref": fake_uuid,
            "from_tx_time": None,
            "limit": 10,
        })
        assert resp == {"entries": []}, (
            f"Expected empty entries for unknown claim_ref, got: {resp}"
        )


# ── Documented UNREACHABLE variants ──────────────────────────────────────────
# The following MemError variants are NOT reachable via the public Python API
# in v0.2 without internal test harness or deliberate engine corruption:
#
#   ClaimNotFound             — audit query returns empty list, not NotFoundError
#   WriteLockContention       — StorageError raised from concurrent writes, but not
#                               individually distinguishable from Persistence errors
#   WriteAuthorityViolation   — DC-2: engine auto-namespaces by agent_id
#   SpawnBlocking             — internal tokio task-join failure
#   AtomicCommitViolation     — internal invariant (I9)
#   MonotonicityViolation     — internal invariant (I10)
#   BeliefCacheInconsistency  — internal invariant (I3)
#   OracleError               — no oracle engine in v0.2
#   AdjudicationHandleNotFound— no adjudicator in v0.2
#   ConfigurationError        — OP-3 calibration; not surfaceable via open_in_memory
#   Persistence               — requires SQLite/IO failure (concurrent writes surface
#                               as StorageError but via Persistence variant)
#   PragmaInitFailed          — requires connection-setup failure
#   UnknownAgentId            — engine auto-creates agent records on first use
