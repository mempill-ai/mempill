"""
mempill_mcp.server — FastMCP server with lifespan-managed engine.

Lifecycle:
  1. Reads MEMPILL_AGENT_ID from env (required; fails fast if missing).
  2. Reads MEMPILL_DB_PATH from env (optional; in-memory engine if absent).
  3. Opens the mempill Engine once in the lifespan context manager.
  4. Yields {"engine": Engine, "agent_id": str} to all tool functions.
"""

from __future__ import annotations

import os
from contextlib import asynccontextmanager
from typing import AsyncIterator

import mempill
from mempill import Engine
from mcp.server.fastmcp import FastMCP


# ── Lifespan ──────────────────────────────────────────────────────────────────

@asynccontextmanager
async def _lifespan(server: FastMCP) -> AsyncIterator[dict]:
    """Open the mempill engine once and yield context to tools."""
    agent_id = os.environ.get("MEMPILL_AGENT_ID")
    if not agent_id:
        raise RuntimeError(
            "MEMPILL_AGENT_ID environment variable is required but not set. "
            "Set it to a unique agent identifier before starting mempill-mcp."
        )

    db_path = os.environ.get("MEMPILL_DB_PATH")
    if db_path:
        engine: Engine = mempill.open(db_path)
    else:
        engine = mempill.open_in_memory()

    try:
        yield {"engine": engine, "agent_id": agent_id}
    finally:
        # Engine cleanup is handled by GC; no explicit close() needed.
        pass


# ── FastMCP instance ──────────────────────────────────────────────────────────

mcp: FastMCP = FastMCP(
    name="mempill-mcp",
    instructions=(
        "mempill memory engine adapter. "
        "Use ingest_claim to write beliefs, query_memory to read them, "
        "reconcile to resolve conflicts, and audit to inspect history."
    ),
    lifespan=_lifespan,
)
