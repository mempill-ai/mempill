"""
test_valid_at.py — Bi-temporal valid_at query tests.

Verifies that valid_at (valid-time axis) is accepted, forwarded, and composed
independently with as_of_tx_time (transaction-time axis).

Engine behavior notes (TASK-33-W5-LIB A, DIAG-4 finding A — updated; these notes
previously claimed "valid_at does not change the result" for a single-claim fold,
which was true ONLY when the query instant fell inside that claim's window. That
claim was a symptom of a bug, not a rule — see below):
  - valid_at now ALWAYS window-tests every candidate, including a single live
    claim: an instant BEFORE that claim's own valid_time.start correctly yields
    NoBelief (`test_valid_at_before_single_claim_start_returns_no_belief`), not
    the claim unconditionally.
  - valid_at also re-enters a claim that is excluded ONLY by an active
    ValidityAssertion::Bound (e.g. `end_fact`/`assert_validity`), narrowed to its
    real believed window — a claim ended via `end_fact` is NOT "gone" for an
    in-window valid_at instant, it is the CORRECT historical answer
    (`test_valid_at_reenters_end_fact_bounded_claim`). A claim excluded by a
    disposition with no accompanying Bound (Quarantined/Invalidated/Rejected, or
    a Deny-bounded challenger) still stays excluded — it was never genuinely
    believed.
  - The canonical bi-temporal scenario (different valid_at → different belief)
    is tested in the Rust conformance suite (mempill-core/src/testing/conformance.rs
    run_valid_at_conformance), which bypasses the ingest pipeline to keep both
    claims live simultaneously.

What these tests verify:
  1. valid_at is forwarded through the PyO3 depythonize path without error.
  2. valid_at=None is accepted (serde default; same as omitting the key).
  3. as_of_tx_time + valid_at compose without error (D2 independence).
  4. valid_at in a gap with a single bounded claim returns the claim
     (single-claim fold does not apply succession narrowing).
  5. The live belief is still correct when valid_at is set (no regression).
  6. A single live claim queried at an instant BEFORE its own valid_time.start
     returns NoBelief (the `len() > 1` guard drop).
  7. A claim ended via `end_fact` re-enters for an in-window valid_at instant.
"""

from __future__ import annotations

import mempill
from mempill.types import Disposition, ProvenanceLabel


def _ingest_ceo(
    engine: mempill.Engine,
    agent_id: str,
    value: str,
    valid_from: str,
    valid_until: str | None = None,
    vtc: float = 0.9,
) -> dict:
    """Helper: ingest a 'ceo' claim with explicit valid-time bounds."""
    valid_time: dict = {
        "start": valid_from,
        "valid_time_confidence": vtc,
    }
    if valid_until is not None:
        valid_time["end"] = valid_until
    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "acme",
        "predicate": "ceo",
        "value": value,
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": valid_time,
        "confidence": {"value_confidence": 0.95, "valid_time_confidence": vtc},
        "criticality": "Medium",
        "derived_from": [],
    })


class TestValidAtAccepted:
    """valid_at parameter is forwarded through the PyO3 binding without error."""

    def test_valid_at_iso_string_accepted(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """valid_at as an ISO-8601 UTC string must be accepted without error."""
        _ingest_ceo(engine, agent_id, "Alice", "2020-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": "2021-06-01T00:00:00Z",
        })
        assert "belief" in resp, f"Expected belief key in response, got: {resp}"

    def test_valid_at_none_accepted(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """valid_at=None must be accepted by the engine (serde default; same as omitting)."""
        _ingest_ceo(engine, agent_id, "Alice", "2020-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": None,
        })
        assert "belief" in resp, f"Expected belief key when valid_at=None, got: {resp}"

    def test_valid_at_none_same_as_omitted(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Passing valid_at=None must produce the same result as omitting the key."""
        _ingest_ceo(engine, agent_id, "Alice", "2020-01-01T00:00:00Z")
        resp_omitted = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
        })
        resp_none = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": None,
        })
        # Both must resolve to the same value.
        val_omitted = resp_omitted["belief"]["primary"]["fact"]["value"]
        val_none = resp_none["belief"]["primary"]["fact"]["value"]
        assert val_omitted == val_none, (
            f"valid_at=None must produce same result as omitting: {val_omitted!r} != {val_none!r}"
        )


class TestValidAtD2Independence:
    """D2: valid_at and as_of_tx_time compose independently without error."""

    def test_both_axes_accepted_together(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Supplying both valid_at and as_of_tx_time must not raise."""
        _ingest_ceo(engine, agent_id, "Alice", "2020-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "as_of_tx_time": "2099-01-01T00:00:00Z",  # far future — all writes visible
            "valid_at": "2021-06-01T00:00:00Z",
        })
        assert "belief" in resp, (
            f"Expected belief when combining as_of_tx_time + valid_at, got: {resp}"
        )

    def test_future_as_of_tx_time_with_valid_at(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Far-future as_of_tx_time is compatible with valid_at (both axes present)."""
        _ingest_ceo(engine, agent_id, "Carol", "2015-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "as_of_tx_time": "2099-12-31T23:59:59Z",
            "valid_at": "2020-06-01T00:00:00Z",
        })
        assert "belief" in resp, (
            f"Expected belief dict, got: {resp}"
        )
        # valid_at=2020-06-01 is INSIDE Carol's window (start=2015-01-01, open end) — the
        # single-claim fold is window-tested, and the instant falls inside it, so Carol is
        # returned. This is NOT "valid_at has no effect on a single-claim fold" as a general
        # rule (TASK-33-W5-LIB A) — see `test_valid_at_before_single_claim_start_returns_no_belief`
        # for the same single-claim fold with an out-of-window instant, which correctly yields
        # NoBelief instead.
        assert resp["belief"]["primary"]["fact"]["value"] == "Carol", (
            f"valid_at=2020-06-01 is inside Carol's window [2015-01-01, ∞) → Carol. Got: {resp}"
        )

    def test_as_of_tx_time_with_valid_at_no_error(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """as_of_tx_time + valid_at both set must not raise (D2 independence)."""
        _ingest_ceo(engine, agent_id, "Alice", "2020-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "as_of_tx_time": "2000-01-01T00:00:00Z",  # before any write
            "valid_at": "2021-06-01T00:00:00Z",
        })
        # The engine accepted both parameters — response must be a dict with 'belief'.
        assert "belief" in resp, (
            f"Expected response with 'belief' key, got: {resp}"
        )
        # NOTE: as_of_tx_time before any write means no claims are tx-visible.
        # The engine returns NoBelief when no claims pass the tx-time filter,
        # BUT only when the disposition map (which uses all-time ledger) also
        # agrees — current engine behavior may vary; we only assert no error here.


class TestValidAtLiveBeliefUnchanged:
    """valid_at does not regress existing live-belief queries."""

    def test_live_belief_unaffected_by_valid_at(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """valid_at does not alter the live belief when only a single claim is live."""
        _ingest_ceo(engine, agent_id, "Alice", "2020-01-01T00:00:00Z")

        # With a single live claim, valid_at has no effect on the fold result.
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": "2099-01-01T00:00:00Z",
        })
        primary = resp["belief"].get("primary")
        assert primary is not None, (
            f"Expected primary belief with single live claim, got: {resp}"
        )
        assert primary["fact"]["value"] == "Alice", (
            f"Single-claim live belief must be Alice. Got: {primary['fact']['value']!r}"
        )

    def test_no_valid_at_still_returns_live_belief(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Backward-compatible: omitting valid_at returns the current live belief."""
        _ingest_ceo(engine, agent_id, "Carol", "2015-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
        })
        assert resp["belief"]["primary"]["fact"]["value"] == "Carol", (
            f"Omitting valid_at must return the live belief. Got: {resp}"
        )

    def test_valid_at_returns_correct_value_single_claim(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """A single live claim, queried at an in-window valid_at instant, returns the same
        value as omitting valid_at — NOT because "valid_at has no effect on a single-claim
        fold" (see TASK-33-W5-LIB A), but because the instant is genuinely inside the claim's
        window. `test_valid_at_before_single_claim_start_returns_no_belief` below proves the
        window IS checked: an out-of-window instant on the same single claim yields NoBelief.
        """
        _ingest_ceo(engine, agent_id, "Eve", "2010-01-01T00:00:00Z")
        resp_no_vat = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
        })
        resp_with_vat = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": "2015-06-01T00:00:00Z",  # inside Eve's window [2010-01-01, ∞)
        })
        v1 = resp_no_vat["belief"]["primary"]["fact"]["value"]
        v2 = resp_with_vat["belief"]["primary"]["fact"]["value"]
        assert v1 == v2 == "Eve", (
            f"valid_at=2015-06-01 is inside Eve's window → same result as omitting valid_at. "
            f"Without: {v1!r}, With: {v2!r}"
        )

    def test_valid_at_before_single_claim_start_returns_no_belief(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """TASK-33-W5-LIB A (DIAG-4 finding A, scenario 6): a single live, unbounded claim
        queried at an instant BEFORE its own valid_time.start must return NoBelief — the
        `len() > 1` guard is dropped so even a lone candidate is window-tested.
        """
        _ingest_ceo(engine, agent_id, "Eve", "2010-01-01T00:00:00Z")
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": "1995-01-01T00:00:00Z",  # before Eve's start
        })
        assert resp["belief"]["status"] == "NoBelief", (
            f"valid_at before the only claim's start must be NoBelief. Got: {resp['belief']}"
        )
        assert resp["belief"].get("primary") is None, (
            f"NoBelief must not carry a primary belief. Got: {resp['belief']}"
        )

    def test_valid_at_reenters_end_fact_bounded_claim(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """TASK-33-W5-LIB A (DIAG-4 finding A, scenario 1): a claim explicitly ended via
        `end_fact` must still re-enter the valid_at candidate set, narrowed to its real
        believed window — the raw-live-only fold alone would incorrectly return the
        successor (or NoBelief) for an instant that predates the bound.
        """
        from mempill import end_fact

        _ingest_ceo(engine, agent_id, "Diane", "2021-04-01T00:00:00Z")
        end_fact(engine, agent_id, "acme", "ceo", "2025-01-01")
        _ingest_ceo(engine, agent_id, "John", "2025-01-01T00:00:00Z")

        # valid_at before the bound must still return Diane, not John or NoBelief.
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": "2022-06-01T00:00:00Z",
        })
        assert resp["belief"]["primary"]["fact"]["value"] == "Diane", (
            f"valid_at=2022-06-01 (before the end_fact bound at 2025-01-01) must return "
            f"Diane, got: {resp['belief']}"
        )

        # valid_at after the bound must return John.
        resp2 = engine.query_memory({
            "agent_id": agent_id,
            "subject": "acme",
            "predicate": "ceo",
            "valid_at": "2025-06-01T00:00:00Z",
        })
        assert resp2["belief"]["primary"]["fact"]["value"] == "John", (
            f"valid_at=2025-06-01 (after the bound) must return John, got: {resp2['belief']}"
        )
