"""
mempill.types — Ergonomic Python type layer for the mempill SDK.

Provides:
  - Disposition: str-Enum with all 12 variants (matches Rust serde names)
  - ProvenanceLabel: factory helpers returning the wire-shape dict
  - TypedDicts for all 8 DTO shapes (IDE/type-checker hints only)
"""

from __future__ import annotations

import enum
from typing import Any, Optional


# ── Disposition ───────────────────────────────────────────────────────────────

class Disposition(str, enum.Enum):
    """The 12-state write-outcome model (SDK_CONTRACT §9, B3a/B3b).

    Values match the Rust serde serialisation exactly, so:
        resp["disposition"] == Disposition.CommittedCheap
    evaluates to True when the response dict comes from the engine.
    """

    CommittedCheap = "CommittedCheap"
    CommittedInferred = "CommittedInferred"
    QueuedForAdjudication = "QueuedForAdjudication"
    Contested = "Contested"
    PendingConflict = "PendingConflict"
    PendingReview = "PendingReview"
    PendingLowConfidence = "PendingLowConfidence"
    Quarantined = "Quarantined"
    Superseded = "Superseded"
    Invalidated = "Invalidated"
    Reinstated = "Reinstated"
    Rejected = "Rejected"


# ── ProvenanceLabel ───────────────────────────────────────────────────────────

class ProvenanceLabel:
    """Factory for provenance dicts matching the Rust adjacently-tagged wire shape.

    Wire shapes (serde tag="type", content="kind"):
      External(UserAsserted)    → {"type": "External", "kind": "UserAsserted"}
      External(ExternalFirstHand) → {"type": "External", "kind": "ExternalFirstHand"}
      RecallReEntry             → {"type": "RecallReEntry"}
      ModelDerived              → {"type": "ModelDerived"}

    Usage:
        prov = ProvenanceLabel.external_user_asserted()
        prov = ProvenanceLabel.external_first_hand()
        prov = ProvenanceLabel.recall_re_entry()
        prov = ProvenanceLabel.model_derived()
    """

    # ── ExternalKind constants ────────────────────────────────────────────────

    USER_ASSERTED: str = "UserAsserted"
    EXTERNAL_FIRST_HAND: str = "ExternalFirstHand"

    @staticmethod
    def external_user_asserted() -> dict[str, str]:
        """First-hand human assertion (user as oracle). Cheap-path eligible."""
        return {"type": "External", "kind": "UserAsserted"}

    @staticmethod
    def external_first_hand() -> dict[str, str]:
        """First-hand external evidence (tool result, system-of-record, sensor). Cheap-path eligible."""
        return {"type": "External", "kind": "ExternalFirstHand"}

    @staticmethod
    def recall_re_entry() -> dict[str, str]:
        """Content the engine previously served, re-entering the write path (X2 loop).
        Caught by the Amplification Guard (C6). Corroborates by identity; never becomes ground truth.
        """
        return {"type": "RecallReEntry"}

    @staticmethod
    def model_derived() -> dict[str, str]:
        """Model-emitted / inferred content. Mandatory default for model output.
        Committed down-weighted; ineligible to overturn until anchored.
        """
        return {"type": "ModelDerived"}


# ── TypedDicts ────────────────────────────────────────────────────────────────
# These mirror the 8 DTO shapes in mempill-core/src/application/dto.rs.
# They are for IDE assistance / mypy only; the engine accepts plain dicts.

from typing import TypedDict, NotRequired


class ConfidenceDict(TypedDict):
    """Two-score confidence (SDK_CONTRACT §1.4, B2)."""
    value_confidence: float
    valid_time_confidence: float


class IngestClaimRequest(TypedDict):
    """Public write request matching IngestClaimRequest DTO."""
    agent_id: str
    subject: str
    predicate: str
    value: Any
    provenance: dict[str, str]
    cardinality: str  # "Functional" | "SetValued" | "Unknown"
    valid_time: NotRequired[Optional[dict[str, Any]]]
    confidence: ConfidenceDict
    criticality: str  # "Low" | "Medium" | "High" | "Critical"
    derived_from: list[str]  # list of UUID strings


class IngestClaimResponse(TypedDict):
    """Response from engine.ingest_claim()."""
    claim_ref: str  # UUID string
    disposition: str  # Disposition variant name
    contested_with: list[str]  # list of UUID strings


class QueryMemoryRequest(TypedDict):
    """Request to engine.query_memory()."""
    agent_id: str
    subject: str
    predicate: str
    as_of_tx_time: NotRequired[Optional[str]]  # ISO-8601 UTC string


class QueryMemoryResponse(TypedDict):
    """Response from engine.query_memory() — contains the canonical BeliefProjection."""
    belief: dict[str, Any]


class ReconcileRequest(TypedDict):
    """Request to engine.reconcile()."""
    agent_id: str
    subject_lines: list[tuple[str, str]]  # list of (subject, predicate) pairs


class ReconcileResponse(TypedDict):
    """Response from engine.reconcile()."""
    outcomes: list[tuple[str, str]]  # list of (claim_ref_uuid, disposition) pairs
    oracle_escalations: int


class AuditQueryRequest(TypedDict):
    """Request to engine.query_audit()."""
    agent_id: str
    claim_ref: NotRequired[Optional[str]]  # UUID string or None
    from_tx_time: NotRequired[Optional[str]]  # ISO-8601 UTC string or None
    limit: int


class AuditQueryResponse(TypedDict):
    """Response from engine.query_audit()."""
    entries: list[dict[str, Any]]  # list of LedgerEntry dicts


__all__ = [
    "Disposition",
    "ProvenanceLabel",
    "ConfidenceDict",
    "IngestClaimRequest",
    "IngestClaimResponse",
    "QueryMemoryRequest",
    "QueryMemoryResponse",
    "ReconcileRequest",
    "ReconcileResponse",
    "AuditQueryRequest",
    "AuditQueryResponse",
]
