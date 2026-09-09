"""
test_valid_time_confidence_authority.py — pins the documented relationship between
ingest_claim's two valid-time-confidence inputs.

`ingest_claim` takes a top-level `confidence_valid_time` parameter AND a nested
`valid_time["valid_time_confidence"]` field. Only the nested field is persisted and
authoritative (both storage backends keep a single `valid_time_confidence` column,
populated exclusively from `valid_time["valid_time_confidence"]`). The top-level
`confidence_valid_time` parameter is forwarded into the request but silently dropped
by storage — see README.md and the ingest_claim docstring in tools.py.

This test deliberately passes MISMATCHED values for the two parameters and asserts
that the belief returned by query_memory reflects only `valid_time_confidence`
(from the `valid_time` dict), on both the `valid_time` and `confidence` sub-objects.
"""

from __future__ import annotations

import json
from typing import Any

import pytest

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
async def test_valid_time_confidence_is_authoritative_over_confidence_valid_time(
    agent_id: str,
) -> None:
    """The nested valid_time.valid_time_confidence must win; confidence_valid_time is ignored."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client,
            "ingest_claim",
            subject="acme",
            predicate="hq_confirmed",
            value="true",
            provenance="External:UserAsserted",
            # Deliberately mismatched: the top-level param says 0.2, the
            # authoritative nested field says 0.85.
            confidence_valid_time=0.2,
            valid_time={
                "start": "2020-03-01T00:00:00Z",
                "valid_time_confidence": 0.85,
            },
        )
        result = await _call(
            client, "query_memory", subject="acme", predicate="hq_confirmed"
        )
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        primary = result.get("belief", {}).get("primary", {})
        assert isinstance(primary, dict), f"Expected primary dict, got: {primary!r}"

        vt_confidence = primary.get("valid_time", {}).get("valid_time_confidence")
        conf_confidence = primary.get("confidence", {}).get("valid_time_confidence")

        assert vt_confidence == pytest.approx(0.85, abs=1e-4), (
            f"valid_time.valid_time_confidence must reflect the nested field (0.85), "
            f"got: {vt_confidence!r}"
        )
        assert conf_confidence == pytest.approx(0.85, abs=1e-4), (
            "confidence.valid_time_confidence must also reflect the nested "
            f"valid_time.valid_time_confidence (0.85), not the ignored top-level "
            f"confidence_valid_time param (0.2); got: {conf_confidence!r}"
        )
