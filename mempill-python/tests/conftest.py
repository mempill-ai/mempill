"""
conftest.py — shared fixtures for mempill-python pytest suite.
"""

from __future__ import annotations

import pytest
import mempill


@pytest.fixture()
def engine() -> mempill.Engine:
    """Fresh in-memory engine per test — no shared state."""
    return mempill.open_in_memory()


@pytest.fixture()
def agent_id() -> str:
    return "test-agent-v02"


@pytest.fixture()
def base_ingest(agent_id: str) -> dict:
    """Minimal valid IngestClaimRequest skeleton; override per test."""
    from mempill.types import ProvenanceLabel

    return {
        "agent_id": agent_id,
        "subject": "user",
        "predicate": "city",
        "value": "Berlin",
        "provenance": ProvenanceLabel.external_user_asserted(),
        "cardinality": "Functional",
        "valid_time": None,
        "confidence": {"value_confidence": 0.95, "valid_time_confidence": 0.0},
        "criticality": "Medium",
        "derived_from": [],
    }
