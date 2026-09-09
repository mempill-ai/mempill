"""
mempill_mcp — FastMCP adapter exposing the mempill engine as MCP tools.

Public surface:
    mcp   — the FastMCP server instance (5 tools registered after import).

Usage (programmatic):
    import os
    os.environ["MEMPILL_AGENT_ID"] = "my-agent"
    from mempill_mcp import mcp

Usage (stdio transport, via script):
    $ MEMPILL_AGENT_ID=my-agent mempill-mcp
"""

from importlib.metadata import PackageNotFoundError, version as _pkg_version

from mempill_mcp.server import mcp

# Import tools module to trigger @mcp.tool() registration.
import mempill_mcp.tools  # noqa: F401, E402

try:
    __version__ = _pkg_version("mempill-mcp")
except PackageNotFoundError:  # pragma: no cover - editable/unbuilt checkout
    __version__ = "0.0.0+unknown"

__all__ = ["mcp", "__version__"]
