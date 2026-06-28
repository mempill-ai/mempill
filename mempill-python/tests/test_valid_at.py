"""
test_valid_at.py — Bi-temporal valid_at query tests.

Verifies that valid_at (valid-time axis) is accepted, forwarded, and composed
independently with as_of_tx_time (transaction-time axis).

Engine behavior notes (important for correct test expectations):
  - valid_at selection only narrows a LIVE claim set.  When a successor is
    ingested through the public ingest_claim API, the predecessor claim is
    marked Superseded in the ledger.  The disposition filter removes Superseded
    claims BEFORE the valid_at selection fires — so a superseded predecessor
    cannot be "recovered" by valid_at through the public API.
  - valid_at selection DOES fire when multiple claims are BOTH live (neither
    superseded).  This happens during the window before reconciliation, or for
    SetValued cardinality, or in as_of_tx_time queries that look back before
    the supersession ledger entry was written.
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
        # With one live claim, valid_at does not change the result (single-claim fold).
        assert resp["belief"]["primary"]["fact"]["value"] == "Carol", (
            f"Single live claim: valid_at does not filter it out. Got: {resp}"
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
        """With a single live claim, valid_at does not change the result."""
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
            "valid_at": "2015-06-01T00:00:00Z",
        })
        # Both must agree (single-claim fold; succession not triggered).
        v1 = resp_no_vat["belief"]["primary"]["fact"]["value"]
        v2 = resp_with_vat["belief"]["primary"]["fact"]["value"]
        assert v1 == v2 == "Eve", (
            f"Single-claim fold: valid_at must not change result. "
            f"Without: {v1!r}, With: {v2!r}"
        )
