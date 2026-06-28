# mempill-mcp

MCP adapter for the mempill AI-agent memory engine.

A FastMCP server exposing 4 tools over stdio transport. Backed by the `mempill` Python wheel
(which wraps the Rust engine). Requires Python ≥ 3.11.

See the [root README](../README.md) for full architecture and concepts.

## Install

```sh
# 1. Install the mempill Python wheel first.
cd mempill-python
maturin develop --release
cd ..

# 2. Install mempill-mcp.
cd mempill-mcp
pip install .
```

## Run

```sh
export MEMPILL_AGENT_ID="my-agent"            # required
export MEMPILL_DB_PATH="/data/my-agent.db"    # optional; omit for in-memory (ephemeral)
mempill-mcp
```

Or: `python -m mempill_mcp`

The server starts on stdio transport (the default for Claude Desktop and other MCP clients).

## Environment contract

| Variable | Required | Description |
|---|---|---|
| `MEMPILL_AGENT_ID` | Yes | Unique agent identifier. The server fails fast if not set. |
| `MEMPILL_DB_PATH` | No | Path to the SQLite database file. Omit for in-memory (data lost on exit). |

The engine is opened once at startup (FastMCP lifespan) and shared across all tool calls.

## Tools

### `ingest_claim`

Write a belief claim to the engine.

Parameters:
- `subject` (str) — the entity the claim is about, e.g. `"user:alice"`
- `predicate` (str) — the property being asserted, e.g. `"location"`
- `value` (any JSON) — the claimed value
- `provenance` (str or dict) — see below
- `cardinality` (str, default `"Functional"`) — `"Functional"` | `"SetValued"` | `"Unknown"`
- `confidence_value` (float, default 0.9) — value confidence in [0, 1]
- `confidence_valid_time` (float, default 0.9) — temporal confidence in [0, 1]
- `criticality` (str, default `"Low"`) — `"Low"` | `"Medium"` | `"High"` | `"Critical"`
- `valid_time` (dict, optional) — `{"start"?: ISO-8601, "end"?: ISO-8601}`
- `derived_from` (list[str], optional) — source claim UUIDs

Returns: `{"claim_ref": str, "disposition": str, "contested_with": [str]}`
Non-committed dispositions include a `"status_reason"` field.

### `query_memory`

Read the canonical belief for a (subject, predicate) pair.

Parameters:
- `subject` (str)
- `predicate` (str)
- `as_of_tx_time` (str, optional) — ISO-8601 UTC timestamp; rewinds the transaction-time axis
- `valid_at` (str, optional) — ISO-8601 UTC timestamp; filters by real-world validity window (independent of `as_of_tx_time`)

Returns: `{"belief": {...BeliefProjection...}}`

### `reconcile`

Trigger conflict reconciliation for a set of subject lines.

Parameters:
- `subject_lines` (list[list[str]]) — list of `[subject, predicate]` pairs

Returns: `{"outcomes": [[claim_ref, disposition], ...], "oracle_escalations": int}`

### `audit`

Query the immutable ledger for claim history.

Parameters:
- `limit` (int, default 50) — max entries to return
- `claim_ref` (str, optional) — filter by specific claim UUID
- `from_tx_time` (str, optional) — ISO-8601 UTC lower bound on transaction time

Returns: `{"entries": [LedgerEntry, ...]}`

## Provenance strings

`ingest_claim` accepts provenance as a friendly string (case-insensitive, separator-tolerant)
or as a wire-shape dict:

| String | Wire dict |
|---|---|
| `"External:UserAsserted"` | `{"type": "External", "kind": "UserAsserted"}` |
| `"External:ExternalFirstHand"` | `{"type": "External", "kind": "ExternalFirstHand"}` |
| `"RecallReEntry"` | `{"type": "RecallReEntry"}` |
| `"ModelDerived"` | `{"type": "ModelDerived"}` |

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
