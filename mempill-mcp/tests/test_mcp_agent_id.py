"""
test_mcp_agent_id.py — Validates the MEMPILL_AGENT_ID fail-fast contract (A33).

The MCP server lifespan raises RuntimeError immediately if MEMPILL_AGENT_ID
is not set. This test verifies that the error surfaces during server startup,
not silently at first tool call.
"""

from __future__ import annotations

import os
import pytest
import anyio

from mcp.shared.memory import create_connected_server_and_client_session


@pytest.mark.anyio
async def test_missing_agent_id_raises_on_startup(monkeypatch: pytest.MonkeyPatch) -> None:
    """
    When MEMPILL_AGENT_ID is not set, the server lifespan must fail fast
    with a clear error — not silently yield a broken context.
    """
    monkeypatch.delenv("MEMPILL_AGENT_ID", raising=False)
    monkeypatch.delenv("MEMPILL_DB_PATH", raising=False)

    # Re-import to pick up the cleared env (server module is already imported
    # but the lifespan reads os.environ at call time, so no reload needed).
    from mempill_mcp.server import mcp as mcp_server

    raised = False
    error_message = ""
    try:
        async with create_connected_server_and_client_session(
            mcp_server,
            raise_exceptions=True,
        ) as client:
            # If we reach here, the lifespan did NOT fail fast — that's a bug.
            # Attempt any tool call to force the issue.
            try:
                await client.list_tools()
            except Exception:
                pass
    except Exception as exc:
        raised = True
        error_message = str(exc)

    assert raised, (
        "Server startup must raise an exception when MEMPILL_AGENT_ID is missing — "
        "the fail-fast contract (A33) was violated."
    )


@pytest.mark.anyio
async def test_empty_agent_id_raises_on_startup(monkeypatch: pytest.MonkeyPatch) -> None:
    """An empty string MEMPILL_AGENT_ID must also be rejected."""
    monkeypatch.setenv("MEMPILL_AGENT_ID", "")
    monkeypatch.delenv("MEMPILL_DB_PATH", raising=False)

    from mempill_mcp.server import mcp as mcp_server

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
        "Server startup must raise when MEMPILL_AGENT_ID is empty — "
        "the fail-fast contract (A33) was violated."
    )


@pytest.mark.anyio
async def test_valid_agent_id_starts_successfully(monkeypatch: pytest.MonkeyPatch) -> None:
    """Positive case: a non-empty MEMPILL_AGENT_ID must allow normal startup."""
    monkeypatch.setenv("MEMPILL_AGENT_ID", "valid-agent-for-startup-test")
    monkeypatch.delenv("MEMPILL_DB_PATH", raising=False)

    from mempill_mcp.server import mcp as mcp_server

    async with create_connected_server_and_client_session(mcp_server) as client:
        tools_result = await client.list_tools()
        assert len(tools_result.tools) > 0, "Server must expose tools after successful startup"
