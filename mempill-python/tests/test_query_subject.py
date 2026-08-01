"""
test_query_subject.py — Exercises Engine.query_subject() through the Python API (gap 🟡10).

query_subject enumerates all resolved beliefs for every predicate stored under a
subject, folding each predicate through the same QueryMemoryUseCase path used by
query_memory. Mirrors the core unit-test scenarios in
mempill-core/src/application/query_subject.rs so surface parity between the Rust
use case and the PyO3 binding is provable:

  - query_subject_returns_all_predicates
  - query_subject_excludes_predicate_after_tx_cutoff
  - query_subject_valid_at_selects_historical_value
  - query_subject_contested_predicate_reports_contested

Response shape (per engine.rs:149-160, list[dict] returned directly — NOT wrapped
in {"entries": [...]}): predicate, value (str|None), status, valid_from_display
(str|None), valid_until_display (str|None), provenance, claim_ref (str|None),
conf (float|None). Sorted by predicate (alphabetical).

IMPORTANT (see test_valid_at.py engine-behaviour notes): valid_at selection only
narrows a LIVE claim set. When a successor is ingested through the public
ingest_claim API with Functional cardinality, the predecessor is marked
Superseded and is no longer selectable via valid_at. To exercise real historical
selection through the public API (without bypassing the ingest pipeline), the
valid_at test below uses SetValued cardinality so both claims remain live
simultaneously — this is the documented supported path for valid_at to actually
change the selected value post-ingest.
"""

from __future__ import annotations

import mempill
from mempill.types import Disposition, ProvenanceLabel


def _ingest(
    engine: mempill.Engine,
    agent_id: str,
    subject: str,
    predicate: str,
    value: str,
    provenance: dict | None = None,
    cardinality: str = "Functional",
    valid_time: dict | None = None,
) -> dict:
    return engine.ingest_claim({
        "agent_id": agent_id,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "provenance": provenance or ProvenanceLabel.external_user_asserted(),
        "cardinality": cardinality,
        "valid_time": valid_time,
        "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.9 if valid_time else 0.0},
        "criticality": "Medium",
        "derived_from": [],
    })


def _query_subject(
    engine: mempill.Engine,
    agent_id: str,
    subject: str,
    valid_at: str | None = None,
    as_of_tx_time: str | None = None,
) -> list[dict]:
    return engine.query_subject({
        "agent_id": agent_id,
        "subject": subject,
        "valid_at": valid_at,
        "as_of_tx_time": as_of_tx_time,
    })


class TestQuerySubjectHappyPath:
    def test_returns_entry_per_distinct_predicate(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Multiple predicates ingested under one subject must all be returned."""
        _ingest(engine, agent_id, "alice-chen", "city", "Berlin")
        _ingest(engine, agent_id, "alice-chen", "employer", "Acme Corp")
        _ingest(engine, agent_id, "alice-chen", "dietary", "vegan")

        entries = _query_subject(engine, agent_id, "alice-chen")

        assert len(entries) == 3, f"Expected 3 predicates, got {len(entries)}: {entries}"
        preds = [e["predicate"] for e in entries]
        assert preds == ["city", "dietary", "employer"], (
            f"Entries must be sorted alphabetically by predicate, got: {preds}"
        )

    def test_entry_has_display_fields(self, engine: mempill.Engine, agent_id: str) -> None:
        """Each entry must carry the display fields per engine.rs:149-160."""
        _ingest(engine, agent_id, "alice-chen", "city", "Berlin")
        entries = _query_subject(engine, agent_id, "alice-chen")

        required_keys = {
            "predicate", "value", "status", "valid_from_display",
            "valid_until_display", "provenance", "claim_ref", "conf",
        }
        assert len(entries) == 1
        missing = required_keys - entries[0].keys()
        assert not missing, f"SubjectFactEntry missing keys: {missing}, got: {entries[0]}"
        assert entries[0]["value"] == "Berlin"
        assert entries[0]["claim_ref"] is not None
        assert entries[0]["conf"] is not None

    def test_response_is_bare_list_not_wrapped(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """query_subject returns list[dict] directly, unlike query_audit's {"entries": [...]}."""
        _ingest(engine, agent_id, "alice-chen", "city", "Berlin")
        result = engine.query_subject({
            "agent_id": agent_id, "subject": "alice-chen",
            "valid_at": None, "as_of_tx_time": None,
        })
        assert isinstance(result, list), f"Expected bare list, got {type(result)}: {result}"


class TestQuerySubjectTxCutoff:
    def test_predicate_ingested_after_cutoff_is_excluded(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """A predicate ingested after as_of_tx_time must not appear in the result."""
        _ingest(engine, agent_id, "alice-chen", "city", "Berlin")

        import datetime
        cutoff = datetime.datetime.now(datetime.timezone.utc).isoformat()

        _ingest(engine, agent_id, "alice-chen", "phone", "+1234")

        entries = _query_subject(engine, agent_id, "alice-chen", as_of_tx_time=cutoff)
        preds = [e["predicate"] for e in entries]
        assert "city" in preds, "city (before cutoff) must be present"
        assert "phone" not in preds, f"phone (after cutoff) must be excluded, got: {preds}"

    def test_no_cutoff_returns_all_predicates(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Omitting as_of_tx_time returns every predicate, including recent ones."""
        _ingest(engine, agent_id, "alice-chen", "city", "Berlin")
        _ingest(engine, agent_id, "alice-chen", "phone", "+1234")

        entries = _query_subject(engine, agent_id, "alice-chen")
        preds = {e["predicate"] for e in entries}
        assert preds == {"city", "phone"}


class TestQuerySubjectValidAt:
    def test_valid_at_selects_historical_value(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """valid_at narrows a predicate to the value that was true at that instant.

        Uses SetValued cardinality so both claims remain live simultaneously
        (Functional cardinality would supersede the predecessor on the second
        ingest, making it unreachable via the public API — see module docstring).
        """
        _ingest(
            engine, agent_id, "alice-chen", "city", "Berlin",
            cardinality="SetValued",
            valid_time={"start": "2020-01-01T00:00:00Z", "end": "2023-01-01T00:00:00Z",
                        "valid_time_confidence": 0.9},
        )
        _ingest(
            engine, agent_id, "alice-chen", "city", "Paris",
            cardinality="SetValued",
            valid_time={"start": "2023-01-01T00:00:00Z", "valid_time_confidence": 0.9},
        )

        entries_2021 = _query_subject(engine, agent_id, "alice-chen", valid_at="2021-06-01T00:00:00Z")
        city_2021 = next(e for e in entries_2021 if e["predicate"] == "city")
        assert city_2021["value"] == "Berlin", (
            f"valid_at=2021 must resolve to Berlin, got: {city_2021}"
        )
        assert city_2021["status"] == "Resolved"

        entries_2024 = _query_subject(engine, agent_id, "alice-chen", valid_at="2024-01-01T00:00:00Z")
        city_2024 = next(e for e in entries_2024 if e["predicate"] == "city")
        assert city_2024["value"] == "Paris", (
            f"valid_at=2024 must resolve to Paris, got: {city_2024}"
        )


class TestQuerySubjectContested:
    def test_contested_predicate_reports_contested_status(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """A predicate with conflicting claims must report status=Contested."""
        _ingest(engine, agent_id, "alice-chen", "city", "Berlin")
        _ingest(engine, agent_id, "alice-chen", "employer", "Acme Corp")

        r1 = _ingest(engine, agent_id, "alice-chen", "name", "Alice")
        assert r1["disposition"] == Disposition.CommittedCheap

        r2 = _ingest(
            engine, agent_id, "alice-chen", "name", "Alicia",
            provenance=ProvenanceLabel.external_first_hand(),
        )
        assert r2["disposition"] == Disposition.Contested, (
            f"Second conflicting claim must be Contested, got {r2['disposition']!r}"
        )

        entries = _query_subject(engine, agent_id, "alice-chen")
        preds = {e["predicate"] for e in entries}
        assert preds == {"city", "employer", "name"}, (
            "All predicates (including the contested one) must still be enumerated"
        )

        name_entry = next(e for e in entries if e["predicate"] == "name")
        assert name_entry["status"] == "Contested", (
            f"name predicate must report Contested status, got: {name_entry}"
        )

        # Other, uncontested predicates must be unaffected.
        city_entry = next(e for e in entries if e["predicate"] == "city")
        assert city_entry["status"] != "Contested"


class TestQuerySubjectEmpty:
    def test_unknown_subject_returns_empty_list(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """A subject with no claims must return an empty list, not an error."""
        entries = _query_subject(engine, agent_id, "ghost-subject-no-claims")
        assert entries == [], f"Expected empty list for unknown subject, got: {entries}"

    def test_empty_subject_after_other_subjects_ingested(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Other subjects' claims must not leak into an unrelated subject's query."""
        _ingest(engine, agent_id, "bob-jones", "city", "Rome")
        entries = _query_subject(engine, agent_id, "alice-chen")
        assert entries == [], (
            f"Querying a subject with no claims of its own must not return "
            f"another subject's entries, got: {entries}"
        )
