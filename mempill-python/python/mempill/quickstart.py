"""
mempill five-minute quickstart.

Run with:
    python -m mempill.quickstart
    uv run python -m mempill.quickstart

Demonstrates the Tier-1 ergonomic API (remember / recall) in three steps:
  1. Remember two facts about the same subject (city, with non-overlapping time windows).
  2. Recall — the open-ended Munich fact wins (most recent valid period).
  3. Contested — two timeless Functional facts on the same subject-line, showing
     that value=None + is_contested()=True + candidates populated is the correct
     read, NOT a NoBelief misread.

Zero imports of internal types (ProvenanceLabel, Disposition, ConfidenceDict, etc.).
"""

from mempill import open_in_memory, remember, recall, RememberOptions


def main() -> None:
    engine = open_in_memory()
    agent = "my-agent"

    print("=== mempill quickstart ===")
    print()

    # ── Step 1: remember two facts ────────────────────────────────────────────
    # Berlin is valid until end of 2024; Munich takes over from 2025.
    # Using non-overlapping windows means the engine can resolve a winner.
    print("Step 1: remember two city facts (non-overlapping time windows)")
    receipt_berlin = remember(
        engine, agent, "user", "city", "Berlin",
        RememberOptions(valid_from="2020-01-01", valid_until="2024-12-31"),
    )
    print(f"  Berlin → disposition={receipt_berlin.disposition}  ref={receipt_berlin.claim_ref[:8]}...")

    receipt_munich = remember(
        engine, agent, "user", "city", "Munich",
        RememberOptions(valid_from="2025-01-01"),
    )
    print(f"  Munich → disposition={receipt_munich.disposition}  ref={receipt_munich.claim_ref[:8]}...")
    print()

    # ── Step 2: recall — Munich wins (open-ended from 2025) ──────────────────
    print("Step 2: recall user.city")
    result = recall(engine, agent, "user", "city")
    print(f"  status={result.status}  value={result.as_str()!r}")
    assert result.as_str() == "Munich", f"Expected 'Munich', got {result.as_str()!r}"
    assert not result.is_contested(), "city should not be contested after temporal resolution"
    print("  [ok] Munich wins — open-ended from 2025-01-01 is the live belief")
    print()

    # ── Step 3: Contested — two timeless Functional facts ────────────────────
    print("Step 3: contested belief (two timeless CEO claims)")
    remember(engine, agent, "acme", "ceo", "Alice")
    remember(engine, agent, "acme", "ceo", "Bob")
    ceo = recall(engine, agent, "acme", "ceo")
    print(f"  status={ceo.status}  value={ceo.value!r}  candidates={len(ceo.candidates)}")
    assert ceo.is_contested(), f"Expected Contested, got status={ceo.status!r}"
    assert ceo.value is None, "Contested value must be None — NOT misread as NoBelief"
    assert len(ceo.candidates) == 2, f"Expected 2 candidates, got {len(ceo.candidates)}"
    print("  [ok] is_contested()=True, value=None, candidates=[Alice, Bob]")
    for c in ceo.candidates:
        print(f"    candidate: {c.value!r}  ref={c.claim_ref[:8]}...")
    print()

    # ── Step 4: resolve by reconcile, then recall clean ───────────────────────
    print("Step 4: reconcile acme.ceo → one winner")
    recon = engine.reconcile({
        "agent_id": agent,
        "subject_lines": [("acme", "ceo")],
    })
    print(f"  reconcile outcomes: {recon.get('outcomes', [])}")
    resolved = recall(engine, agent, "acme", "ceo")
    print(f"  post-reconcile status={resolved.status}  value={resolved.as_str()!r}")
    print()

    print("=== quickstart passed ===")


if __name__ == "__main__":
    main()
