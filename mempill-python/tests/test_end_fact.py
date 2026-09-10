"""
test_end_fact.py — Tests for assert_validity (raw) and end_fact (ergonomic), TASK-33 E2.

Covers:
  - engine.assert_validity() raw dict passthrough: bound, idempotent no-op, AlreadyBound,
    IncoherentTemporalWindow, InsufficientProvenanceForOverturn (ModelDerived rejected).
  - engine.resolve_live_claim_for_line() raw resolution: empty / single / ambiguous.
  - end_fact() ergonomic sugar: resolves and bounds, never guesses on an ambiguous line,
    raises NotFoundError on an empty line, and the DIAG-3 regression sequence
    (open incumbent -> end_fact -> non-overlapping challenger -> clean CommittedCheap
    succession, not Contested).
"""

from __future__ import annotations

import pytest

import mempill
from mempill import remember, recall, history, end_fact, EndFactReceipt, RememberOptions
from mempill import NotFoundError, ValidationError, ConflictError


@pytest.fixture()
def engine() -> mempill.Engine:
    return mempill.open_in_memory()


AGENT = "end-fact-test-agent"


# ── engine.assert_validity() raw passthrough ───────────────────────────────────

class TestAssertValidityRaw:
    def test_bound_returns_superseded(self, engine: mempill.Engine) -> None:
        receipt = remember(
            engine, AGENT, "raw-subj", "raw-pred", "v1",
            RememberOptions(valid_from="2020-01-01"),
        )
        resp = engine.assert_validity({
            "agent_id": AGENT,
            "target": receipt.claim_ref,
            "assertion": {"type": "Bound", "value": {"at": "2021-01-01T00:00:00Z"}},
            "provenance": {"type": "External", "kind": "UserAsserted"},
            "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
        })
        assert resp["disposition"] == "Superseded"
        assert resp["no_op"] is False
        assert resp["claim_ref"] == receipt.claim_ref

    def test_bound_idempotent_same_at_is_noop(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "raw-idem-subj", "raw-idem-pred", "v1")
        req = {
            "agent_id": AGENT,
            "target": receipt.claim_ref,
            "assertion": {"type": "Bound", "value": {"at": "2021-01-01T00:00:00Z"}},
            "provenance": {"type": "External", "kind": "UserAsserted"},
            "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
        }
        first = engine.assert_validity(req)
        second = engine.assert_validity(req)
        assert first["no_op"] is False
        assert second["no_op"] is True
        assert second["assertion_ref"] == first["assertion_ref"]

    def test_bound_different_at_raises_conflict_error(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "raw-ab-subj", "raw-ab-pred", "v1")
        base = {
            "agent_id": AGENT,
            "target": receipt.claim_ref,
            "provenance": {"type": "External", "kind": "UserAsserted"},
            "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
        }
        engine.assert_validity({**base, "assertion": {"type": "Bound", "value": {"at": "2021-01-01T00:00:00Z"}}})
        with pytest.raises(ConflictError):
            engine.assert_validity({**base, "assertion": {"type": "Bound", "value": {"at": "2022-01-01T00:00:00Z"}}})

    def test_bound_before_start_raises_validation_error(self, engine: mempill.Engine) -> None:
        receipt = remember(
            engine, AGENT, "raw-ic-subj", "raw-ic-pred", "v1",
            RememberOptions(valid_from="2020-06-01"),
        )
        with pytest.raises(ValidationError):
            engine.assert_validity({
                "agent_id": AGENT,
                "target": receipt.claim_ref,
                "assertion": {"type": "Bound", "value": {"at": "2020-01-01T00:00:00Z"}},
                "provenance": {"type": "External", "kind": "UserAsserted"},
                "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
            })

    def test_bound_model_derived_provenance_raises_validation_error(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "raw-pv-subj", "raw-pv-pred", "v1")
        with pytest.raises(ValidationError):
            engine.assert_validity({
                "agent_id": AGENT,
                "target": receipt.claim_ref,
                "assertion": {"type": "Bound", "value": {"at": "2021-01-01T00:00:00Z"}},
                "provenance": {"type": "ModelDerived"},
                "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
            })

    def test_reopen_restores_liveness(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "raw-ro-subj", "raw-ro-pred", "v1")
        base = {
            "agent_id": AGENT,
            "target": receipt.claim_ref,
            "provenance": {"type": "External", "kind": "UserAsserted"},
            "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
        }
        engine.assert_validity({**base, "assertion": {"type": "Bound", "value": {"at": "2021-01-01T00:00:00Z"}}})
        resp = engine.assert_validity({**base, "assertion": {"type": "Reopen"}})
        assert resp["disposition"] == "Reinstated"
        assert resp["no_op"] is False

        result = recall(engine, AGENT, "raw-ro-subj", "raw-ro-pred")
        assert not result.is_empty()

    def test_cross_agent_target_raises_not_found(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, "agent-a", "raw-xa-subj", "raw-xa-pred", "v1")
        with pytest.raises(NotFoundError):
            engine.assert_validity({
                "agent_id": "agent-b",
                "target": receipt.claim_ref,
                "assertion": {"type": "Bound", "value": {"at": "2021-01-01T00:00:00Z"}},
                "provenance": {"type": "External", "kind": "UserAsserted"},
                "confidence": {"value_confidence": 1.0, "valid_time_confidence": 1.0},
            })


# ── engine.resolve_live_claim_for_line() raw resolution ────────────────────────

class TestResolveLiveClaimForLine:
    def test_empty_line(self, engine: mempill.Engine) -> None:
        resolution = engine.resolve_live_claim_for_line(AGENT, "resolve-empty-subj", "resolve-empty-pred")
        assert resolution["status"] == "empty"
        assert resolution["claim_ref"] is None

    def test_single_live_claim(self, engine: mempill.Engine) -> None:
        receipt = remember(engine, AGENT, "resolve-single-subj", "resolve-single-pred", "v1")
        resolution = engine.resolve_live_claim_for_line(AGENT, "resolve-single-subj", "resolve-single-pred")
        assert resolution["status"] == "single"
        assert resolution["claim_ref"] == receipt.claim_ref

    def test_ambiguous_multiple_live_claims(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "resolve-ambig-subj", "resolve-ambig-pred", "a")
        remember(engine, AGENT, "resolve-ambig-subj", "resolve-ambig-pred", "b")
        resolution = engine.resolve_live_claim_for_line(AGENT, "resolve-ambig-subj", "resolve-ambig-pred")
        assert resolution["status"] == "ambiguous"
        assert resolution["live_count"] == 2


# ── end_fact() ergonomic sugar ──────────────────────────────────────────────────

class TestEndFact:
    def test_single_live_claim_bounds_and_returns_receipt(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "ef-single-subj", "ef-single-pred", "Austin",
                  RememberOptions(valid_from="2021-04-01"))
        receipt = end_fact(engine, AGENT, "ef-single-subj", "ef-single-pred", "2024-09-23")
        assert isinstance(receipt, EndFactReceipt)
        assert receipt.disposition == "Superseded"
        assert receipt.no_op is False
        assert receipt.effective_at.startswith("2024-09-23")

    def test_zero_live_claims_raises_not_found(self, engine: mempill.Engine) -> None:
        with pytest.raises(NotFoundError):
            end_fact(engine, AGENT, "ef-empty-subj", "ef-empty-pred", "2024-01-01")

    def test_multiple_live_claims_raises_validation_error_never_guesses(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "ef-ambig-subj", "ef-ambig-pred", "a")
        remember(engine, AGENT, "ef-ambig-subj", "ef-ambig-pred", "b")
        with pytest.raises(ValidationError):
            end_fact(engine, AGENT, "ef-ambig-subj", "ef-ambig-pred", "2024-01-01")

    def test_repeated_end_fact_on_fully_closed_line_raises_not_found(self, engine: mempill.Engine) -> None:
        """end_fact resolves against the LIVE set (I8, same as recall/history). Once the
        sole claim on the line is bounded, the line has zero live claims, so a repeat
        end_fact() call correctly raises NotFoundError — it is not "the same claim,
        idempotent no-op". True idempotent re-bind (I6) is a property of
        assert_validity() called directly with the known claim_ref (see
        TestAssertValidityRaw.test_bound_idempotent_same_at_is_noop), not of
        subject/predicate resolution repeated after the line has closed.
        """
        remember(engine, AGENT, "ef-idem-subj", "ef-idem-pred", "v1",
                  RememberOptions(valid_from="2020-01-01"))
        first = end_fact(engine, AGENT, "ef-idem-subj", "ef-idem-pred", "2021-01-01")
        assert first.no_op is False
        with pytest.raises(NotFoundError):
            end_fact(engine, AGENT, "ef-idem-subj", "ef-idem-pred", "2021-01-01")

    def test_diag3_sequence_end_fact_then_challenger_is_clean_succession(self, engine: mempill.Engine) -> None:
        """DIAG_close_incumbent.md §1: open incumbent -> end_fact -> non-overlapping
        challenger must fold to a clean CommittedCheap succession, not Contested.
        This is the exact regression assert_validity/end_fact fixes.
        """
        remember(engine, AGENT, "diag3-subj", "diag3-pred", "Austin",
                  RememberOptions(valid_from="2021-04-01"))
        end_fact(engine, AGENT, "diag3-subj", "diag3-pred", "2024-09-23")
        remember(engine, AGENT, "diag3-subj", "diag3-pred", "NYC",
                  RememberOptions(valid_from="2024-09-23"))

        result = recall(engine, AGENT, "diag3-subj", "diag3-pred")
        assert not result.is_contested(), f"expected clean succession, got status={result.status}"
        assert result.value == "NYC"


# ── end_fact date precision (TASK-33-W5-LIB-R2, F2) ────────────────────────────

class TestEndFactGranularity:
    """`end_fact(at=...)` must preserve the PRECISION of the caller's date string.

    Before this wiring the ergonomic layer posted only the normalized instant, so
    end_fact(..., "2024-09") stored 2024-09-01T00:00:00Z with no granularity and history
    rendered the fabricated day "2024-09-01". The granularity now travels with the bound
    (`assertion.value.at_granularity`), derived by the engine's single Rust date parser.
    """

    def test_month_precision_round_trips_to_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-ef-subj", "gran-ef-pred", "v1",
                  RememberOptions(valid_from="2020-01-01"))
        end_fact(engine, AGENT, "gran-ef-subj", "gran-ef-pred", "2024-09")

        entry = history(engine, AGENT, "gran-ef-subj", "gran-ef-pred").entries[0]
        assert entry.valid_until_granularity == "month"
        assert entry.valid_until_display == "2024-09"

    def test_year_precision_round_trips_to_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-ef-y-subj", "gran-ef-y-pred", "v1",
                  RememberOptions(valid_from="2020-01-01"))
        end_fact(engine, AGENT, "gran-ef-y-subj", "gran-ef-y-pred", "2024")

        entry = history(engine, AGENT, "gran-ef-y-subj", "gran-ef-y-pred").entries[0]
        assert entry.valid_until_granularity == "year"
        assert entry.valid_until_display == "2024"

    def test_date_granularity_of_matches_the_rust_parser(self) -> None:
        from mempill._mempill import date_granularity_of

        assert date_granularity_of("2024") == "year"
        assert date_granularity_of("2024-09") == "month"
        assert date_granularity_of("2024-09-15") == "day"
        assert date_granularity_of("2024-09-15T10:30:00Z") == "instant"
        assert date_granularity_of("last September") is None


# ── date_granularity_of public API ────────────────────────────────────────────

class TestDateGranularityOfPublicAPI:
    """Verify that date_granularity_of is re-exported at the package level."""

    def test_date_granularity_of_is_exposed_at_package_level(self) -> None:
        assert hasattr(mempill, "date_granularity_of")
        assert mempill.date_granularity_of("2024-09") == "month"

    def test_date_granularity_of_is_in_all(self) -> None:
        assert "date_granularity_of" in mempill.__all__
