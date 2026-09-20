"""
test_remember_granularity.py — remember() must preserve the PRECISION of
opts.valid_from / opts.valid_until, matching end_fact() and the Rust ergonomic
remember().

Before this wiring, remember(opts=RememberOptions(valid_from="2021-04")) stored only
the normalized RFC3339 instant with no start_granularity, so history()/recall()
rendered the fabricated day "2021-04-01" instead of "2021-04". The granularity now
travels with the request (`valid_time.start_granularity` / `.end_granularity`),
derived by the engine's single Rust date parser (mempill._mempill.date_granularity_of)
from the ORIGINAL caller string, never from the normalized instant.
"""

from __future__ import annotations

import pytest

import mempill
from mempill import remember, recall, history, end_fact, RememberOptions


@pytest.fixture()
def engine() -> mempill.Engine:
    return mempill.open_in_memory()


AGENT = "remember-granularity-test-agent"


class TestRememberValidFromGranularity:
    def test_month_precision_round_trips_to_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-vf-month-subj", "gran-vf-month-pred", "v1",
                  RememberOptions(valid_from="2021-04"))

        entry = history(engine, AGENT, "gran-vf-month-subj", "gran-vf-month-pred").entries[0]
        assert entry.valid_from_granularity == "month"
        assert entry.valid_from_display == "2021-04"

    def test_year_precision_round_trips_to_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-vf-year-subj", "gran-vf-year-pred", "v1",
                  RememberOptions(valid_from="2021"))

        entry = history(engine, AGENT, "gran-vf-year-subj", "gran-vf-year-pred").entries[0]
        assert entry.valid_from_granularity == "year"
        assert entry.valid_from_display == "2021"

    def test_day_precision_round_trips_to_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-vf-day-subj", "gran-vf-day-pred", "v1",
                  RememberOptions(valid_from="2021-04-15"))

        entry = history(engine, AGENT, "gran-vf-day-subj", "gran-vf-day-pred").entries[0]
        assert entry.valid_from_granularity == "day"
        assert entry.valid_from_display == "2021-04-15"

    def test_full_timestamp_is_instant(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-vf-inst-subj", "gran-vf-inst-pred", "v1",
                  RememberOptions(valid_from="2021-04-15T10:30:00Z"))

        entry = history(engine, AGENT, "gran-vf-inst-subj", "gran-vf-inst-pred").entries[0]
        assert entry.valid_from_granularity == "instant"
        # The engine renders an instant's display as its calendar date, not the
        # full timestamp — read from the entry, not assumed.
        assert entry.valid_from_display == "2021-04-15"


class TestRememberValidUntilGranularity:
    def test_month_precision_round_trips_to_history(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "gran-vu-month-subj", "gran-vu-month-pred", "v1",
                  RememberOptions(valid_from="2020-01-01", valid_until="2025-11"))

        entry = history(engine, AGENT, "gran-vu-month-subj", "gran-vu-month-pred").entries[0]
        assert entry.valid_until_granularity == "month"
        assert entry.valid_until_display == "2025-11"


class TestRememberNoDatesSendsNoGranularityKeys:
    def test_no_dates_no_granularity_keys(self, engine: mempill.Engine) -> None:
        # Existing behavior: no valid_from/valid_until means the raw request's
        # valid_time dict carries no start/end/start_granularity/end_granularity —
        # only remember()'s internal request-building is under test here, so we
        # confirm indirectly via a clean open-ended Current entry.
        remember(engine, AGENT, "gran-none-subj", "gran-none-pred", "v1")

        entry = history(engine, AGENT, "gran-none-subj", "gran-none-pred").entries[0]
        assert entry.valid_from is None
        assert entry.valid_until is None
        assert entry.valid_from_granularity is None
        assert entry.valid_until_granularity is None
        assert entry.status == "Current"


class TestRememberEndFactRememberCombinedFlow:
    """remember -> end_fact -> remember succession, precision preserved throughout."""

    def test_diane_joan_succession(self, engine: mempill.Engine) -> None:
        remember(engine, AGENT, "combined-subj", "combined-pred", "Diane",
                  RememberOptions(valid_from="2021-04"))
        end_fact(engine, AGENT, "combined-subj", "combined-pred", "2024-09")
        remember(engine, AGENT, "combined-subj", "combined-pred", "Joan",
                  RememberOptions(valid_from="2024-09"))

        entries = history(engine, AGENT, "combined-subj", "combined-pred").entries
        assert len(entries) == 2

        diane, joan = entries
        assert diane.value == "Diane"
        assert diane.status == "Superseded"
        assert diane.valid_from_display == "2021-04"
        assert diane.valid_until_display == "2024-09"

        assert joan.value == "Joan"
        assert joan.status == "Current"
        assert joan.valid_from_display == "2024-09"
        assert joan.valid_until_display is None

        result = recall(engine, AGENT, "combined-subj", "combined-pred")
        assert not result.is_contested()
        assert result.value == "Joan"
