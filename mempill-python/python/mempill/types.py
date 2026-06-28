"""
mempill.types — Ergonomic Python type layer for the mempill SDK.

Provides:
  - Disposition: str-Enum with all 12 variants (matches Rust serde names)
  - ProvenanceLabel: factory helpers returning the wire-shape dict
  - TypedDicts for all 8 DTO shapes (IDE/type-checker hints only)
  - ValidTimeDict: temporal bound with confidence and optional granularity
  - BeliefProjection: typed shape of the belief returned by query_memory
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


class ValidTimeDict(TypedDict, total=False):
    """Temporal bound for a claim's valid-time window.

    All keys are optional; omitting both ``start`` and ``end`` signals open/unknown bounds.

    Fields:
        start: ISO-8601 UTC string marking the start of the valid-time window (inclusive).
               Example: "2023-01-01T00:00:00Z"
        end:   ISO-8601 UTC string marking the end of the valid-time window (exclusive).
               None / omitted means open-ended (still valid as of now).
        valid_time_confidence: Confidence that the asserted time bounds are correct [0.0, 1.0].
                               0.0 = unknown / no time bounds supplied.
        granularity: Optional human-readable precision hint, e.g. "year", "month", "day".
                     The engine does not interpret this value — it is stored verbatim and
                     surfaced to callers for display purposes (planned v0.3 feature).
    """

    start: str
    end: str
    valid_time_confidence: float
    granularity: str


class FactDict(TypedDict):
    """The asserted fact inside a BeliefSlot."""

    subject: str
    predicate: str
    value: Any


class BeliefSlot(TypedDict, total=False):
    """A single candidate in the BeliefProjection (primary or alternative).

    Maps to the Rust ``BeliefSlot`` type returned by ``query_memory``.
    Use ``belief["primary"]["fact"]["value"]`` for the resolved value.
    """

    claim_ref: str          # UUID string of the backing claim
    fact: FactDict          # the asserted fact (subject, predicate, value)
    valid_time: ValidTimeDict
    confidence: ConfidenceDict
    provenance: dict[str, str]   # adjacently-tagged: {"type": ..., "kind"?: ...}
    currency_signal: dict[str, Any]


class BeliefProjection(TypedDict, total=False):
    """Full belief projection returned by engine.query_memory().

    Access pattern (most common):
        resp = engine.query_memory({...})
        value = resp["belief"]["primary"]["fact"]["value"]

    For Contested beliefs, ``primary`` is the first candidate and ``alternatives``
    holds the remaining ones. Check ``resp["belief"]["status"]`` before trusting
    ``primary`` alone.
    """

    status: str             # "Resolved" | "Contested" | "Conflict" | "TimingUncertain" | "NoBelief"
    primary: BeliefSlot     # the canonical live belief (None when status is NoBelief)
    alternatives: list[BeliefSlot]  # competing candidates (non-empty for Contested/Conflict)
    staleness: dict[str, Any]       # {"is_stale": bool, ...}
    currency: str           # top-level currency signal string


class QueryMemoryRequest(TypedDict):
    """Request to engine.query_memory().

    Bi-temporal query — two independent time axes:

    * ``valid_at`` selects the belief *true in the world* at the given real-world
      instant (the valid-time axis).  Example: "What was the CEO on 2023-06-15?"

    * ``as_of_tx_time`` selects what the *system knew* at a given point in its log
      (the transaction-time axis).  Example: "What did the engine know last Tuesday?"

    The two compose independently per the D2 bi-temporal rule: the transaction-time
    filter is applied first, then the valid-time selection narrows the result.

    When neither is set, both axes default to ``now`` (current live belief).
    """

    agent_id: str
    subject: str
    predicate: str
    as_of_tx_time: NotRequired[Optional[str]]  # ISO-8601 UTC string — transaction-time axis
    valid_at: NotRequired[Optional[str]]        # ISO-8601 UTC string — valid-time axis


class QueryMemoryResponse(TypedDict):
    """Response from engine.query_memory() — contains the canonical BeliefProjection."""

    belief: BeliefProjection


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
    "ValidTimeDict",
    "FactDict",
    "BeliefSlot",
    "BeliefProjection",
    "IngestClaimRequest",
    "IngestClaimResponse",
    "QueryMemoryRequest",
    "QueryMemoryResponse",
    "ReconcileRequest",
    "ReconcileResponse",
    "AuditQueryRequest",
    "AuditQueryResponse",
]
