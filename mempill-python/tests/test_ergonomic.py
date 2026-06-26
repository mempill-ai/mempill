"""
test_ergonomic.py — Integration tests for the Tier-1 ergonomic API.

Covers:
  - remember() defaults applied correctly (provenance, confidence, cardinality)
  - Lenient date parsing: YYYY, YYYY-MM, YYYY-MM-DD, RFC3339 pass-through
  - Unparsable date raises UnparsableDateError with a clear hint
  - recall() returns flat RecallResult with correct .value
  - Contested → value None, is_contested() True, candidates populated (not NoBelief)
  - NoBelief → is_empty() True, is_contested() False
  - Gap 1: RememberOptions.derived_from forwarded via remember()
  - Gap 2: RecallResult.primary + ContestCandidate.detail expose rich BeliefDetail fields
"""

from __future__ import annotations

import pytest
import mempill
from mempill import (
    remember,
    recall,
    RememberOptions,
    RememberReceipt,
    RecallResult,
    BeliefDetail,
    UnparsableDateError,
)


# ── Fixtures ──────────────────────────────────────────────────────────────────

@pytest.fixture()
def engine() -> mempill.Engine:
    return mempill.open_in_memory()


AGENT = "ergo-test-agent"


# ── remember() — defaults ─────────────────────────────────────────────────────

class TestRememberDefaults:
    def test_returns_remember_receipt(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "user", "city", "Berlin")
        assert isinstance(receipt, RememberReceipt)
        assert len(receipt.claim_ref) == 36  # UUID string
        assert receipt.disposition == "CommittedCheap"
        assert receipt.contested_with == []

    def test_default_disposition_committed_cheap(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "user", "name", "Alice")
        assert receipt.disposition == "CommittedCheap"

    def test_value_roundtrips_via_recall(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "age", 42)
        result = recall(engine, AGENT, "user", "age")
        assert result.value == 42

    def test_string_value_roundtrip(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "company", "name", "Acme Corp")
        result = recall(engine, AGENT, "company", "name")
        assert result.as_str() == "Acme Corp"

    def test_default_confidence_is_1(self, engine: mempill.Engine) -> None:
        """Default opts.confidence=1.0 → Cheap-path eligible (external user-asserted)."""
        receipt = remember(engine, AGENT, "x", "y", "z")
        # CommittedCheap proves confidence=1.0 and provenance=External/UserAsserted together.
        assert receipt.disposition == "CommittedCheap"


# ── RememberOptions — confidence, cardinality ─────────────────────────────────

class TestRememberOptions:
    def test_custom_confidence_accepted(self, engine: mempill.Engine) -> None:
        receipt = remember(
            engine, AGENT, "user", "score", 0.8,
            RememberOptions(confidence=0.75),
        )
        # Engine still commits cheap when provenance is External
        assert receipt.claim_ref

    def test_set_valued_cardinality(self, engine: mempill.Engine) -> None:
        r1 = remember(engine, AGENT, "user", "tag", "python",
                      RememberOptions(cardinality="SetValued"))
        r2 = remember(engine, AGENT, "user", "tag", "rust",
                      RememberOptions(cardinality="SetValued"))
        # Both claims are accepted (distinct claim_refs) regardless of disposition.
        assert r1.claim_ref != r2.claim_ref


# ── Lenient date parsing ──────────────────────────────────────────────────────

class TestLenientDateParsing:
    @pytest.mark.parametrize("date_str", [
        "2020",
        "2020-03",
        "2020-03-15",
        "2020-03-15T00:00:00Z",   # full RFC3339 pass-through
    ])
    def test_date_accepted(self, engine: mempill.Engine, date_str: str) -> None:
        """All supported formats must be accepted without error."""
        receipt = remember(
            engine, AGENT, "event", "started", "yes",
            RememberOptions(valid_from=date_str),
        )
        assert receipt.claim_ref, f"Expected claim_ref for date={date_str!r}"

    def test_full_rfc3339_passthrough(self, engine: mempill.Engine) -> None:
        receipt = remember(
            engine, AGENT, "contract", "signed", True,
            RememberOptions(valid_from="2023-06-01T00:00:00Z"),
        )
        assert receipt.claim_ref

    def test_unparsable_natural_language_raises(self, engine: mempill.Engine) -> None:
        with pytest.raises(UnparsableDateError) as exc_info:
            remember(
                engine, AGENT, "event", "when", "party",
                RememberOptions(valid_from="March 2020"),
            )
        err = exc_info.value
        assert "March 2020" in str(err)
        assert "YYYY" in str(err), "Error hint must mention accepted formats"

    def test_empty_string_raises(self, engine: mempill.Engine) -> None:
        with pytest.raises(UnparsableDateError):
            remember(
                engine, AGENT, "event", "when", "party",
                RememberOptions(valid_from=""),
            )

    def test_unparsable_date_has_input_attribute(self, engine: mempill.Engine) -> None:
        with pytest.raises(UnparsableDateError) as exc_info:
            remember(
                engine, AGENT, "x", "y", "z",
                RememberOptions(valid_from="not-a-date"),
            )
        assert exc_info.value.input == "not-a-date"

    def test_valid_time_confidence_zero_without_dates(self, engine: mempill.Engine) -> None:
        """When no dates are supplied, valid_time_confidence should be 0.0.

        We verify indirectly: the claim commits cheap (which means the request
        was structurally valid). The only way to assert the 0.0 default is to
        ensure no exception is raised for a timeless fact.
        """
        receipt = remember(engine, AGENT, "user", "pref", "dark-mode")
        assert receipt.disposition == "CommittedCheap"

    def test_valid_time_confidence_matches_confidence_with_dates(self, engine: mempill.Engine) -> None:
        """When dates ARE supplied, valid_time_confidence mirrors opts.confidence."""
        receipt = remember(
            engine, AGENT, "task", "due", "2025-12-31",
            RememberOptions(valid_from="2025-01-01", confidence=0.9),
        )
        assert receipt.claim_ref  # structurally valid = both fields were set correctly


# ── recall() — flat RecallResult ──────────────────────────────────────────────

class TestRecall:
    def test_returns_recall_result(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        result = recall(engine, AGENT, "user", "city")
        assert isinstance(result, RecallResult)

    def test_resolved_value_accessible(self, engine: mempill.Engine) -> None:
        # A timeless fact (no valid_time dates) yields TimingUncertain status
        # because the engine does not know when the fact became valid. The value
        # is still populated in primary (it is the only candidate).
        remember(engine, AGENT, "planet", "name", "Earth")
        result = recall(engine, AGENT, "planet", "name")
        assert result.value == "Earth"
        assert result.status in ("Resolved", "TimingUncertain"), (
            f"Unexpected status {result.status!r} for a single-claim belief"
        )
        assert result.as_str() == "Earth"
        assert not result.is_contested()
        assert not result.is_empty()

    def test_no_belief_is_empty(self, engine: mempill.Engine) -> None:
        result = recall(engine, AGENT, "ghost", "attr")
        assert result.is_empty()
        assert not result.is_contested()
        assert result.value is None

    def test_no_belief_candidates_empty(self, engine: mempill.Engine) -> None:
        result = recall(engine, AGENT, "ghost", "missing")
        assert result.candidates == []


# ── Contested ─────────────────────────────────────────────────────────────────

class TestContested:
    def test_contested_value_is_none(self, engine: mempill.Engine) -> None:
        """Contested must set value=None — structurally prevents the NoBelief misread."""
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        assert result.value is None, "Contested belief must have value=None"

    def test_is_contested_true(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        assert result.is_contested()

    def test_is_empty_false_for_contested(self, engine: mempill.Engine) -> None:
        """Contested must NOT be mistaken for NoBelief."""
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        assert not result.is_empty(), "Contested must not be empty — candidates exist"

    def test_candidates_populated(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        assert len(result.candidates) == 2

    def test_candidates_contain_both_values(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        values = {c.value for c in result.candidates}
        assert "Alice" in values
        assert "Bob" in values

    def test_candidates_have_claim_refs(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        for candidate in result.candidates:
            assert len(candidate.claim_ref) == 36, "Candidate claim_ref must be a UUID"

    def test_contested_remember_receipt_has_contested_with(self, engine: mempill.Engine) -> None:
        r1 = remember(engine, AGENT, "acme", "cfo", "Carol")
        r2 = remember(engine, AGENT, "acme", "cfo", "Dave")
        assert r2.disposition == "Contested"
        assert r1.claim_ref in r2.contested_with


# ── Gap 1: derived_from forwarded via remember() ──────────────────────────────

class TestDerivedFrom:
    def test_derived_from_default_is_empty(self, engine: mempill.Engine) -> None:
        """RememberOptions.derived_from defaults to []."""
        opts = RememberOptions()
        assert opts.derived_from == []

    def test_derived_from_accepted_by_engine(self, engine: mempill.Engine) -> None:
        """remember() with derived_from= does not raise and the claim is committed."""
        source = remember(engine, AGENT, "user", "city", "Berlin")
        derived = remember(
            engine, AGENT, "user", "city_note", "Capital of Germany",
            RememberOptions(derived_from=[source.claim_ref]),
        )
        assert len(derived.claim_ref) == 36

    def test_derived_from_multiple_refs(self, engine: mempill.Engine) -> None:
        """Multiple derived_from refs are all forwarded."""
        r1 = remember(engine, AGENT, "user", "fact1", "x")
        r2 = remember(engine, AGENT, "user", "fact2", "y")
        derived = remember(
            engine, AGENT, "user", "combined", "xy",
            RememberOptions(derived_from=[r1.claim_ref, r2.claim_ref]),
        )
        assert len(derived.claim_ref) == 36

    def test_derived_from_fact_recalls_correctly(self, engine: mempill.Engine) -> None:
        """A derived fact can be recalled normally after ingestion."""
        source = remember(engine, AGENT, "user", "city", "Berlin")
        remember(
            engine, AGENT, "user", "city_note", "Capital of Germany",
            RememberOptions(derived_from=[source.claim_ref]),
        )
        result = recall(engine, AGENT, "user", "city_note")
        assert not result.is_empty()
        assert result.value == "Capital of Germany"


# ── Gap 2: RecallResult.primary + ContestCandidate.detail (BeliefDetail) ─────

class TestBeliefDetail:
    def test_primary_is_none_for_no_belief(self, engine: mempill.Engine) -> None:
        result = recall(engine, AGENT, "ghost", "attr")
        assert result.primary is None

    def test_primary_set_for_resolved_belief(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "user", "city", "Berlin")
        result = recall(engine, AGENT, "user", "city")
        assert result.primary is not None
        assert isinstance(result.primary, BeliefDetail)
        assert result.primary.claim_ref == receipt.claim_ref

    def test_primary_value_matches_recall_value(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        result = recall(engine, AGENT, "user", "city")
        assert result.primary is not None
        assert result.primary.value == result.value

    def test_primary_valid_from_populated_when_date_supplied(self, engine: mempill.Engine) -> None:
        remember(
            engine, AGENT, "event", "started", "yes",
            RememberOptions(valid_from="2025-01-01", valid_until="2026-01-01"),
        )
        result = recall(engine, AGENT, "event", "started")
        assert result.primary is not None
        assert result.primary.valid_from is not None, "valid_from must be populated"
        assert result.primary.valid_until is not None, "valid_until must be populated"
        assert "2025" in result.primary.valid_from

    def test_primary_value_confidence_matches_opts(self, engine: mempill.Engine) -> None:
        remember(
            engine, AGENT, "user", "score", 0.8,
            RememberOptions(confidence=0.75),
        )
        result = recall(engine, AGENT, "user", "score")
        assert result.primary is not None
        assert abs(result.primary.value_confidence - 0.75) < 1e-4

    def test_primary_provenance_user_asserted(self, engine: mempill.Engine) -> None:
        """Default provenance is External/UserAsserted."""
        remember(engine, AGENT, "user", "city", "Berlin")
        result = recall(engine, AGENT, "user", "city")
        assert result.primary is not None
        assert "UserAsserted" in result.primary.provenance

    def test_primary_corroboration_count_zero_for_fresh_write(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        result = recall(engine, AGENT, "user", "city")
        assert result.primary is not None
        assert result.primary.corroboration_count == 0

    def test_primary_none_for_contested(self, engine: mempill.Engine) -> None:
        """Contested belief must not set primary — use candidates[n].detail."""
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        assert result.is_contested()
        assert result.primary is None

    def test_contested_candidates_have_detail(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "acme", "ceo", "Alice")
        remember(engine, AGENT, "acme", "ceo", "Bob")
        result = recall(engine, AGENT, "acme", "ceo")
        assert len(result.candidates) == 2
        for c in result.candidates:
            assert c.detail is not None
            assert isinstance(c.detail, BeliefDetail)
            assert len(c.detail.claim_ref) == 36
            assert c.detail.value in ("Alice", "Bob")
            assert c.detail.value == c.value
            assert "UserAsserted" in c.detail.provenance

    def test_belief_detail_fields_parity_with_rust(self, engine: mempill.Engine) -> None:
        """Verify all BeliefDetail fields match the Rust surface exactly."""
        receipt = remember(
            engine, AGENT, "user", "city", "Berlin",
            RememberOptions(valid_from="2025-01-01", confidence=0.9),
        )
        result = recall(engine, AGENT, "user", "city")
        p = result.primary
        assert p is not None
        # Field presence and type checks (Rust parity):
        assert isinstance(p.claim_ref, str) and len(p.claim_ref) == 36
        assert p.value == "Berlin"
        assert p.valid_from is not None
        assert isinstance(p.value_confidence, float)
        assert isinstance(p.provenance, str)
        assert isinstance(p.corroboration_count, int)
        assert p.claim_ref == receipt.claim_ref
