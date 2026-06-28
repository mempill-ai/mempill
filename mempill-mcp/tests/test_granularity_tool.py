"""
test_granularity_tool.py — MCP query_memory tool granularity + display-string tests.

Verifies that when a claim is ingested via the MCP ingest_claim tool with
explicit valid-time granularity, the query_memory tool result includes:

  1. ``valid_time.start_granularity`` — raw granularity tag preserved in the
     response dict (``"month"``, ``"year"``, ``"day"``, or ``"instant"``).
  2. ``valid_from_display`` — pre-rendered, honest start display string on the
     primary belief slot.  For Month-granularity this MUST be ``"YYYY-MM"`` with
     no fabricated day component.
  3. ``valid_until_display`` — same for the end endpoint (absent when open-ended).
"""

from __future__ import annotations

import json
from typing import Any

import pytest
import anyio

from mcp.shared.memory import create_connected_server_and_client_session


def _get_mcp():
    """Return the mempill FastMCP server instance."""
    from mempill_mcp.server import mcp
    return mcp


def _parse_tool_result(result: Any) -> dict | str:
    """Extract dict from a CallToolResult."""
    if hasattr(result, "content") and result.content:
        raw = result.content[0]
        if hasattr(raw, "text"):
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


# ── MCP granularity tests ─────────────────────────────────────────────────────

@pytest.mark.anyio
async def test_query_memory_tool_returns_start_granularity(agent_id: str) -> None:
    """query_memory tool result must include valid_time.start_granularity when set."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="acme",
            predicate="founded",
            value="1990",
            provenance="External:UserAsserted",
            valid_time={
                "start": "2020-03-01T00:00:00Z",
                "valid_time_confidence": 0.9,
                "start_granularity": "month",
            },
        )
        result = await _call(client, "query_memory", subject="acme", predicate="founded")
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        primary = result.get("belief", {}).get("primary", {})
        assert isinstance(primary, dict), f"Expected primary dict, got: {primary!r}"
        granularity = primary.get("valid_time", {}).get("start_granularity")
        assert granularity == "month", (
            f"start_granularity must be 'month' in MCP result, got: {granularity!r}"
        )


@pytest.mark.anyio
async def test_query_memory_tool_returns_valid_from_display_month(agent_id: str) -> None:
    """query_memory tool must return valid_from_display='2020-03' for Month granularity.

    This is the honest-display invariant: Month-precision dates must render as
    'YYYY-MM' — never 'YYYY-MM-DD' (no fabricated day component).
    """
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="ceo",
            predicate="tenure_start",
            value="Alice",
            provenance="External:UserAsserted",
            valid_time={
                "start": "2020-03-01T00:00:00Z",
                "valid_time_confidence": 0.9,
                "start_granularity": "month",
            },
        )
        result = await _call(client, "query_memory", subject="ceo", predicate="tenure_start")
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        primary = result.get("belief", {}).get("primary", {})
        display = primary.get("valid_from_display")
        assert display == "2020-03", (
            f"Month granularity must render as '2020-03' in MCP result, got: {display!r}. "
            "This proves no fabricated day component."
        )
        # Hard invariant: only one dash in the display string.
        assert display is not None and display.count("-") == 1, (
            f"Month display must have exactly one dash (no day); got: {display!r}"
        )


@pytest.mark.anyio
async def test_query_memory_tool_valid_until_display_year(agent_id: str) -> None:
    """query_memory tool must return valid_until_display='2023' for Year-granularity end."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="project",
            predicate="active",
            value="true",
            provenance="External:UserAsserted",
            valid_time={
                "start": "2020-01-01T00:00:00Z",
                "start_granularity": "year",
                "end": "2023-01-01T00:00:00Z",
                "end_granularity": "year",
                "valid_time_confidence": 0.9,
            },
        )
        result = await _call(client, "query_memory", subject="project", predicate="active")
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        primary = result.get("belief", {}).get("primary", {})
        from_display = primary.get("valid_from_display")
        until_display = primary.get("valid_until_display")
        assert from_display == "2020", (
            f"Year-granularity start must render as '2020', got: {from_display!r}"
        )
        assert until_display == "2023", (
            f"Year-granularity end must render as '2023', got: {until_display!r}"
        )


@pytest.mark.anyio
async def test_query_memory_tool_open_end_no_until_display(agent_id: str) -> None:
    """query_memory tool: open-ended claims must NOT have valid_until_display."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="person",
            predicate="role",
            value="Engineer",
            provenance="External:UserAsserted",
            valid_time={
                "start": "2022-06-01T00:00:00Z",
                "start_granularity": "month",
                "valid_time_confidence": 0.9,
                # No end date.
            },
        )
        result = await _call(client, "query_memory", subject="person", predicate="role")
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        primary = result.get("belief", {}).get("primary", {})
        assert "valid_until_display" not in primary, (
            "Open-ended claim must NOT have valid_until_display in MCP result, "
            f"but got: {primary.get('valid_until_display')!r}"
        )
