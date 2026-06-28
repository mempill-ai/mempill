"""
test_granularity.py — Per-endpoint date-granularity and honest display-string tests.

Verifies that when a claim is ingested with explicit valid-time granularity
(e.g. ``start_granularity: "month"``), the engine returns:

  1. ``valid_time.start_granularity`` / ``valid_time.end_granularity`` — raw
     granularity strings preserved in the response dict.
  2. ``valid_from_display`` / ``valid_until_display`` — pre-rendered, honest
     display strings on the belief slot.  The invariant is: a Month-granularity
     date must render as ``"YYYY-MM"`` — never ``"YYYY-MM-DD"`` (no fabricated day).

Coverage:
  - Month granularity: display = "YYYY-MM" (no day component); proved via assertion.
  - Year granularity: display = "YYYY".
  - Day granularity: display = "YYYY-MM-DD".
  - Instant granularity: display = "YYYY-MM-DD" (sub-day suppressed).
  - Open end (no end date): valid_until_display absent.
  - Both endpoints with different granularities.
  - Legacy row (no granularity set): display falls back to "YYYY-MM-DD".
  - Ingest → query pipeline: month granularity survives the full write-read cycle.
"""

from __future__ import annotations

import mempill
from mempill.types import ProvenanceLabel


# ── Helpers ───────────────────────────────────────────────────────────────────

def _ingest_with_granularity(
    engine: mempill.Engine,
    agent_id: str,
    *,
    start_dt: str,
    start_gran: str | None = None,
    end_dt: str | None = None,
    end_gran: str | None = None,
    value: str = "test-value",
) -> dict:
    """Ingest a claim with explicit valid-time bounds and optional granularity tags."""
    vt: dict = {"valid_time_confidence": 0.9}
    vt["start"] = start_dt
    if start_gran is not None:
        vt["start_granularity"] = start_gran
    if end_dt is not None:
        vt["end"] = end_dt
    if end_gran is not None:
        vt["end_granularity"] = end_gran

    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": "entity",
        "predicate": "prop",
        "value": value,
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": vt,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.9},
        "criticality": "Low",
        "derived_from": [],
    })


def _query(engine: mempill.Engine, agent_id: str) -> dict:
    resp = engine.query_memory({
        "agent_id": agent_id,
        "subject": "entity",
        "predicate": "prop",
    })
    return resp["belief"]["primary"]


# ── Month granularity ─────────────────────────────────────────────────────────

class TestMonthGranularity:
    """Month-precision start must render as YYYY-MM — never YYYY-MM-DD."""

    def test_start_granularity_month_survives_ingest_query(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """start_granularity='month' must round-trip through ingest→query."""
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-01T00:00:00Z",
            start_gran="month",
        )
        primary = _query(engine, agent_id)
        assert primary["valid_time"].get("start_granularity") == "month", (
            "start_granularity must survive ingest→query as 'month'"
        )

    def test_valid_from_display_is_yyyy_mm_no_day(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Month granularity: display must be YYYY-MM with no day component."""
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-01T00:00:00Z",
            start_gran="month",
        )
        primary = _query(engine, agent_id)
        display = primary.get("valid_from_display")
        assert display == "2020-03", (
            f"Month granularity must render as '2020-03', got: {display!r}"
        )
        # Hard invariant: no fabricated day — only one dash allowed.
        assert display is not None and display.count("-") == 1, (
            f"Month display '2020-03' must have exactly one dash (no day); got: {display!r}"
        )

    def test_valid_until_display_absent_for_open_end(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """No end date → valid_until_display must be absent (not null)."""
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-01T00:00:00Z",
            start_gran="month",
        )
        primary = _query(engine, agent_id)
        assert "valid_until_display" not in primary, (
            "valid_until_display must be absent when end is open-ended, "
            f"but got: {primary.get('valid_until_display')!r}"
        )


# ── Year granularity ──────────────────────────────────────────────────────────

class TestYearGranularity:
    """Year-precision start must render as YYYY — no month or day."""

    def test_valid_from_display_is_yyyy(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-01-01T00:00:00Z",
            start_gran="year",
        )
        primary = _query(engine, agent_id)
        display = primary.get("valid_from_display")
        assert display == "2020", (
            f"Year granularity must render as '2020', got: {display!r}"
        )
        # Must not contain any dash.
        assert display is not None and "-" not in display, (
            f"Year display must not contain a dash; got: {display!r}"
        )

    def test_start_granularity_raw_is_year(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-01-01T00:00:00Z",
            start_gran="year",
        )
        primary = _query(engine, agent_id)
        assert primary["valid_time"].get("start_granularity") == "year"


# ── Day granularity ───────────────────────────────────────────────────────────

class TestDayGranularity:
    """Day-precision start must render as YYYY-MM-DD."""

    def test_valid_from_display_is_yyyy_mm_dd(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-15T00:00:00Z",
            start_gran="day",
        )
        primary = _query(engine, agent_id)
        display = primary.get("valid_from_display")
        assert display == "2020-03-15", (
            f"Day granularity must render as '2020-03-15', got: {display!r}"
        )
        assert display is not None and display.count("-") == 2


# ── Instant granularity ───────────────────────────────────────────────────────

class TestInstantGranularity:
    """Instant granularity renders at day precision (sub-day suppressed)."""

    def test_valid_from_display_renders_at_day_precision(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-15T00:00:00Z",
            start_gran="instant",
        )
        primary = _query(engine, agent_id)
        display = primary.get("valid_from_display")
        assert display == "2020-03-15", (
            f"Instant granularity must render at day precision, got: {display!r}"
        )


# ── Both endpoints ────────────────────────────────────────────────────────────

class TestBothEndpointGranularity:
    """Both start and end endpoints with different granularities."""

    def test_start_month_end_year_displays(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-01T00:00:00Z",
            start_gran="month",
            end_dt="2023-01-01T00:00:00Z",
            end_gran="year",
        )
        primary = _query(engine, agent_id)
        assert primary.get("valid_from_display") == "2020-03", (
            f"Start month display must be '2020-03', got: {primary.get('valid_from_display')!r}"
        )
        assert primary.get("valid_until_display") == "2023", (
            f"End year display must be '2023', got: {primary.get('valid_until_display')!r}"
        )

    def test_raw_granularity_fields_present(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-01T00:00:00Z",
            start_gran="month",
            end_dt="2023-01-01T00:00:00Z",
            end_gran="year",
        )
        primary = _query(engine, agent_id)
        vt = primary["valid_time"]
        assert vt.get("start_granularity") == "month"
        assert vt.get("end_granularity") == "year"


# ── Legacy (no granularity) ───────────────────────────────────────────────────

class TestNoGranularity:
    """Rows ingested without granularity (legacy) fall back to YYYY-MM-DD display."""

    def test_no_granularity_falls_back_to_day_form(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """When start_granularity is absent, display must fall back to YYYY-MM-DD."""
        _ingest_with_granularity(
            engine, agent_id,
            start_dt="2020-03-15T00:00:00Z",
            # No start_gran
        )
        primary = _query(engine, agent_id)
        # No raw granularity key in the response.
        assert "start_granularity" not in primary["valid_time"], (
            "Absent granularity must not appear in valid_time dict"
        )
        # Display falls back to day form.
        display = primary.get("valid_from_display")
        assert display == "2020-03-15", (
            f"Absent granularity must fall back to YYYY-MM-DD; got: {display!r}"
        )

    def test_no_date_at_all_display_absent(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Claim with no valid_time → valid_from_display must be absent."""
        engine.ingest_claim({
            "agent_id": agent_id,
            "subject": "entity",
            "predicate": "prop",
            "value": "no-time",
            "provenance": ProvenanceLabel.external_user_asserted(),
            "cardinality": "Functional",
            "valid_time": None,
            "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
            "criticality": "Low",
            "derived_from": [],
        })
        resp = engine.query_memory({
            "agent_id": agent_id,
            "subject": "entity",
            "predicate": "prop",
        })
        primary = resp["belief"]["primary"]
        assert "valid_from_display" not in primary, (
            "No valid_time → valid_from_display must be absent from belief slot"
        )
        assert "valid_until_display" not in primary, (
            "No valid_time → valid_until_display must be absent from belief slot"
        )
