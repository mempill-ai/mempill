"""
test_end_fact_tool.py — In-process FastMCP client tests for the end_fact tool
(TASK-33 E2, 5th mempill-mcp tool).

Covers:
  - end_fact bounds the sole live claim and returns claim_ref/disposition/effective_at/no_op.
  - DIAG_close_incumbent.md §1 regression: open incumbent -> end_fact -> non-overlapping
    challenger folds to a clean succession via query_memory, not Contested.
  - Ambiguous line (>1 live claim) surfaces an error result, never a silent guess.
"""

from __future__ import annotations

from typing import Any

import pytest

from mcp.shared.memory import create_connected_server_and_client_session


def _get_mcp():
    from mempill_mcp.server import mcp
    return mcp


def _parse_tool_result(result: Any) -> dict | str:
    if hasattr(result, "content") and result.content:
        raw = result.content[0]
        if hasattr(raw, "text"):
            import json
            text = raw.text
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                return text
        return raw
    return result


async def _call(client, tool: str, **kwargs) -> dict | str:
    result = await client.call_tool(tool, kwargs)
    return _parse_tool_result(result)


@pytest.mark.anyio
async def test_end_fact_bounds_sole_live_claim(agent_id: str) -> None:
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        ingested = await _call(
            client, "ingest_claim",
            subject="user", predicate="city", value="Austin",
            provenance="External:UserAsserted",
            valid_time={"start": "2021-04-01T00:00:00Z", "valid_time_confidence": 0.9},
        )
        assert ingested["disposition"] == "CommittedCheap"

        result = await _call(
            client, "end_fact",
            subject="user", predicate="city", at="2024-09-23",
        )
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        assert result["disposition"] == "Superseded"
        assert result["claim_ref"] == ingested["claim_ref"]
        assert result["no_op"] is False
        assert result["effective_at"].startswith("2024-09-23")


@pytest.mark.anyio
async def test_end_fact_then_new_claim_folds_to_clean_succession(agent_id: str) -> None:
    """DIAG_close_incumbent.md §1: the exact regression assert_validity/end_fact fixes."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client, "ingest_claim",
            subject="user", predicate="city", value="Austin",
            provenance="External:UserAsserted",
            valid_time={"start": "2021-04-01T00:00:00Z", "valid_time_confidence": 0.9},
        )
        await _call(client, "end_fact", subject="user", predicate="city", at="2024-09-23")
        await _call(
            client, "ingest_claim",
            subject="user", predicate="city", value="NYC",
            provenance="External:UserAsserted",
            valid_time={"start": "2024-09-23T00:00:00Z", "valid_time_confidence": 0.9},
        )

        result = await _call(client, "query_memory", subject="user", predicate="city")
        belief = result.get("belief", {})
        assert belief.get("status") == "Resolved", (
            f"expected clean succession (Resolved), got {belief.get('status')}; full={result}"
        )
        assert belief.get("primary", {}).get("fact", {}).get("value") == "NYC"


@pytest.mark.anyio
async def test_end_fact_ambiguous_line_never_guesses(agent_id: str) -> None:
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client, "ingest_claim",
            subject="user", predicate="nickname", value="A",
            provenance="External:UserAsserted",
        )
        await _call(
            client, "ingest_claim",
            subject="user", predicate="nickname", value="B",
            provenance="External:UserAsserted",
        )

        result = await _call(
            client, "end_fact",
            subject="user", predicate="nickname", at="2024-01-01",
        )
        # An ambiguous line must never silently bound one of the two candidates —
        # the tool call must fail (isError result), not return a claim_ref.
        if isinstance(result, dict):
            assert "claim_ref" not in result or result.get("claim_ref") is None
        else:
            assert isinstance(result, str)
