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
        valid_from:   Lenient date string — YYYY / YYYY-MM / YYYY-MM-DD / RFC3339.
                      None means open / unknown start (valid_time_confidence = 0.0).
        valid_until:  Lenient date string. None means open-ended.
        confidence:   Value confidence [0.0, 1.0]. Also used as valid_time_confidence
                      when dates are provided (eliminates the duplicate-field quirk).
                      Defaults to 1.0 (user-stated facts are user-stated truth).
        cardinality:  "Functional" | "SetValued" | "Unknown". Defaults to "Functional".
        provenance:   Wire-shape provenance dict. Defaults to External/UserAsserted.
                      Power users may pass ProvenanceLabel.model_derived() etc.
        criticality:  "Low" | "Medium" | "High" | "Critical". Defaults to "Medium".
        derived_from: List of upstream claim_ref UUID strings that this fact was
                      derived from. Forwarded verbatim into the ingest request's
                      ``derived_from`` field. Defaults to empty list.
                      Used to express lineage for RecallReEntry / model-derived chains.
    """

    valid_from:   Optional[str]   = None
    valid_until:  Optional[str]   = None
    confidence:   float           = 1.0
    cardinality:  str             = "Functional"
    provenance:   Optional[dict]  = None   # None → External/UserAsserted at call time
    criticality:  str             = "Medium"
    derived_from: list[str]       = field(default_factory=list)


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
class BeliefDetail:
    """Rich detail for a single belief (primary or candidate).

    Available via ``RecallResult.primary`` (resolved belief) and
    ``ContestCandidate.detail`` (each contested candidate).

    Attributes:
        claim_ref:           UUID string of the backing claim.
        value:               The asserted value.
        valid_from:          RFC3339 start of the valid-time window, or None if open/unknown.
        valid_until:         RFC3339 end of the valid-time window, or None if open-ended.
        value_confidence:    Value confidence (0.0–1.0).
        provenance:          Human-readable label, e.g. "External/UserAsserted",
                             "RecallReEntry", "ModelDerived".
        corroboration_count: Number of independent corroborating sources recorded by
                             the engine (confidence annotation only; not a gate).
    """

    claim_ref:           str
    value:               Any
    valid_from:          Optional[str]
    valid_until:         Optional[str]
    value_confidence:    float
    provenance:          str
    corroboration_count: int


@dataclass
class ContestCandidate:
    """One candidate in a Contested/Conflict belief.

    Attributes:
        value:     The candidate value.
        claim_ref: UUID string of the backing claim.
        valid_from: RFC3339 start of the claim's valid time, or None if open.
        detail:    Full ``BeliefDetail`` for this candidate — same fields as
                   the primary belief detail, enabling view construction without
                   navigating the deep belief path.
    """

    value:      Any
    claim_ref:  str
    valid_from: Optional[str]
    detail:     BeliefDetail = field(default_factory=lambda: BeliefDetail(
        claim_ref="", value=None, valid_from=None, valid_until=None,
        value_confidence=0.0, provenance="", corroboration_count=0,
    ))


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
        primary:    Full ``BeliefDetail`` for the resolved primary belief.
                    None when status is NoBelief. For Contested/Conflict, read
                    ``candidates[n].detail`` instead — primary is not set.
    """

    value:      Optional[Any]
    status:     str
    candidates: list[ContestCandidate]  = field(default_factory=list)
    currency:   str                     = "Fresh"
    is_stale:   bool                    = False
    primary:    Optional[BeliefDetail]  = None

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


# ── Internal helpers ─────────────────────────────────────────────────────────

def _provenance_label_str(prov: Any) -> str:
    """Convert a raw provenance dict or string to a human-readable label.

    Matches the Rust ergonomic surface:
      External/UserAsserted, External/ExternalFirstHand, RecallReEntry, ModelDerived.
    """
    if isinstance(prov, dict):
        t = prov.get("type", "")
        k = prov.get("kind", "")
        if t == "External":
            if k == "UserAsserted":
                return "External/UserAsserted"
            if k == "ExternalFirstHand":
                return "External/ExternalFirstHand"
            return f"External/{k}" if k else "External"
        if t == "RecallReEntry":
            return "RecallReEntry"
        if t == "ModelDerived":
            return "ModelDerived"
        return t or str(prov)
    if isinstance(prov, str):
        return prov
    return str(prov) if prov is not None else ""


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
        "derived_from": list(opts.derived_from),
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
    primary_raw = belief.get("primary")

    if primary_raw is not None:
        resolved_value = (primary_raw.get("fact") or {}).get("value")
        currency_signal = primary_raw.get("currency_signal") or {}
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

    # ── Helper: build a BeliefDetail from a raw belief dict ──────────────────
    def _make_detail(b: dict) -> BeliefDetail:
        fact = b.get("fact") or {}
        vt = b.get("valid_time") or {}
        conf = b.get("confidence") or {}
        csig = b.get("currency_signal") or {}
        prov_raw = b.get("provenance")
        return BeliefDetail(
            claim_ref=b.get("claim_ref", ""),
            value=fact.get("value"),
            valid_from=vt.get("start"),
            valid_until=vt.get("end"),
            value_confidence=float(conf.get("value_confidence", 0.0)),
            provenance=_provenance_label_str(prov_raw),
            corroboration_count=int(csig.get("corroboration_count", 0)),
        )

    # Build primary BeliefDetail when we have a concrete primary and status is not Contested.
    if primary_raw is not None and status not in ("Contested", "Conflict"):
        primary_detail: Optional[BeliefDetail] = _make_detail(primary_raw)
    else:
        primary_detail = None

    # Map alternatives → ContestCandidate list (populated for Contested/Conflict).
    # For Contested: the engine places both competing beliefs in alternatives[].
    # For Contested: we also pull the primary belief (if any) into candidates
    # so the full set is visible — matching the Rust ergonomic behaviour.
    candidates: list[ContestCandidate] = []
    raw_contest_beliefs = []
    if status in ("Contested", "Conflict"):
        if primary_raw is not None:
            raw_contest_beliefs.append(primary_raw)
        raw_contest_beliefs.extend(b for b in (belief.get("alternatives") or []) if b is not None)
    else:
        raw_contest_beliefs = []

    for b in raw_contest_beliefs:
        b_value = (b.get("fact") or {}).get("value")
        b_ref   = b.get("claim_ref", "")
        b_vt    = (b.get("valid_time") or {}).get("start")
        candidates.append(ContestCandidate(
            value=b_value,
            claim_ref=b_ref,
            valid_from=b_vt,
            detail=_make_detail(b),
        ))

    # For non-contested beliefs, alternatives are not contest candidates but we
    # still need to handle the original non-contested alternative path.
    if not candidates and status not in ("Contested", "Conflict"):
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
                detail=_make_detail(alt),
            ))

    return RecallResult(
        value=resolved_value,
        status=status,
        candidates=candidates,
        currency=currency_str,
        is_stale=is_stale,
        primary=primary_detail,
    )


# ── History DTOs ──────────────────────────────────────────────────────────────

@dataclass
class HistoryEntry:
    """One slot in the history timeline for a subject-line.

    Attributes:
        claim_ref:        UUID string identifying the underlying claim.
        value:            The asserted value for this claim.
        valid_from:       RFC3339 start of the valid-time window, or None if unknown.
        valid_until:      Effective end of the slot (successor's ordering key), or
                          None for the open-ended current slot.
        status:           "Current" or "Superseded".
        provenance:       Human-readable label, e.g. "External/UserAsserted".
        value_confidence: Confidence in the claim's value (0.0–1.0).
    """

    claim_ref:        str
    value:            Any
    valid_from:       Optional[str]
    valid_until:      Optional[str]
    status:           str
    provenance:       str
    value_confidence: float


class History:
    """Full ordered history timeline for a (subject, predicate) subject-line.

    Returned by history(). Iterable over entries (oldest→newest).

    Attributes:
        entries: All HistoryEntry objects ordered oldest→newest.
    """

    def __init__(self, entries: list[HistoryEntry]) -> None:
        self.entries: list[HistoryEntry] = entries

    def current(self) -> Optional[HistoryEntry]:
        """Return the single Current entry, or None if none exists.

        Guaranteed to agree with recall() — the same canonical fold is used.
        """
        for e in self.entries:
            if e.status == "Current":
                return e
        return None

    def is_empty(self) -> bool:
        """True when the subject-line has no claims at all."""
        return len(self.entries) == 0

    def __iter__(self):
        """Iterate over entries oldest→newest."""
        return iter(self.entries)

    def __len__(self) -> int:
        return len(self.entries)

    def __repr__(self) -> str:
        return f"History(entries={self.entries!r})"


# ── Tier-1 history function ────────────────────────────────────────────────────

def history(
    engine: Any,
    agent_id: str,
    subject: str,
    predicate: str,
) -> History:
    """Return the full ordered history timeline for a (subject, predicate) pair.

    Entries are ordered oldest→newest by the canonical ordering key (same as the
    truth engine fold). Each entry carries `.status` ("Current" or "Superseded"),
    `.value`, `.valid_from`, `.valid_until`, `.provenance`, `.value_confidence`,
    and `.claim_ref`.

    The `.current()` entry is guaranteed to agree with recall() — both use the
    same canonical fold at the engine level.

    Args:
        engine:    A mempill Engine or OracleEngine.
        agent_id:  The agent's identity string.
        subject:   Entity key (e.g. "acme").
        predicate: Property key (e.g. "ceo").

    Returns:
        History — iterable over HistoryEntry objects. Use .current() for the live
        belief, .is_empty() to check for no claims.

    Raises:
        mempill.ValidationError: propagated from the engine on malformed requests.
        mempill.StorageError: propagated from the engine on storage failures.
    """
    resp = engine.query_history({
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
    })

    raw_entries = resp.get("entries") or []
    entries: list[HistoryEntry] = []
    for e in raw_entries:
        # valid_from / valid_until arrive as RFC3339 strings or None.
        vf = e.get("valid_from")
        vu = e.get("valid_until")
        # status is serde-serialized as the variant name: "Current" or "Superseded".
        status_raw = e.get("status")
        if isinstance(status_raw, dict):
            # If somehow serde emits adjacently-tagged, extract tag.
            status = status_raw.get("type", str(status_raw))
        else:
            status = str(status_raw) if status_raw is not None else "Superseded"

        entries.append(HistoryEntry(
            claim_ref=e.get("claim_ref", ""),
            value=e.get("value"),
            valid_from=vf,
            valid_until=vu,
            status=status,
            provenance=e.get("provenance", ""),
            value_confidence=float(e.get("value_confidence", 0.0)),
        ))

    return History(entries)


__all__ = [
    "UnparsableDateError",
    "RememberOptions",
    "RememberReceipt",
    "BeliefDetail",
    "ContestCandidate",
    "RecallResult",
    "remember",
    "recall",
    "HistoryEntry",
    "History",
    "history",
]
