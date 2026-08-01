"""
test_audit_pagination.py — Multi-page walk of Engine.query_audit() via
limit + from_tx_time continuation (gap 🟡9, Python side).

SURFACE NOTE (not a defect fixed here, documented behaviour):
  query_audit's `from_tx_time` bound is INCLUSIVE (mempill-sqlite/src/store.rs
  load_ledger: `WHERE recorded_at >= ?`), and results are returned in
  recorded_at DESCENDING order (newest first) — see
  mempill-core/src/engine/audit_ledger.rs::query_ledger, which reverses the
  ASC-ordered SQL page for "replay chronological order" but the reversal
  actually yields DESC as observed through the public API. Because the cursor
  is inclusive and there is no monotonic entry-id cursor, a naive continuation
  using `from_tx_time = last_seen_recorded_at` re-fetches the boundary entry on
  every page transition. This test walks pages using the correct continuation
  strategy (cursor = the MOST RECENT recorded_at seen so far, i.e. the first
  element of each DESC-ordered page) and de-duplicates by `entry_id` client-side
  to prove full coverage with no loss — but the exposed API requires the CALLER
  to do this de-duplication; there is no cursor/limit variant that returns each
  entry exactly once server-side. See RECOMMENDATIONS in the task-level report:
  a from_tx_time-exclusive variant or an entry_id-based cursor would remove this
  caller-side burden and the (rare, timestamp-collision) risk of a stalled walk
  when more entries share one recorded_at value than fit in a single page.
"""

from __future__ import annotations

import mempill
from mempill.types import ProvenanceLabel


def _ingest_n(engine: mempill.Engine, agent_id: str, n: int) -> None:
    for i in range(n):
        result = engine.ingest_claim({
            "agent_id": agent_id,
            "subject": "user",
            "predicate": f"prop-{i}",
            "value": f"val-{i}",
            "provenance": ProvenanceLabel.external_user_asserted(),
            "cardinality": "Functional",
            "valid_time": None,
            "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
            "criticality": "Medium",
            "derived_from": [],
        })
        assert result["disposition"] == "CommittedCheap", (
            f"Setup ingest {i} unexpectedly non-committed: {result}"
        )


def _walk_all_pages(engine: mempill.Engine, agent_id: str, page_size: int) -> tuple[dict, int]:
    """Multi-page walk using limit + from_tx_time continuation, deduped by entry_id.

    Cursor strategy: each page is returned in recorded_at DESCENDING order, so the
    first element of a page is the most-recently-recorded entry fetched so far.
    Using that as the next from_tx_time (inclusive lower bound) guarantees no
    chronological gap between pages; the resulting single-entry overlap at each
    page boundary is removed via entry_id de-duplication.

    Returns (seen: dict[entry_id -> entry], page_count: int).
    """
    seen: dict[str, dict] = {}
    cursor: str | None = None
    page_count = 0
    max_pages = 1000  # guard against an accidental infinite loop in the test itself

    while page_count < max_pages:
        page_count += 1
        page = engine.query_audit({
            "agent_id": agent_id,
            "claim_ref": None,
            "from_tx_time": cursor,
            "limit": page_size,
        })["entries"]

        if not page:
            break

        for entry in page:
            seen[entry["entry_id"]] = entry

        if len(page) < page_size:
            break  # short page => exhausted

        next_cursor = page[0]["recorded_at"]
        assert next_cursor != cursor, (
            "Pagination cursor failed to advance — page boundary entries all share "
            "the same recorded_at as the previous cursor; this would stall the walk."
        )
        cursor = next_cursor

    return seen, page_count


class TestAuditPaginationFullCoverage:
    def test_multi_page_walk_covers_every_entry_exactly_once(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Paginating with a small page size must yield the same entry set as one
        large-limit call, with each entry counted exactly once (no dup, no skip)."""
        _ingest_n(engine, agent_id, 12)

        ground_truth = engine.query_audit({
            "agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 1000,
        })["entries"]
        assert len(ground_truth) == 12, f"Expected 12 ledger entries, got {len(ground_truth)}"

        seen, page_count = _walk_all_pages(engine, agent_id, page_size=5)

        assert page_count > 1, "Test must exercise multiple pages (page_size < total)"
        assert len(seen) == len(ground_truth), (
            f"Deduped multi-page walk collected {len(seen)} entries, "
            f"expected {len(ground_truth)} (ground truth via single large-limit call)"
        )
        assert set(seen.keys()) == {e["entry_id"] for e in ground_truth}, (
            "Multi-page walk must cover exactly the same entry_id set as the "
            "ground-truth single call — no duplicate double-counted and no entry skipped"
        )

    def test_page_size_evenly_divides_total_still_full_coverage(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Page size exactly dividing the total entry count must not lose or
        duplicate the final page (boundary edge case: last page == full page_size)."""
        _ingest_n(engine, agent_id, 10)

        ground_truth = engine.query_audit({
            "agent_id": agent_id, "claim_ref": None, "from_tx_time": None, "limit": 1000,
        })["entries"]
        assert len(ground_truth) == 10

        seen, _ = _walk_all_pages(engine, agent_id, page_size=5)
        assert len(seen) == 10, f"Expected full coverage of 10 entries, got {len(seen)}"

    def test_page_size_larger_than_total_single_page(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """When page_size exceeds the total entry count, the walk must terminate
        after a single page with full coverage."""
        _ingest_n(engine, agent_id, 3)

        seen, page_count = _walk_all_pages(engine, agent_id, page_size=50)
        assert page_count == 1, f"Expected exactly 1 page, got {page_count}"
        assert len(seen) == 3

    def test_empty_ledger_walk_terminates_with_no_entries(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """An agent with no ledger entries must terminate the walk cleanly."""
        seen, page_count = _walk_all_pages(engine, agent_id, page_size=5)
        assert seen == {}
        assert page_count == 1

    def test_walk_result_matches_claim_refs_ingested(
        self, engine: mempill.Engine, agent_id: str
    ) -> None:
        """Every claim_ref ingested must be traceable through the paginated walk."""
        claim_refs = []
        for i in range(9):
            result = engine.ingest_claim({
                "agent_id": agent_id,
                "subject": "user",
                "predicate": f"attr-{i}",
                "value": f"v{i}",
                "provenance": ProvenanceLabel.external_user_asserted(),
                "cardinality": "Functional",
                "valid_time": None,
                "confidence": {"value_confidence": 0.9, "valid_time_confidence": 0.0},
                "criticality": "Medium",
                "derived_from": [],
            })
            claim_refs.append(result["claim_ref"])

        seen, _ = _walk_all_pages(engine, agent_id, page_size=4)
        walked_claim_refs = {e["claim_ref"] for e in seen.values()}
        assert walked_claim_refs == set(claim_refs), (
            f"Paginated walk claim_refs {walked_claim_refs} must match ingested "
            f"claim_refs {set(claim_refs)}"
        )
