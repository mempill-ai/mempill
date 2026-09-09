"""
test_version.py — Verifies F-03: `mempill_mcp.__version__` is exposed and non-empty.
"""

from __future__ import annotations

import mempill_mcp


def test_version_is_exposed_and_nonempty() -> None:
    assert hasattr(mempill_mcp, "__version__")
    assert isinstance(mempill_mcp.__version__, str)
    assert mempill_mcp.__version__ != ""


def test_version_is_in_all() -> None:
    assert "__version__" in mempill_mcp.__all__
