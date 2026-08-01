"""
test_mcp_file_backed.py — Verifies MEMPILL_DB_DIR file-backed mode (gap 🟡6).

The lifespan (mempill_mcp.server._lifespan) reads MEMPILL_DB_DIR / MEMPILL_AGENT_ID
from the environment EACH time it is entered (no caching at import time), so a fresh
`create_connected_server_and_client_session(mcp_server)` context re-triggers engine
construction with whatever env is currently set. This lets a single test simulate a
"server restart" by exiting one session context and entering a second one with the
same env — a brand new Engine object is opened against the same on-disk file.

Tests:
  1. test_file_backed_startup_creates_agent_db_file — server starts with MEMPILL_DB_DIR
     set; the per-agent file agent_<id>.db appears under the dir.
  2. test_file_backed_ingest_then_query_round_trips — ingest via MCP tool, query via
     MCP tool in the same session; value round-trips.
  3. test_file_backed_restart_persists_data — ingest in session 1, tear the session
     down (server/engine GC'd), open a brand new session (fresh Engine, same env);
     the previously-ingested fact is still queryable.
  4. test_file_backed_invalid_agent_id_raises_on_startup — an agent_id containing
     characters outside [A-Za-z0-9_-] must fail fast at server startup when
     MEMPILL_DB_DIR is set (StorageError from mempill.open_for_agent), matching the
     existing test_mcp_agent_id.py fail-fast conventions.

Environment isolation:
  conftest.py's autouse `set_agent_id_env` fixture unconditionally clears
  MEMPILL_DB_DIR for every test in the suite. This module overrides MEMPILL_DB_DIR
  (and, where needed, MEMPILL_AGENT_ID) via monkeypatch.setenv/monkeypatch.delenv
  *inside each test function*, AFTER the autouse fixture has already run, so the
  override applies only within this module's own tests and never leaks into other
  test modules (monkeypatch changes are undone automatically at test teardown).
"""

from __future__ import annotations

import json
import os
from pathlib import Path
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


@pytest.mark.anyio
async def test_file_backed_startup_creates_agent_db_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Server startup with MEMPILL_DB_DIR set must create agent_<id>.db under the dir."""
    monkeypatch.setenv("MEMPILL_AGENT_ID", "file-backed-agent-01")
    monkeypatch.setenv("MEMPILL_DB_DIR", str(tmp_path))

    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        tools_result = await client.list_tools()
        assert len(tools_result.tools) > 0, "Server must start and expose tools"

    db_file = tmp_path / "agent_file-backed-agent-01.db"
    assert db_file.exists(), (
        f"Expected per-agent db file {db_file} to be created under MEMPILL_DB_DIR; "
        f"dir contents: {list(tmp_path.iterdir())}"
    )


@pytest.mark.anyio
async def test_file_backed_ingest_then_query_round_trips(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Ingest via MCP ingest_claim, query via MCP query_memory: value round-trips."""
    monkeypatch.setenv("MEMPILL_AGENT_ID", "file-backed-agent-02")
    monkeypatch.setenv("MEMPILL_DB_DIR", str(tmp_path))

    mcp_server = _get_mcp()
    async with create_connected_server_and_client_session(mcp_server) as client:
        ingest_result = await _call(
            client, "ingest_claim",
            subject="user", predicate="city", value="Lisbon",
            provenance="External:UserAsserted",
        )
        assert isinstance(ingest_result, dict) and ingest_result.get("disposition") == "CommittedCheap"

        query_result = await _call(client, "query_memory", subject="user", predicate="city")
        assert isinstance(query_result, dict)
        primary = query_result.get("belief", {}).get("primary", {})
        assert primary.get("fact", {}).get("value") == "Lisbon", (
            f"Expected 'Lisbon' round-trip via file-backed engine, got: {primary}"
        )


@pytest.mark.anyio
async def test_file_backed_restart_persists_data(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Data ingested before a simulated restart must be queryable after it.

    Session 1 ingests a fact and exits (engine/server torn down). Session 2 opens a
    brand-new server/client session against the SAME MEMPILL_DB_DIR + MEMPILL_AGENT_ID
    (lifespan re-reads env and re-opens the engine each time it's entered) — this
    exercises real on-disk persistence, not in-process object reuse.
    """
    monkeypatch.setenv("MEMPILL_AGENT_ID", "file-backed-agent-03")
    monkeypatch.setenv("MEMPILL_DB_DIR", str(tmp_path))

    mcp_server = _get_mcp()

    # Session 1: ingest, then tear down.
    async with create_connected_server_and_client_session(mcp_server) as client:
        ingest_result = await _call(
            client, "ingest_claim",
            subject="user", predicate="dietary", value="vegan",
            provenance="External:UserAsserted",
        )
        assert isinstance(ingest_result, dict) and ingest_result.get("disposition") == "CommittedCheap"

    # Session 2: fresh server/engine, same env — must see the persisted fact.
    async with create_connected_server_and_client_session(mcp_server) as client:
        query_result = await _call(client, "query_memory", subject="user", predicate="dietary")
        assert isinstance(query_result, dict)
        primary = query_result.get("belief", {}).get("primary", {})
        assert primary.get("fact", {}).get("value") == "vegan", (
            f"Expected 'vegan' to survive a simulated restart via on-disk persistence, "
            f"got: {primary}"
        )


@pytest.mark.anyio
async def test_file_backed_invalid_agent_id_raises_on_startup(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An agent_id with characters outside [A-Za-z0-9_-] must fail fast at startup
    when MEMPILL_DB_DIR is set (mempill.open_for_agent raises StorageError), mirroring
    the fail-fast conventions in test_mcp_agent_id.py.
    """
    monkeypatch.setenv("MEMPILL_AGENT_ID", "bad/agent id!")
    monkeypatch.setenv("MEMPILL_DB_DIR", str(tmp_path))

    mcp_server = _get_mcp()

    raised = False
    try:
        async with create_connected_server_and_client_session(
            mcp_server,
            raise_exceptions=True,
        ) as client:
            try:
                await client.list_tools()
            except Exception:
                pass
    except Exception:
        raised = True

    assert raised, (
        "Server startup must raise a clean error when MEMPILL_DB_DIR is set and "
        "MEMPILL_AGENT_ID contains filesystem-unsafe characters — the fail-fast "
        "contract must extend to file-backed mode."
    )
