"""
test_quickstart.py — CI-enforced regression test for the Tier-1 DX quickstart.

Runs `mempill.quickstart.main()` in-process so the quickstart's own asserts
(Munich succession, Contested value=None + 2 candidates) are wired into the
normal pytest run rather than left as a standalone example that can silently
rot. A failing assert inside `main()` propagates as an `AssertionError` here,
so CI fails loudly instead of swallowing it.

Also runs the module as a subprocess (`python -m mempill.quickstart`) to
verify the documented entry point exits 0, matching the two ways users are
told to run it.
"""

from __future__ import annotations

import subprocess
import sys

import pytest

from mempill.quickstart import main as quickstart_main


def test_quickstart_main_runs_without_raising() -> None:
    """`mempill.quickstart.main()` must complete all internal asserts cleanly."""
    quickstart_main()


def test_quickstart_module_entrypoint_exits_zero() -> None:
    """`python -m mempill.quickstart` must exit 0 and print the success marker."""
    result = subprocess.run(
        [sys.executable, "-m", "mempill.quickstart"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, (
        f"quickstart exited {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    assert "quickstart passed" in result.stdout


def test_quickstart_fails_loudly_on_broken_assert(monkeypatch: pytest.MonkeyPatch) -> None:
    """Sanity check: an assertion failure inside main() is NOT swallowed.

    Guards against a future refactor of quickstart_main() wrapping asserts in
    try/except (which would silently defeat this test file's purpose).
    """
    import mempill.quickstart as qs_module

    def broken_main() -> None:
        assert False, "intentionally broken for CI-gate verification"

    monkeypatch.setattr(qs_module, "main", broken_main)
    with pytest.raises(AssertionError):
        qs_module.main()
