"""
mempill_mcp.tools — The 4 MCP tools wrapping the mempill Engine.

All tools pull (engine, agent_id) from the lifespan context via the MCP
Context object: ctx.request_context.lifespan_context.

status_reason surfacing:
  When ingest_claim or query_memory returns a disposition in the
  non-committed set {Contested, PendingConflict, PendingReview,
  Quarantined, QueuedForAdjudication}, an explanatory "status_reason"
  string is added to the returned dict so the MCP client can understand
  why the belief was not directly committed.
"""

from __future__ import annotations

from typing import Any, Optional

from mcp.server.fastmcp import Context

from mempill import ProvenanceLabel
from mempill.types import Disposition

from mempill_mcp.server import mcp


# ── Disposition → human-readable reason map ───────────────────────────────────

_STATUS_REASONS: dict[str, str] = {
    Disposition.Contested: (
        "This claim conflicts with one or more existing beliefs of equal or "
        "higher confidence. It is held in contested state until reconciliation "
        "resolves which belief is authoritative."
    ),
    Disposition.PendingConflict: (
        "A conflict was detected during ingestion. The claim is queued pending "
        "conflict resolution before it can be committed."
    ),
    Disposition.PendingReview: (
        "The claim was flagged for manual or oracle review before it can be "
        "committed. This may occur due to low confidence or criticality thresholds."
    ),
    Disposition.Quarantined: (
        "The claim was quarantined due to provenance or amplification-guard "
        "constraints. It cannot be committed until explicitly cleared."
    ),
    Disposition.QueuedForAdjudication: (
        "The claim is queued for adjudication. An automated or oracle process "
        "must evaluate it before the outcome is finalised."
    ),
}

_NON_COMMITTED = frozenset(_STATUS_REASONS.keys())


def _maybe_add_status_reason(result: dict[str, Any]) -> dict[str, Any]:
    """Annotate result with status_reason if disposition is non-committed."""
    disposition = result.get("disposition") or result.get("belief", {}).get("status")
    if disposition and disposition in _NON_COMMITTED:
        result = dict(result)
        result["status_reason"] = _STATUS_REASONS[disposition]
    return result


def _normalise_provenance(provenance: Any) -> dict[str, str]:
    """Accept a friendly string or dict and return a wire-shape provenance dict.

    Accepted string forms (case-insensitive, separator-tolerant):
      "External:UserAsserted"      → ProvenanceLabel.external_user_asserted()
      "External:ExternalFirstHand" → ProvenanceLabel.external_first_hand()
      "RecallReEntry"              → ProvenanceLabel.recall_re_entry()
      "ModelDerived"               → ProvenanceLabel.model_derived()

    If a dict is passed, it is returned as-is (the engine validates it).

    IMPORTANT: the string is split on ':' FIRST so that "External:ExternalFirstHand"
    is parsed as type="External", kind="ExternalFirstHand" — NOT collapsed into the
    single key "externalexternalfirsthand" (the previous double-external bug).
    """
    if isinstance(provenance, dict):
        return provenance

    if not isinstance(provenance, str):
        raise ValueError(
            f"provenance must be a dict or string, got {type(provenance).__name__!r}"
        )

    # Normalise the raw string: strip whitespace, lowercase, remove separators.
    raw = provenance.strip()

    # Split on ':' first to separate type prefix from kind suffix.
    parts = raw.split(":", 1)
    ptype = parts[0].lower().replace("-", "").replace("_", "")

    if ptype == "external":
        if len(parts) < 2:
            raise ValueError(
                f"Unknown provenance string {provenance!r}. "
                "Accepted values: 'External:UserAsserted', 'External:ExternalFirstHand', "
                "'RecallReEntry', 'ModelDerived'."
            )
        kind = parts[1].lower().replace("-", "").replace("_", "")
        kind_mapping = {
            "userasserted": ProvenanceLabel.external_user_asserted(),
            "externalfirsthand": ProvenanceLabel.external_first_hand(),
        }
        if kind not in kind_mapping:
            raise ValueError(
                f"Unknown provenance string {provenance!r}. "
                "Accepted values: 'External:UserAsserted', 'External:ExternalFirstHand', "
                "'RecallReEntry', 'ModelDerived'."
            )
        return kind_mapping[kind]

    # Simple (no-colon) provenance types.
    simple_mapping = {
        "recallreentry": ProvenanceLabel.recall_re_entry(),
        "modelderived": ProvenanceLabel.model_derived(),
    }
    if ptype not in simple_mapping:
        raise ValueError(
            f"Unknown provenance string {provenance!r}. "
            "Accepted values: 'External:UserAsserted', 'External:ExternalFirstHand', "
            "'RecallReEntry', 'ModelDerived'."
        )
    return simple_mapping[ptype]


# ── Tool 1: ingest_claim ──────────────────────────────────────────────────────

@mcp.tool()
async def ingest_claim(
    subject: str,
    predicate: str,
    value: Any,
    provenance: Any,
    cardinality: str = "Functional",
    confidence_value: float = 0.9,
    confidence_valid_time: float = 0.9,
    criticality: str = "Low",
    valid_time: Optional[dict[str, Any]] = None,
    derived_from: Optional[list[str]] = None,
    ctx: Context = None,
) -> dict[str, Any]:
    """Write a belief claim to the mempill engine.

    Args:
        subject: The entity the claim is about (e.g. "user:alice").
        predicate: The property being asserted (e.g. "age", "location").
        value: The claimed value (any JSON-serialisable type).
        provenance: Wire-shape dict {"type": ..., "kind"?: ...} or a friendly
            string: "External:UserAsserted", "External:ExternalFirstHand",
            "RecallReEntry", or "ModelDerived".
        cardinality: "Functional" (one value), "SetValued" (multiple), or "Unknown".
        confidence_value: Value confidence in [0, 1]. Default 0.9.
        confidence_valid_time: Temporal confidence in [0, 1]. Default 0.9.
        criticality: "Low", "Medium", "High", or "Critical".
        valid_time: Optional temporal bound {"start"?: ISO-8601, "end"?: ISO-8601}.
        derived_from: Optional list of source claim UUIDs.

    Returns:
        {"claim_ref": str, "disposition": str, "contested_with": [str], ...}
        A "status_reason" field is added when disposition is non-committed.
    """
    lc = ctx.request_context.lifespan_context
    engine = lc["engine"]
    agent_id = lc["agent_id"]

    request = {
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "provenance": _normalise_provenance(provenance),
        "cardinality": cardinality,
        "confidence": {
            "value_confidence": confidence_value,
            "valid_time_confidence": confidence_valid_time,
        },
        "criticality": criticality,
        "derived_from": derived_from or [],
    }
    if valid_time is not None:
        request["valid_time"] = valid_time

    result: dict[str, Any] = engine.ingest_claim(request)
    return _maybe_add_status_reason(result)


# ── Tool 2: query_memory ──────────────────────────────────────────────────────

@mcp.tool()
async def query_memory(
    subject: str,
    predicate: str,
    as_of_tx_time: Optional[str] = None,
    valid_at: Optional[str] = None,
    ctx: Context = None,
) -> dict[str, Any]:
    """Query the canonical belief for a (subject, predicate) pair.

    Bi-temporal query — two independent time axes:

    * ``valid_at`` selects the belief that was *true in the world* at the given
      real-world instant.  Example: "What was the CEO on 2023-06-15?"
      This is the *valid-time* axis (D2 bi-temporal rule).

    * ``as_of_tx_time`` selects what the *system knew* at a given point in its
      own log.  Example: "What did the engine believe last Tuesday before the
      correction was ingested?"
      This is the *transaction-time* axis.

    The two can be combined independently:
        ``valid_at="2023-06-15T00:00:00Z", as_of_tx_time="2024-01-01T00:00:00Z"``
    asks "What did the system believe on 2024-01-01 about what was true on
    2023-06-15?" — the canonical bi-temporal point-in-time query.

    When neither is set, the current live belief is returned (both axes = now).

    Args:
        subject: The entity to query (e.g. "user:alice").
        predicate: The property to read (e.g. "age").
        as_of_tx_time: Optional ISO-8601 UTC timestamp for transaction-time
            point-in-time query.  Filters out claims recorded after this
            instant (controls which writes are visible).
        valid_at: Optional ISO-8601 UTC timestamp for valid-time selection.
            After the transaction-time filter is applied, narrows to the claim
            whose valid-time window contains this instant.
            When absent, backward-compatible behaviour is preserved: the
            as_of_tx_time (or now) is also used as the valid-time instant.

    Returns:
        {"belief": {...}} with the BeliefProjection.
        A "status_reason" field is added when the belief status is Contested or
        PendingReview, explaining why the belief is not yet authoritative.

        Each belief slot (``belief["primary"]``, ``belief["alternatives"][i]``)
        also carries per-endpoint valid-time precision metadata:

          - ``valid_from_display`` (str | absent): start of the valid-time window
            at its recorded precision.  Examples: ``"2020"`` (Year),
            ``"2020-03"`` (Month, no fabricated day), ``"2020-03-15"`` (Day).
            Absent when the start is unknown / not set.

          - ``valid_until_display`` (str | absent): same for the end endpoint.
            Absent when open-ended.

          - ``valid_time["start_granularity"]`` (str | absent): raw granularity tag
            — ``"year"``, ``"month"``, ``"day"``, or ``"instant"``.

          - ``valid_time["end_granularity"]`` (str | absent): same for the end.
    """
    lc = ctx.request_context.lifespan_context
    engine = lc["engine"]
    agent_id = lc["agent_id"]

    request: dict[str, Any] = {
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
    }
    if as_of_tx_time is not None:
        request["as_of_tx_time"] = as_of_tx_time
    if valid_at is not None:
        request["valid_at"] = valid_at

    result: dict[str, Any] = engine.query_memory(request)

    # Surface status_reason for non-authoritative belief statuses
    belief = result.get("belief", {})
    status = belief.get("status")
    if status and status in _NON_COMMITTED:
        result = dict(result)
        result["status_reason"] = _STATUS_REASONS[status]

    return result


# ── Tool 3: reconcile ─────────────────────────────────────────────────────────

@mcp.tool()
async def reconcile(
    subject_lines: list[list[str]],
    ctx: Context = None,
) -> dict[str, Any]:
    """Trigger conflict reconciliation for a set of (subject, predicate) pairs.

    Args:
        subject_lines: List of [subject, predicate] pairs to reconcile.
            Example: [["user:alice", "age"], ["user:bob", "location"]]

    Returns:
        {"outcomes": [[claim_ref, disposition], ...], "oracle_escalations": int}
    """
    lc = ctx.request_context.lifespan_context
    engine = lc["engine"]
    agent_id = lc["agent_id"]

    request = {
        "agent_id": agent_id,
        "subject_lines": [tuple(pair) for pair in subject_lines],
    }
    return engine.reconcile(request)


# ── Tool 4: audit ─────────────────────────────────────────────────────────────

@mcp.tool()
async def audit(
    limit: int = 50,
    claim_ref: Optional[str] = None,
    from_tx_time: Optional[str] = None,
    ctx: Context = None,
) -> dict[str, Any]:
    """Query the audit ledger for claim history.

    Args:
        limit: Maximum number of entries to return (default 50).
        claim_ref: Optional UUID of a specific claim to filter by.
        from_tx_time: Optional ISO-8601 UTC lower bound on transaction time.

    Returns:
        {"entries": [LedgerEntry, ...]}
    """
    lc = ctx.request_context.lifespan_context
    engine = lc["engine"]
    agent_id = lc["agent_id"]

    request: dict[str, Any] = {
        "agent_id": agent_id,
        "limit": limit,
        "claim_ref": claim_ref,
        "from_tx_time": from_tx_time,
    }
    return engine.query_audit(request)
