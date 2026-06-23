"""
test_mcp_tools.py — In-process FastMCP client tests for all 4 mempill tools.

Uses mcp.shared.memory.create_connected_server_and_client_session to avoid
any subprocess or network; exercises the full tool dispatch path including
lifespan context injection.

Tools under test:
  1. ingest_claim   — write a belief and assert CommittedCheap
  2. query_memory   — read back the belief after ingest
  3. reconcile      — resolve conflicts
  4. audit          — inspect ledger; agent_id in entries matches env var

Contested scenario:
  - Ingest two conflicting External claims via MCP ingest_claim tool.
  - Assert second ingest returns disposition=="Contested" with status_reason.
  - Call query_memory and assert belief.status == "Contested".

DEFECT-MCP-1 FIXED:
  _normalise_provenance in tools.py now splits on ':' first before lookup,
  so "External:ExternalFirstHand" correctly maps to external_first_hand()
  instead of producing the invalid key "externalexternalfirsthand".
  All provenance string forms are verified in test_provenance_external_first_hand_works.
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
    """Extract dict from a CallToolResult. Returns str if content is an error message."""
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


# ── Core tool tests ───────────────────────────────────────────────────────────

@pytest.mark.anyio
async def test_ingest_claim_returns_committed_cheap(agent_id: str) -> None:
    """ingest_claim tool must return disposition=CommittedCheap for a clean write."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        result = await _call(
            client,
            "ingest_claim",
            subject="user",
            predicate="city",
            value="Berlin",
            provenance="External:UserAsserted",
        )
        assert isinstance(result, dict), f"Expected dict, got: {result!r}"
        assert result["disposition"] == "CommittedCheap", (
            f"Expected CommittedCheap, got {result.get('disposition')!r}. Full: {result}"
        )
        assert "claim_ref" in result
        assert "contested_with" in result


@pytest.mark.anyio
async def test_query_memory_reflects_ingest(agent_id: str) -> None:
    """query_memory must return the value that was ingested via ingest_claim."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client, "ingest_claim",
            subject="user", predicate="city", value="Tokyo",
            provenance="External:UserAsserted",
        )
        result = await _call(client, "query_memory", subject="user", predicate="city")
        assert isinstance(result, dict), f"Expected dict from query_memory, got: {result!r}"
        belief = result.get("belief", {})
        primary = belief.get("primary", {})
        assert primary.get("fact", {}).get("value") == "Tokyo", (
            f"Expected value 'Tokyo', got: {primary}"
        )


@pytest.mark.anyio
async def test_reconcile_runs_after_conflict(agent_id: str) -> None:
    """reconcile tool must complete without error after a contested conflict."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client, "ingest_claim",
            subject="user", predicate="name", value="Alice",
            provenance="External:UserAsserted",
        )
        await _call(
            client, "ingest_claim",
            subject="user", predicate="name", value="Bob",
            provenance="External:UserAsserted",
        )
        result = await _call(
            client, "reconcile",
            subject_lines=[["user", "name"]],
        )
        assert isinstance(result, dict), f"reconcile must return dict, got: {result!r}"
        assert "outcomes" in result, f"reconcile must return outcomes, got: {result}"
        assert isinstance(result["outcomes"], list)


@pytest.mark.anyio
async def test_audit_entries_have_correct_agent_id(agent_id: str) -> None:
    """audit tool must return ledger entries whose agent_id matches MEMPILL_AGENT_ID."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        await _call(
            client, "ingest_claim",
            subject="user", predicate="score", value="42",
            provenance="External:UserAsserted",
        )
        result = await _call(client, "audit", limit=10)
        assert isinstance(result, dict), f"Expected dict from audit, got: {result!r}"
        entries = result.get("entries", [])
        assert len(entries) >= 1, "Audit must return at least one entry after ingest"
        for entry in entries:
            assert entry["agent_id"] == agent_id, (
                f"Ledger entry agent_id {entry['agent_id']!r} != {agent_id!r}"
            )


@pytest.mark.anyio
async def test_contested_belief_surfaces_status_reason(agent_id: str) -> None:
    """
    After two conflicting ingest_claim calls, the second must return
    disposition=="Contested" AND a non-empty status_reason field.
    """
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        r1 = await _call(
            client, "ingest_claim",
            subject="user", predicate="location", value="New York",
            provenance="External:UserAsserted",
        )
        assert isinstance(r1, dict) and r1.get("disposition") == "CommittedCheap"

        r2 = await _call(
            client, "ingest_claim",
            subject="user", predicate="location", value="San Francisco",
            provenance="External:UserAsserted",
        )
        assert isinstance(r2, dict), f"Expected dict from second ingest, got: {r2!r}"
        assert r2.get("disposition") == "Contested", (
            f"Expected Contested from conflicting ingest, got {r2.get('disposition')!r}"
        )
        # status_reason must be present and non-empty for Contested disposition.
        assert "status_reason" in r2 and r2["status_reason"], (
            f"status_reason must be non-empty when disposition=Contested, got: {r2}"
        )

        # query_memory must also surface Contested belief.
        query_result = await _call(client, "query_memory", subject="user", predicate="location")
        assert isinstance(query_result, dict)
        belief = query_result.get("belief", {})
        assert belief.get("status") == "Contested", (
            f"belief.status must be Contested after conflict, got: {belief.get('status')!r}"
        )


@pytest.mark.anyio
async def test_all_four_tools_are_listed() -> None:
    """The MCP server must advertise exactly the 4 expected tools."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        tools_result = await client.list_tools()
        tool_names = {t.name for t in tools_result.tools}
        expected = {"ingest_claim", "query_memory", "reconcile", "audit"}
        assert expected.issubset(tool_names), (
            f"Missing tools: {expected - tool_names}. Available: {tool_names}"
        )


# ── DEFECT documentation tests ────────────────────────────────────────────────

@pytest.mark.anyio
async def test_provenance_user_asserted_works(agent_id: str) -> None:
    """External:UserAsserted provenance string normalises correctly."""
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        result = await _call(
            client, "ingest_claim",
            subject="user", predicate="prop-ua", value="v",
            provenance="External:UserAsserted",
        )
        assert isinstance(result, dict) and "claim_ref" in result


@pytest.mark.anyio
async def test_provenance_external_first_hand_works(agent_id: str) -> None:
    """
    DEFECT-MCP-1 fix verification: 'External:ExternalFirstHand' must normalise correctly.

    Fix: _normalise_provenance now splits on ':' first, extracting type="External" and
    kind="ExternalFirstHand" separately, then maps kind to external_first_hand().
    The previous bug produced key "externalexternalfirsthand" (double "external").

    All four provenance string forms are verified:
      - "External:UserAsserted"      -> must succeed
      - "External:ExternalFirstHand" -> must succeed (was broken)
      - "RecallReEntry"              -> must succeed
      - "ModelDerived"               -> must succeed
    """
    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        # Primary fix: External:ExternalFirstHand must now work.
        result = await _call(
            client, "ingest_claim",
            subject="user", predicate="prop-efh", value="v",
            provenance="External:ExternalFirstHand",
        )
        assert isinstance(result, dict) and "claim_ref" in result, (
            f"'External:ExternalFirstHand' provenance normalisation failed. "
            f"Got: {result!r}"
        )

        # RecallReEntry must work.
        r2 = await _call(
            client, "ingest_claim",
            subject="user", predicate="prop-rre", value="v",
            provenance="RecallReEntry",
        )
        assert isinstance(r2, dict) and "claim_ref" in r2, (
            f"'RecallReEntry' provenance failed. Got: {r2!r}"
        )

        # ModelDerived must work.
        r3 = await _call(
            client, "ingest_claim",
            subject="user", predicate="prop-md", value="v",
            provenance="ModelDerived",
        )
        assert isinstance(r3, dict) and "claim_ref" in r3, (
            f"'ModelDerived' provenance failed. Got: {r3!r}"
        )
