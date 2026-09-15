# mempill-mcp

MCP adapter for the mempill AI-agent memory engine.

A FastMCP server exposing 5 tools over stdio transport. Backed by the `mempill` Python wheel
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
export MEMPILL_AGENT_ID="my-agent"    # required
export MEMPILL_DB_DIR="/data"         # optional; omit for in-memory (ephemeral)
mempill-mcp
```

Or: `python -m mempill_mcp`

The server starts on stdio transport (the default for Claude Desktop and other MCP clients).

## Environment contract

| Variable | Required | Description |
|---|---|---|
| `MEMPILL_AGENT_ID` | Yes | Unique agent identifier. The server fails fast if not set. |
| `MEMPILL_DB_DIR` | No | Base directory for SQLite storage. The database file is derived automatically as `MEMPILL_DB_DIR/agent_{MEMPILL_AGENT_ID}.db`. Omit for in-memory (data lost on exit). |

> **Breaking change (0.4.0):** `MEMPILL_DB_PATH` (a full file path) was replaced by
> `MEMPILL_DB_DIR` (a base directory), because storage is now opened via
> `mempill.open_for_agent(base_dir, agent_id)` — the file path is always derived from
> `agent_id`, so it is no longer possible to point two different agents at the same file.
> Existing pre-0.4.0 `MEMPILL_DB_PATH` databases are not auto-migrated; point
> `MEMPILL_DB_DIR` at a fresh directory, or migrate the old file manually (see the
> `mempill-sqlite` CHANGELOG).

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
- `confidence_valid_time` (float, default 0.9) — **not persisted; currently a no-op.**
  The mempill storage layer (SQLite and Postgres both) keeps only a single
  `valid_time_confidence` column per claim, and that column is populated exclusively
  from `valid_time["valid_time_confidence"]` (see below). This parameter's value is
  forwarded into the ingest request but is silently dropped by both storage backends —
  it is never written, never read back, and has no effect on gating, succession, or the
  returned belief. **Do not rely on this parameter for anything; it will be removed or
  wired up in a future release.** The single source of truth for valid-time confidence
  is `valid_time["valid_time_confidence"]`.
- `criticality` (str, default `"Low"`) — `"Low"` | `"Medium"` | `"High"` | `"Critical"`
- `valid_time` (dict, optional) — `{"start"?: ISO-8601, "end"?: ISO-8601, "valid_time_confidence": float, "start_granularity"?: str, "end_granularity"?: str}`.
  `start` / `end` are optional (omit for unknown/open-ended). `valid_time_confidence`
  (inside this dict) is the **authoritative** temporal-confidence value: it is the one
  persisted to storage, the one every downstream engine decision reads (incoherence
  gating, succession detection, valid-time ordering — see `mempill-core`'s `gate.rs` /
  `truth_engine.rs` / `valid_time_helpers.rs`), and the one echoed back in both
  `belief.valid_time.valid_time_confidence` and `belief.confidence.valid_time_confidence`
  on `query_memory`. It is **required whenever the `valid_time` dict is supplied at all**
  (no default — omit the whole `valid_time` dict, not just this key, if you have no
  temporal confidence to give; omitting the dict entirely defaults the stored confidence
  to `0.0`, i.e. "unknown").
  `start_granularity` / `end_granularity` are optional display-only precision hints — one of
  `"year"`, `"month"`, `"day"`, `"instant"` — recording how precisely `start` / `end` were
  known (e.g. `"year"` for a bare `"2024"` normalised to a full timestamp); omit for full
  ISO-8601 instants or when precision is unknown. They are never used for matching or
  ordering, only for honest display on read (see `query_memory` below).
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

Returns: `{"belief": {...BeliefProjection...}}`. Each belief slot (`belief.primary`,
`belief.alternatives[i]`) also carries honest-display precision metadata:
`valid_from_display` / `valid_until_display` (pre-rendered strings at the recorded
precision, e.g. `"2020-03"` for Month, absent when the endpoint is unknown/open) and the
raw `valid_time.start_granularity` / `valid_time.end_granularity` tags.

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

### `end_fact`

End an open-ended fact: explicitly close the incumbent claim on a (subject, predicate)
line as of a given instant (SDK_CONTRACT.md §3.1 `assert_validity`). This is the correct
way to say "X stopped being true at time T" — it bounds the incumbent claim in place (the
original row is never touched or duplicated), so a later non-overlapping claim on the
same line folds to a clean succession with no conflict and no adjudication needed.

Resolution never guesses which claim to close: zero live claims raises `NotFoundError`,
exactly one live claim is bounded, and more than one live claim (a genuinely contested or
set-valued line) raises `ValidationError` — inspect the line via `ingest_claim` /
`query_memory` and resolve the ambiguity before retrying.

Parameters:
- `subject` (str)
- `predicate` (str)
- `at` (str) — ISO-8601 date/time the fact stopped being true. Accepts `YYYY`, `YYYY-MM`,
  `YYYY-MM-DD`, or full RFC3339.
- `provenance` (str or dict, optional) — same forms as `ingest_claim` (see below). Must
  be first-hand external evidence — only the host acting as its own oracle may close or
  reopen a fact. Defaults to `External:UserAsserted`.
- `confidence` (float, default 1.0) — confidence in this validity assertion, in [0, 1]

Returns: `{"claim_ref": str, "disposition": str, "effective_at": str, "no_op": bool}`.
`no_op` is `true` only when this call repeated an identical bound already in effect — no
new write was made. Repeating `end_fact` on an already-fully-closed line (nothing left
live) raises `NotFoundError`, not a no-op — there is no live claim left to close.

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
