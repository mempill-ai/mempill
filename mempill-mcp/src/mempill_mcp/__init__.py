"""
mempill_mcp — FastMCP adapter exposing the mempill engine as MCP tools.

Public surface:
    mcp   — the FastMCP server instance (4 tools registered after import).

Usage (programmatic):
    import os
    os.environ["MEMPILL_AGENT_ID"] = "my-agent"
    from mempill_mcp import mcp

Usage (stdio transport, via script):
    $ MEMPILL_AGENT_ID=my-agent mempill-mcp
"""

from mempill_mcp.server import mcp

# Import tools module to trigger @mcp.tool() registration.
import mempill_mcp.tools  # noqa: F401, E402

__all__ = ["mcp"]
