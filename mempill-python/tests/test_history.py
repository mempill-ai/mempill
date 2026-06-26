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
        """Ingest 3 CEO succession facts with explicit valid_from dates.

        Each ingest is followed by a reconcile call so the engine folds
        the contested claims into a proper Superseded/Current chain.

        Returns (alice_ref, john_ref, bob_ref).
        """
        r_alice = remember(engine, AGENT, "acme", "ceo", "Alice",
                           opts=RememberOptions(valid_from="2010-01-01"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["acme", "ceo"]]})
        r_john = remember(engine, AGENT, "acme", "ceo", "John",
                          opts=RememberOptions(valid_from="2018-06-01"))
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

    def test_succession_predecessors_superseded(self, engine: mempill.Engine) -> None:
        self._ingest_succession(engine)
        h = history(engine, AGENT, "acme", "ceo")
        assert h.entries[0].status == "Superseded", "Alice must be Superseded"
        assert h.entries[1].status == "Superseded", "John must be Superseded"

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


# ── Inline timeline demo (runs as a test) ────────────────────────────────────

class TestInlineDemo:
    def test_inline_timeline_demo(self, engine: mempill.Engine) -> None:
        """Demonstrates history() as a quick timeline inspection tool.

        Each remember() is followed by reconcile() so the engine folds
        contested entries into Superseded / Current.
        """
        remember(engine, AGENT, "demo-corp", "ceo", "Alice",
                 opts=RememberOptions(valid_from="2010"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["demo-corp", "ceo"]]})
        remember(engine, AGENT, "demo-corp", "ceo", "John",
                 opts=RememberOptions(valid_from="2018"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["demo-corp", "ceo"]]})
        remember(engine, AGENT, "demo-corp", "ceo", "Bob",
                 opts=RememberOptions(valid_from="2023"))
        engine.reconcile({"agent_id": AGENT, "subject_lines": [["demo-corp", "ceo"]]})

        h = history(engine, AGENT, "demo-corp", "ceo")
        timeline = [(e.value, e.status) for e in h]

        assert timeline == [
            ("Alice", "Superseded"),
            ("John",  "Superseded"),
            ("Bob",   "Current"),
        ], f"Unexpected timeline: {timeline}"
