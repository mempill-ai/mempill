"""
test_version.py — Verifies F-03: `mempill.__version__` is exposed and non-empty.
"""

from __future__ import annotations

import mempill


def test_version_is_exposed_and_nonempty() -> None:
    assert hasattr(mempill, "__version__")
    assert isinstance(mempill.__version__, str)
    assert mempill.__version__ != ""


def test_version_is_in_all() -> None:
    assert "__version__" in mempill.__all__
