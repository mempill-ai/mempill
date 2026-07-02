"""
mempill five-minute quickstart.

Run with:
    python -m mempill.quickstart
    uv run python -m mempill.quickstart

Demonstrates the Tier-1 ergonomic API (remember / recall):
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
    remember(engine, agent, "user", "city", "Berlin",
        RememberOptions(valid_from="2020-01-01", valid_until="2025-01-01"))
    remember(engine, agent, "user", "city", "Munich",
        RememberOptions(valid_from="2025-01-01"))
    result = recall(engine, agent, "user", "city")
    assert result.as_str() == "Munich"
    assert not result.is_contested()
    remember(engine, agent, "acme", "ceo", "Alice")
    remember(engine, agent, "acme", "ceo", "Bob")
    ceo = recall(engine, agent, "acme", "ceo")
    assert ceo.is_contested()
    assert len(ceo.candidates) == 2
    assert ceo.value is None
    print("quickstart passed")


if __name__ == "__main__":
    main()
