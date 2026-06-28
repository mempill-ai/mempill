"""
test_valid_at_tool.py — Verifies that the query_memory MCP tool accepts and
forwards the valid_at parameter to the engine.

Tests:
  1. query_memory with valid_at accepted (no error, returns a belief).
  2. query_memory with valid_at + as_of_tx_time both set (D2 independence).
  3. query_memory without valid_at still works (backward-compatible).
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


@pytest.mark.anyio
async def test_query_memory_valid_at_accepted(agent_id: str) -> None:
    """query_memory tool must accept valid_at without error and return a belief."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        # First ingest a claim with explicit valid-time bounds.
        await _call(
            client,
            "ingest_claim",
            subject="acme",
            predicate="ceo",
            value="Alice",
            provenance="External:UserAsserted",
            valid_time={
                "start": "2020-01-01T00:00:00Z",
                "valid_time_confidence": 1.0,
            },
        )
        # Now query with valid_at — must not raise and must return a dict.
        result = await _call(
            client,
            "query_memory",
            subject="acme",
            predicate="ceo",
            valid_at="2021-06-01T00:00:00Z",
        )
        assert isinstance(result, dict), (
            f"query_memory with valid_at must return dict, got: {result!r}"
        )
        assert "belief" in result, (
            f"Response must contain 'belief' key, got: {result}"
        )


@pytest.mark.anyio
async def test_query_memory_valid_at_and_as_of_tx_time_compose(agent_id: str) -> None:
    """query_memory must accept both valid_at and as_of_tx_time together (D2 independence)."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="acme",
            predicate="founder",
            value="Carol",
            provenance="External:UserAsserted",
            valid_time={
                "start": "2015-01-01T00:00:00Z",
                "valid_time_confidence": 1.0,
            },
        )
        result = await _call(
            client,
            "query_memory",
            subject="acme",
            predicate="founder",
            as_of_tx_time="2099-01-01T00:00:00Z",
            valid_at="2018-06-01T00:00:00Z",
        )
        assert isinstance(result, dict), (
            f"Bi-temporal query must return dict, got: {result!r}"
        )
        assert "belief" in result, (
            f"Bi-temporal query must contain 'belief' key, got: {result}"
        )


@pytest.mark.anyio
async def test_query_memory_without_valid_at_still_works(agent_id: str) -> None:
    """Omitting valid_at must produce the same result as before (backward-compatible)."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="user",
            predicate="email",
            value="user@example.com",
            provenance="External:UserAsserted",
        )
        result = await _call(
            client,
            "query_memory",
            subject="user",
            predicate="email",
            # valid_at intentionally omitted
        )
        assert isinstance(result, dict)
        primary = result.get("belief", {}).get("primary", {})
        assert primary.get("fact", {}).get("value") == "user@example.com", (
            f"Without valid_at, live belief must still be returned. Got: {result}"
        )


@pytest.mark.anyio
async def test_query_memory_valid_at_tool_schema_includes_param(agent_id: str) -> None:
    """The query_memory tool schema must advertise valid_at as a parameter."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        tools_result = await client.list_tools()
        qm_tool = next(
            (t for t in tools_result.tools if t.name == "query_memory"),
            None,
        )
        assert qm_tool is not None, "query_memory tool must be registered"
        # The input schema properties must include valid_at.
        schema = qm_tool.inputSchema if hasattr(qm_tool, "inputSchema") else {}
        properties = schema.get("properties", {}) if isinstance(schema, dict) else {}
        assert "valid_at" in properties, (
            f"query_memory tool schema must include 'valid_at' property. "
            f"Got properties: {list(properties.keys())}"
        )
