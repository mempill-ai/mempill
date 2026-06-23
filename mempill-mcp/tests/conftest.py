"""
conftest.py — shared fixtures for mempill-mcp pytest suite.

Uses mcp.shared.memory.create_connected_server_and_client_session for
in-process FastMCP testing (no subprocess, no network, no stdio).
"""

from __future__ import annotations

import os
import pytest


@pytest.fixture()
def agent_id() -> str:
    return "mcp-test-agent-01"


@pytest.fixture(autouse=True)
def set_agent_id_env(agent_id: str, monkeypatch: pytest.MonkeyPatch) -> None:
    """Inject MEMPILL_AGENT_ID into the environment for all MCP tests."""
    monkeypatch.setenv("MEMPILL_AGENT_ID", agent_id)
    monkeypatch.delenv("MEMPILL_DB_PATH", raising=False)
