"""
mempill_mcp.__main__ — Entry point for `mempill-mcp` CLI and `python -m mempill_mcp`.

Runs the MCP server over stdio transport (default for Claude Desktop / MCP clients).
Requires MEMPILL_AGENT_ID environment variable to be set before invocation.
"""

from mempill_mcp import mcp


def main() -> None:
    """Start the mempill-mcp server on stdio transport."""
    mcp.run()


if __name__ == "__main__":
    main()
