"""
mempill.ergonomic — Tier-1 ergonomic API: remember() / recall().

Wraps engine.ingest_claim() / engine.query_memory() with sane defaults so the
caller never touches ProvenanceLabel, ConfidenceDict, or deep belief paths.

Simplest usage:
    from mempill import open_in_memory, remember, recall
    engine = open_in_memory()
    remember(engine, "agent", "user", "city", "Berlin")
    result = recall(engine, "agent", "user", "city")
    print(result.as_str())        # "Berlin"
    print(result.is_contested())  # False
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any, Optional


# ── Lenient date normalization ────────────────────────────────────────────────

_ISO_DATE_RE = re.compile(r"^(\d{4})(?:-(\d{2})(?:-(\d{2}))?)?$")


class UnparsableDateError(ValueError):
    """Raised when a date string cannot be normalized to RFC3339.

    Use YYYY, YYYY-MM, YYYY-MM-DD, or a full RFC3339 string.
    Natural-language dates (e.g. "March 2020") must be resolved by the caller
    before passing to remember().
    """

    def __init__(self, input: str) -> None:
        self.input = input
        super().__init__(
            f"Unparsable date {input!r}: use YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339. "
            "Natural-language dates must be parsed by the host before calling remember()."
        )


def _to_rfc3339(value: str) -> str:
    """Normalize a lenient date string to RFC3339 (midnight UTC).

    Accepts:
        "2020"               → "2020-01-01T00:00:00Z"
        "2020-03"            → "2020-03-01T00:00:00Z"
        "2020-03-15"         → "2020-03-15T00:00:00Z"
        "2020-03-15T12:00Z"  → pass-through (contains "T")

    Raises:
        UnparsableDateError: for empty strings, natural-language dates, or any
                             string not matching the above patterns.
    """
    s = value.strip()
    if not s:
        raise UnparsableDateError(value)
    if "T" in s:
        # Full datetime — pass through as-is.
        return s
    m = _ISO_DATE_RE.match(s)
    if m:
        year = m.group(1)
        month = m.group(2) or "01"
        day = m.group(3) or "01"
        return f"{year}-{month}-{day}T00:00:00Z"
    raise UnparsableDateError(value)


# ── DTOs ─────────────────────────────────────────────────────────────────────

@dataclass
class RememberOptions:
    """Optional overrides for remember(). All fields have safe defaults.

    Args:
        valid_from:  Lenient date string — YYYY / YYYY-MM / YYYY-MM-DD / RFC3339.
                     None means open / unknown start (valid_time_confidence = 0.0).
        valid_until: Lenient date string. None means open-ended.
        confidence:  Value confidence [0.0, 1.0]. Also used as valid_time_confidence
                     when dates are provided (eliminates the duplicate-field quirk).
                     Defaults to 1.0 (user-stated facts are user-stated truth).
        cardinality: "Functional" | "SetValued" | "Unknown". Defaults to "Functional".
        provenance:  Wire-shape provenance dict. Defaults to External/UserAsserted.
                     Power users may pass ProvenanceLabel.model_derived() etc.
        criticality: "Low" | "Medium" | "High" | "Critical". Defaults to "Medium".
    """

    valid_from:  Optional[str] = None
    valid_until: Optional[str] = None
    confidence:  float         = 1.0
    cardinality: str           = "Functional"
    provenance:  Optional[dict] = None   # None → External/UserAsserted at call time
    criticality: str           = "Medium"


@dataclass
class RememberReceipt:
    """Return value from remember().

    Attributes:
        claim_ref:      UUID string identifying the stored claim.
        disposition:    Engine outcome (e.g. "CommittedCheap", "Contested").
        contested_with: Non-empty only when disposition is Contested/Conflict.
    """

    claim_ref:      str
    disposition:    str
    contested_with: list[str] = field(default_factory=list)


@dataclass
class ContestCandidate:
    """One candidate in a Contested/Conflict belief.

    Attributes:
        value:     The candidate value.
        claim_ref: UUID string of the backing claim.
        valid_from: RFC3339 start of the claim's valid time, or None if open.
    """

    value:      Any
    claim_ref:  str
    valid_from: Optional[str]


@dataclass
class RecallResult:
    """Flat return type from recall(). No 4-level belief-path traversal required.

    Attributes:
        value:      The resolved value. ALWAYS None for Contested, NoBelief, or
                    TimingUncertain — check is_contested() / is_empty() before using.
        status:     "Resolved" | "Contested" | "Conflict" | "TimingUncertain" | "NoBelief"
        candidates: Populated when status is Contested or Conflict. Use this (not value)
                    to inspect conflicting options.
        currency:   Currency signal string ("Fresh", "Stale", etc.).
        is_stale:   True when the engine signals the belief may be outdated.
    """

    value:      Optional[Any]
    status:     str
    candidates: list[ContestCandidate] = field(default_factory=list)
    currency:   str                    = "Fresh"
    is_stale:   bool                   = False

    def as_str(self) -> Optional[str]:
        """Return value as a string, or None.

        Convenience for the 95% case where the value is a simple scalar.
        Do not use as a substitute for is_contested() / is_empty() checks.
        """
        return str(self.value) if self.value is not None else None

    def is_contested(self) -> bool:
        """True when two or more claims conflict and no winner has been chosen.

        Do NOT use ``value is None`` as the contest check — NoBelief also has
        value=None. This method is the only reliable signal.
        """
        return self.status in ("Contested", "Conflict")

    def is_empty(self) -> bool:
        """True when the engine has no live claim for this subject+predicate."""
        return self.status == "NoBelief"


# ── Tier-1 functions ──────────────────────────────────────────────────────────

def remember(
    engine: Any,
    agent_id: str,
    subject: str,
    predicate: str,
    value: Any,
    opts: Optional[RememberOptions] = None,
) -> RememberReceipt:
    """Remember a fact. Minimum args: engine, agent_id, subject, predicate, value.

    Builds the full ingest dict with sane defaults and calls engine.ingest_claim().
    All Tier-3 boilerplate (provenance shape, confidence duplication, valid_time dict
    quirk, cardinality default) is handled internally.

    Args:
        engine:    A mempill Engine (from open_in_memory() or open()).
        agent_id:  The agent's identity string.
        subject:   Entity key (e.g. "user", "acme").
        predicate: Property key (e.g. "city", "ceo").
        value:     The asserted value (any JSON-serialisable type).
        opts:      Optional overrides. Pass RememberOptions(...) to set dates,
                   confidence, cardinality, provenance, criticality.

    Returns:
        RememberReceipt with claim_ref, disposition, and contested_with.

    Raises:
        UnparsableDateError: if opts.valid_from or opts.valid_until cannot be
            normalized (e.g. "March 2020" or an empty string).
        mempill.ValidationError: propagated from the engine on malformed requests.
        mempill.StorageError: propagated from the engine on storage failures.
    """
    if opts is None:
        opts = RememberOptions()

    # Provenance — default to External/UserAsserted.
    provenance = opts.provenance or {"type": "External", "kind": "UserAsserted"}

    # Date normalization and valid_time_confidence derivation.
    has_dates = opts.valid_from is not None or opts.valid_until is not None
    vtc = opts.confidence if has_dates else 0.0

    valid_time: dict[str, Any] = {"valid_time_confidence": vtc}
    if opts.valid_from is not None:
        valid_time["start"] = _to_rfc3339(opts.valid_from)
    if opts.valid_until is not None:
        valid_time["end"] = _to_rfc3339(opts.valid_until)

    request: dict[str, Any] = {
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "provenance": provenance,
        "cardinality": opts.cardinality,
        "valid_time": valid_time,
        "confidence": {
            "value_confidence": opts.confidence,
            "valid_time_confidence": vtc,
        },
        "criticality": opts.criticality,
        "derived_from": [],
    }

    resp = engine.ingest_claim(request)
    return RememberReceipt(
        claim_ref=resp["claim_ref"],
        disposition=resp["disposition"],
        contested_with=resp.get("contested_with") or [],
    )


def recall(
    engine: Any,
    agent_id: str,
    subject: str,
    predicate: str,
) -> RecallResult:
    """Recall the current belief for a subject+predicate pair.

    Flattens the 4-level belief path into a RecallResult with direct accessors.
    Correctly surfaces Contested beliefs (value=None, candidates populated) so
    that Contested is never silently misread as NoBelief.

    Args:
        engine:    A mempill Engine.
        agent_id:  The agent's identity string.
        subject:   Entity key.
        predicate: Property key.

    Returns:
        RecallResult — check is_contested() / is_empty() before accessing .value.

    Raises:
        mempill.ValidationError: propagated from the engine.
        mempill.NotFoundError: propagated from the engine.
        mempill.StorageError: propagated from the engine.
    """
    resp = engine.query_memory({
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
    })

    belief = resp.get("belief", {})
    status: str = belief.get("status", "NoBelief")

    # Contested: primary is None, both candidates live in alternatives.
    # TimingUncertain / NoBelief: primary is also None.
    # We treat all no-primary cases as value=None — but status distinguishes them.
    primary = belief.get("primary")

    if primary is not None:
        resolved_value = (primary.get("fact") or {}).get("value")
        currency_signal = primary.get("currency_signal") or {}
        # The engine uses "state" as the key inside currency_signal.
        currency_str: str = currency_signal.get("state", currency_signal.get("currency", "Fresh"))
        staleness_block = belief.get("staleness") or {}
        is_stale: bool = staleness_block.get("is_stale", False)
    else:
        resolved_value = None
        # Fall back to the top-level belief.currency field when no primary.
        currency_str = belief.get("currency", "Fresh")
        staleness_block = belief.get("staleness") or {}
        is_stale = staleness_block.get("is_stale", False)

    # Map alternatives → ContestCandidate list (populated for Contested/Conflict).
    candidates: list[ContestCandidate] = []
    for alt in (belief.get("alternatives") or []):
        if alt is None:
            continue
        alt_value = (alt.get("fact") or {}).get("value")
        alt_ref   = alt.get("claim_ref", "")
        alt_vt    = (alt.get("valid_time") or {}).get("start")
        candidates.append(ContestCandidate(
            value=alt_value,
            claim_ref=alt_ref,
            valid_from=alt_vt,
        ))

    return RecallResult(
        value=resolved_value,
        status=status,
        candidates=candidates,
        currency=currency_str,
        is_stale=is_stale,
    )


__all__ = [
    "UnparsableDateError",
    "RememberOptions",
    "RememberReceipt",
    "ContestCandidate",
    "RecallResult",
    "remember",
    "recall",
]
