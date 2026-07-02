"""
mempill_mcp.server — FastMCP server with lifespan-managed engine.

Lifecycle:
  1. Reads MEMPILL_AGENT_ID from env (required; fails fast if missing).
  2. Reads MEMPILL_DB_DIR from env (optional; in-memory engine if absent).
  3. Opens the mempill Engine once in the lifespan context manager.
  4. Yields {"engine": Engine, "agent_id": str} to all tool functions.

BREAKING CHANGE (0.4.0): MEMPILL_DB_PATH (a full file path) has been replaced by
MEMPILL_DB_DIR (a base directory). SQLite storage is now opened via
mempill.open_for_agent(base_dir, agent_id), which derives the database file
automatically as base_dir/agent_{agent_id}.db — this removes the possibility of two
different MEMPILL_AGENT_ID values accidentally sharing one on-disk file. If you were
previously pointing MEMPILL_DB_PATH at an existing pre-0.4.0 shared-file database, that
file is NOT auto-migrated: set MEMPILL_DB_DIR to a fresh directory and, if you need the
old data, migrate it manually (see mempill-sqlite CHANGELOG for the manual migration
note).
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

    db_dir = os.environ.get("MEMPILL_DB_DIR")
    if db_dir:
        engine: Engine = mempill.open_for_agent(db_dir, agent_id)
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
