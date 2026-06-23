# mempill (Python)

Python bindings for the mempill AI-agent memory engine.

Built with PyO3 0.29 and maturin 1.14. Requires Python ≥ 3.11 and a Rust toolchain.

See the [root README](../README.md) for the full architecture, concepts, and invariants.

## Install

```sh
cd mempill-python
pip install maturin
maturin develop --release        # editable install into current venv
# or for a wheel:
maturin build --release && pip install target/wheels/*.whl
```

## Usage

```python
import mempill
from mempill import ProvenanceLabel, Disposition

# Open an engine (file-backed or in-memory).
engine = mempill.open("/path/to/agent.db")   # file-backed SQLite
engine = mempill.open_in_memory()            # ephemeral; tests / MCP sessions

# Ingest a claim.
resp = engine.ingest_claim({
    "agent_id": "my-agent",
    "subject": "user",
    "predicate": "city",
    "value": "Berlin",
    "provenance": ProvenanceLabel.external_user_asserted(),
    "cardinality": "Functional",      # "Functional" | "SetValued" | "Unknown"
    "confidence": {"value_confidence": 0.95, "valid_time_confidence": 0.0},
    "criticality": "Medium",          # "Low" | "Medium" | "High" | "Critical"
    "derived_from": [],
})
print(resp["claim_ref"], resp["disposition"])
assert resp["disposition"] == Disposition.CommittedCheap

# Query the canonical belief.
result = engine.query_memory({
    "agent_id": "my-agent",
    "subject": "user",
    "predicate": "city",
})
print(result["belief"])

# Reconcile conflicts.
result = engine.reconcile({
    "agent_id": "my-agent",
    "subject_lines": [("user", "city")],
})

# Query the audit ledger.
result = engine.query_audit({
    "agent_id": "my-agent",
    "claim_ref": None,
    "from_tx_time": None,
    "limit": 50,
})
```

## Provenance helpers

```python
from mempill import ProvenanceLabel

ProvenanceLabel.external_user_asserted()   # {"type": "External", "kind": "UserAsserted"}
ProvenanceLabel.external_first_hand()      # {"type": "External", "kind": "ExternalFirstHand"}
ProvenanceLabel.recall_re_entry()          # {"type": "RecallReEntry"}
ProvenanceLabel.model_derived()            # {"type": "ModelDerived"}
```

## Exceptions

```
MempillError (base)
  ValidationError   — invalid request fields
  NotFoundError     — claim/agent not found
  ConflictError     — write-authority violation
  StorageError      — database open/migration failure
  ConfigError       — invalid engine configuration
  InternalError     — unexpected engine failure
```

## Type stubs

`.pyi` stubs and `py.typed` marker are included. The package is mypy and stubtest clean.

## License

AGPL-3.0 with linking exception. See the [root README](../README.md) for details.
