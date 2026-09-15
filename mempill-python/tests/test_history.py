"""
test_history.py — Integration tests for history() / History / HistoryEntry.

Covers:
  - Empty subject-line → History.is_empty() True, .entries == []
  - Single claim → one Current entry, open-ended valid_until
  - Succession (Alice → John → Bob via valid_from) → entries ordered oldest→newest,
    predecessors Superseded, last entry Current
  - history().current().value == recall().value (consistency guarantee)
  - History is iterable (for-loop)
  - HistoryEntry fields: claim_ref (UUID str), value, valid_from, valid_until,
    status ("Current"/"Superseded"), provenance, value_confidence
  - OracleEngine also exposes query_history (duck-typed; smoke test via
    open_oracle_in_memory with a no-op oracle)
"""

from __future__ import annotations

import pytest
import mempill
from mempill import (
    remember,
    recall,
    RememberOptions,
    history,
    History,
    HistoryEntry,
)


# ── Fixtures ──────────────────────────────────────────────────────────────────

@pytest.fixture()
def engine() -> mempill.Engine:
    return mempill.open_in_memory()


AGENT = "history-test-agent"


# ── Empty subject-line ────────────────────────────────────────────────────────

class TestEmptyHistory:
    def test_empty_returns_history_object(self, engine: mempill.Engine) -> None:
        h = history(engine, AGENT, "nobody", "nothing")
        assert isinstance(h, History)

    def test_empty_is_empty_true(self, engine: mempill.Engine) -> None:
        h = history(engine, AGENT, "nobody", "nothing")
        assert h.is_empty() is True

    def test_empty_entries_list(self, engine: mempill.Engine) -> None:
        h = history(engine, AGENT, "nobody", "nothing")
        assert h.entries == []

    def test_empty_current_is_none(self, engine: mempill.Engine) -> None:
        h = history(engine, AGENT, "nobody", "nothing")
        assert h.current() is None

    def test_empty_len_is_zero(self, engine: mempill.Engine) -> None:
        h = history(engine, AGENT, "nobody", "nothing")
        assert len(h) == 0


# ── Single claim ──────────────────────────────────────────────────────────────

class TestSingleClaim:
    def test_single_claim_one_entry(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        assert len(h.entries) == 1

    def test_single_claim_status_current(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        assert h.entries[0].status == "Current"

    def test_single_claim_value(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        assert h.entries[0].value == "Berlin"

    def test_single_claim_valid_until_is_none(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        assert h.entries[0].valid_until is None

    def test_single_claim_claim_ref_is_uuid_string(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        cr = h.entries[0].claim_ref
        assert isinstance(cr, str)
        assert len(cr) == 36  # UUID format
        assert cr.count("-") == 4

    def test_single_claim_provenance(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        assert h.entries[0].provenance == "External/UserAsserted"

    def test_single_claim_value_confidence(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin",
                 opts=RememberOptions(confidence=0.75))
        h = history(engine, AGENT, "user", "city")
        assert abs(h.entries[0].value_confidence - 0.75) < 0.01

    def test_is_empty_false_after_insert(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "user", "city", "Berlin")
        h = history(engine, AGENT, "user", "city")
        assert h.is_empty() is False


# ── CEO succession (Alice → John → Bob) ──────────────────────────────────────

class TestSuccession:
    def _ingest_succession(self, engine: mempill.Engine) -> tuple[str, str, str]:
        """Ingest 3 CEO succession facts as a GENUINE trusted, non-overlapping chain
        (each predecessor's valid_until = its successor's valid_from).

        A predecessor with NO explicit end is open-ended and therefore genuinely
        OVERLAPS any later-starting claim (both extend to infinity) — under the
        fold-derived history design that is honestly Contested, not a succession.
        `reconcile()` also never silently resolves a Contested pair (only
        `submit_adjudication` may) — so this helper must supply real, non-overlapping
        windows for the write path to classify ingest-time as a clean `Succession`.

        Returns (alice_ref, john_ref, bob_ref).
        """
        r_alice = remember(engine, AGENT, "acme", "ceo", "Alice",
                           opts=RememberOptions(valid_from="2010-01-01", valid_until="2018-06-01"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["acme", "ceo"]]})
        r_john = remember(engine, AGENT, "acme", "ceo", "John",
                          opts=RememberOptions(valid_from="2018-06-01", valid_until="2023-03-15"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["acme", "ceo"]]})
        r_bob = remember(engine, AGENT, "acme", "ceo", "Bob",
                         opts=RememberOptions(valid_from="2023-03-15"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["acme", "ceo"]]})
        return r_alice.claim_ref, r_john.claim_ref, r_bob.claim_ref

    def test_succession_entry_count(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        assert len(h.entries) == 3

    def test_succession_ordered_oldest_first(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        values = [e.value for e in h.entries]
        assert values == ["Alice", "John", "Bob"], (
            f"Expected oldest→newest order [Alice, John, Bob], got {values}"
        )

    def test_succession_predecessors_ended(self, engine: mempill.Engine) -> None:
        """A genuine valid-time succession never explicitly supersedes its predecessors
        (only HeavyPath / oracle-affirmed supersession writes a Bound assertion) — so
        Alice and John stay raw-live with status "Ended" (window closed, no conflict,
        not the narrowed-current selection), not "Superseded" (explicit bound/dispose).
        """
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        assert h.entries[0].status == "Ended", "Alice must be Ended (never explicitly Bound)"
        assert h.entries[1].status == "Ended", "John must be Ended (never explicitly Bound)"

    def test_succession_last_entry_current(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        assert h.entries[2].status == "Current", "Bob (latest) must be Current"

    def test_succession_current_matches_recall(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        r = recall(engine, AGENT, "acme", "ceo")
        current_entry = h.current()
        assert current_entry is not None
        assert current_entry.value == r.value, (
            f"history().current().value={current_entry.value!r} "
            f"must equal recall().value={r.value!r}"
        )

    def test_succession_valid_from_set_on_entries(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        # All three have high-confidence valid_from — must be non-None
        for entry in h.entries:
            assert entry.valid_from is not None, (
                f"Entry {entry.value!r} has valid_from=None but a date was provided"
            )

    def test_succession_predecessors_have_valid_until(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        # Alice and John are superseded → their valid_until must be non-None
        assert h.entries[0].valid_until is not None, "Alice's slot must be closed"
        assert h.entries[1].valid_until is not None, "John's slot must be closed"

    def test_succession_last_entry_valid_until_none(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        assert h.entries[2].valid_until is None, "Bob (current) must be open-ended"

    def test_current_shortcut_returns_bob(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        current = h.current()
        assert current is not None
        assert current.value == "Bob"


# ── Iterable / convenience ────────────────────────────────────────────────────

class TestHistoryIterable:
    def test_for_loop_over_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "widget", "color", "red")
        remember(engine, AGENT, "widget", "color", "blue",
                 opts=RememberOptions(valid_from="2024-01-01"))
        h = history(engine, AGENT, "widget", "color")
        collected = [e.value for e in h]
        assert len(collected) == 2

    def test_history_entry_is_dataclass(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "widget", "color", "red")
        h = history(engine, AGENT, "widget", "color")
        entry = h.entries[0]
        assert isinstance(entry, HistoryEntry)


# ── OracleEngine smoke test ───────────────────────────────────────────────────

class TestOracleEngineQueryHistory:
    def test_oracle_engine_has_query_history(self) -> None:
        class _Noop:
            def request_adjudication(self, agent_id: str, request: dict) -> str:
                return "550e8400-e29b-41d4-a716-446655440000"

        oracle_engine = mempill.open_oracle_in_memory(_Noop())
        assert hasattr(oracle_engine, "query_history"), (
            "OracleEngine must expose query_history"
        )

    def test_oracle_engine_history_empty(self) -> None:
        class _Noop:
            def request_adjudication(self, agent_id: str, request: dict) -> str:
                return "550e8400-e29b-41d4-a716-446655440000"

        oracle_engine = mempill.open_oracle_in_memory(_Noop())
        h = history(oracle_engine, "test-agent", "entity", "prop")
        assert h.is_empty()

    def test_oracle_engine_history_round_trip(self) -> None:
        class _Noop:
            def request_adjudication(self, agent_id: str, request: dict) -> str:
                return "550e8400-e29b-41d4-a716-446655440000"

        oracle_engine = mempill.open_oracle_in_memory(_Noop())
        remember(oracle_engine, "test-agent", "entity", "prop", "value1")
        h = history(oracle_engine, "test-agent", "entity", "prop")
        assert len(h.entries) == 1
        assert h.entries[0].value == "value1"
        assert h.entries[0].status == "Current"


# ── TASK-32 — granularity / honest display fields ────────────────────────────
#
# NOTE: the pure-Python `remember()` ergonomic helper does not yet propagate
# start_granularity/end_granularity on write (a pre-existing, separately-tracked
# gap in `RememberOptions`/`_to_rfc3339` — see ergonomic.py's `_to_rfc3339`
# docstring). These tests use the raw `engine.ingest_claim()` dict path (which
# DOES support start_granularity/end_granularity verbatim, mirroring
# test_granularity.py's `_ingest_with_granularity` helper) so the read-path
# (`history()`) granularity plumbing under test here is exercised honestly.

def _ingest_with_granularity(
    engine: mempill.Engine,
    agent_id: str,
    subject: str,
    predicate: str,
    value: str,
    *,
    start_dt: str | None = None,
    start_gran: str | None = None,
    end_dt: str | None = None,
    end_gran: str | None = None,
    valid_time_confidence: float = 0.9,
) -> dict:
    vt: dict = {"valid_time_confidence": valid_time_confidence}
    if start_dt is not None:
        vt["start"] = start_dt
    if start_gran is not None:
        vt["start_granularity"] = start_gran
    if end_dt is not None:
        vt["end"] = end_dt
    if end_gran is not None:
        vt["end_granularity"] = end_gran

    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "provenance": {"type": "External", "kind": "UserAsserted"},
        "cardinality": "Functional",
        "valid_time": vt,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": valid_time_confidence},
        "criticality": "Low",
        "derived_from": [],
    })


class TestHistoryGranularity:
    """Cross-precision timeline: month/year/day granularities + one legacy row,
    plus a supersession case asserting the derived valid_until carries the
    SUCCESSOR-side granularity (not the predecessor's own end_granularity).
    """

    def test_single_entry_reports_own_start_granularity_month(self, engine: mempill.Engine) -> None:
        _ingest_with_granularity(
            engine, AGENT, "person", "birth_month", "value",
            start_dt="2020-03-01T00:00:00Z", start_gran="month",
        )
        h = history(engine, AGENT, "person", "birth_month")
        assert h.entries[0].valid_from_granularity == "month"
        assert h.entries[0].valid_from_display == "2020-03"
        # Open-ended (only) entry → no derived bound.
        assert h.entries[0].valid_until_granularity is None
        assert h.entries[0].valid_until_display is None

    def test_single_entry_year_granularity(self, engine: mempill.Engine) -> None:
        _ingest_with_granularity(
            engine, AGENT, "person2", "founded", "value",
            start_dt="2020-01-01T00:00:00Z", start_gran="year",
        )
        h = history(engine, AGENT, "person2", "founded")
        assert h.entries[0].valid_from_granularity == "year"
        assert h.entries[0].valid_from_display == "2020"

    def test_single_entry_day_granularity(self, engine: mempill.Engine) -> None:
        _ingest_with_granularity(
            engine, AGENT, "person3", "event", "value",
            start_dt="2020-03-15T00:00:00Z", start_gran="day",
        )
        h = history(engine, AGENT, "person3", "event")
        assert h.entries[0].valid_from_granularity == "day"
        assert h.entries[0].valid_from_display == "2020-03-15"

    def test_legacy_row_no_granularity_has_none(self, engine: mempill.Engine) -> None:
        """A row with dates but no granularity tag — legacy/unknown precision."""
        _ingest_with_granularity(
            engine, AGENT, "legacy-subj", "legacy-pred", "value",
            start_dt="2020-03-15T00:00:00Z",
        )
        h = history(engine, AGENT, "legacy-subj", "legacy-pred")
        assert h.entries[0].valid_from_granularity is None
        # Legacy fallback still renders a day-form display (never fabricates precision text,
        # but also doesn't withhold display entirely — matches format_valid_time_endpoint).
        assert h.entries[0].valid_from_display == "2020-03-15"
        assert h.entries[0].valid_until_granularity is None
        assert h.entries[0].valid_until_display is None

    def test_supersession_valid_until_uses_own_end_granularity(self, engine: mempill.Engine) -> None:
        """Predecessor (Day-precision start, EXPLICIT Day-precision end at the
        successor's start) followed by a Year-precision successor: the predecessor's
        valid_until_granularity/_display must be its OWN end_granularity (Day), never
        the successor's start_granularity (Year) — own end always wins over a later
        successor's key (bug A fix). Two mutually-trusted open-ended claims would
        instead be a genuine valid-time overlap (Contested, no narrowing) under the
        fold-derived history design, so the predecessor needs an explicit end here.
        """
        _ingest_with_granularity(
            engine, AGENT, "corp", "ceo", "Alice",
            start_dt="2019-06-15T00:00:00Z", start_gran="day",
            end_dt="2020-01-01T00:00:00Z", end_gran="day",
        )
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["corp", "ceo"]]})
        _ingest_with_granularity(
            engine, AGENT, "corp", "ceo", "Bob",
            start_dt="2020-01-01T00:00:00Z", start_gran="year",
        )
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["corp", "ceo"]]})

        h = history(engine, AGENT, "corp", "ceo")
        assert [e.value for e in h.entries] == ["Alice", "Bob"]

        alice, bob = h.entries
        assert alice.valid_from_granularity == "day"
        assert alice.valid_from_display == "2019-06-15"
        assert alice.valid_until_granularity == "day", (
            "Alice's valid_until_granularity must be her OWN end_granularity (Day), "
            "not Bob's (successor) start_granularity (Year) — own end always wins"
        )
        assert alice.valid_until_display == "2020-01-01", (
            "Alice's valid_until_display must render at HER OWN Day precision"
        )

        assert bob.valid_from_granularity == "year"
        assert bob.valid_from_display == "2020"
        assert bob.valid_until_granularity is None, "Bob (current, open-ended) has no bound"
        assert bob.valid_until_display is None

    def test_mixed_precision_three_way_succession(self, engine: mempill.Engine) -> None:
        """Month → Day → Year succession, each predecessor with an EXPLICIT own end at
        the successor's start: each predecessor's derived valid_until must match its
        OWN end_granularity (own end always wins over a later successor's key — bug A
        fix), never the successor's start precision.
        """
        _ingest_with_granularity(
            engine, AGENT, "mixed-corp", "ceo", "Alice",
            start_dt="2010-01-01T00:00:00Z", start_gran="month",
            end_dt="2018-06-01T00:00:00Z", end_gran="month",
        )
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["mixed-corp", "ceo"]]})
        _ingest_with_granularity(
            engine, AGENT, "mixed-corp", "ceo", "John",
            start_dt="2018-06-01T00:00:00Z", start_gran="day",
            end_dt="2023-01-01T00:00:00Z", end_gran="day",
        )
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["mixed-corp", "ceo"]]})
        _ingest_with_granularity(
            engine, AGENT, "mixed-corp", "ceo", "Bob",
            start_dt="2023-01-01T00:00:00Z", start_gran="year",
        )
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["mixed-corp", "ceo"]]})

        h = history(engine, AGENT, "mixed-corp", "ceo")
        assert [e.value for e in h.entries] == ["Alice", "John", "Bob"]
        alice, john, bob = h.entries

        assert alice.valid_from_granularity == "month"
        assert alice.valid_until_granularity == "month", "Alice's bound = her OWN end precision"
        assert alice.valid_until_display == "2018-06"

        assert john.valid_from_granularity == "day"
        assert john.valid_until_granularity == "day", "John's bound = his OWN end precision"
        assert john.valid_until_display == "2023-01-01"

        assert bob.valid_from_granularity == "year"
        assert bob.valid_until_granularity is None


# ── Inline timeline demo (runs as a test) ────────────────────────────────────

class TestInlineDemo:
    def test_inline_timeline_demo(self, engine: mempill.Engine) -> None:
        """Demonstrates history() as a quick timeline inspection tool.

        Each remember() supplies an explicit valid_until closing the window at the next
        entry's valid_from, forming a GENUINE trusted, non-overlapping succession — an
        open-ended predecessor would genuinely OVERLAP a later-starting claim (both
        extend to infinity) and correctly surface as Contested, not a succession.
        `reconcile()` never silently resolves a Contested pair (only
        `submit_adjudication` may); the calls here are effectively no-ops once ingest
        already classified each write as a clean Succession.
        """
        remember(engine, AGENT, "demo-corp", "ceo", "Alice",
                 opts=RememberOptions(valid_from="2010", valid_until="2018"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["demo-corp", "ceo"]]})
        remember(engine, AGENT, "demo-corp", "ceo", "John",
                 opts=RememberOptions(valid_from="2018", valid_until="2023"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["demo-corp", "ceo"]]})
        remember(engine, AGENT, "demo-corp", "ceo", "Bob",
                 opts=RememberOptions(valid_from="2023"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["demo-corp", "ceo"]]})

        h = history(engine, AGENT, "demo-corp", "ceo")
        timeline = [(e.value, e.status) for e in h]

        # Alice/John are never explicitly Bound (a clean succession never supersedes) —
        # their correct status is "Ended" (window closed, no conflict, not narrowed-current).
        assert timeline == [
            ("Alice", "Ended"),
            ("John",  "Ended"),
            ("Bob",   "Current"),
        ], f"Unexpected timeline: {timeline}"
