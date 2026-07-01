# mempill

**Temporally-correct AI-agent memory — append-only, bi-temporal, provenance-aware.**

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)
[![crates.io](https://img.shields.io/crates/v/mempill.svg)](https://crates.io/crates/mempill)
[![docs.rs](https://img.shields.io/docsrs/mempill)](https://docs.rs/mempill)
[![downloads](https://img.shields.io/crates/d/mempill.svg)](https://crates.io/crates/mempill)
[![PyPI](https://img.shields.io/pypi/v/mempill.svg)](https://pypi.org/project/mempill/)

**[Install](https://mempill.netlify.app/getting-started/install/) · [Documentation](https://mempill.netlify.app/) · [Concepts](https://mempill.netlify.app/concepts/temporal-validity-problem/) · [Examples](https://mempill.netlify.app/examples/) · [GitHub](https://github.com/mempill-ai/mempill)**

**0.3.0** · Apache-2.0 · MSRV 1.88 · 507 Rust + 155 Python + 19 MCP tests (main; + Postgres integration via `--features`), 0 warnings (`clippy --all-targets -D warnings` + `missing_docs`)
Includes: Rust core engine + SQLite/PostgreSQL adapters + oracle resolution loop + valid-time succession + Python wheel + MCP adapter + `mempill` facade crate + per-endpoint date granularity.


---

## The problem

AI agents accumulate beliefs over time. A belief stored last week — "the API endpoint is
`api.example.com/v1`", "the user lives in Berlin" — may be false today. Most agent memory
systems either overwrite the old belief silently (destroying history) or preserve it without
any signal that it might be wrong. Neither approach is safe for long-running agents.

This is the **temporal validity problem**: a stored belief can be well-sourced and internally
consistent yet factually wrong, because the underlying truth changed after the claim was
recorded. The failure is not about bad data at write time; it is about the passage of time
making previously-correct data stale — and the agent having no mechanism to detect or signal
that staleness.

## What mempill is

mempill is an **append-only, bi-temporal, provenance-aware claim store** that surfaces
conflict rather than silently resolving it. Every claim is written once and never mutated;
supersession is recorded as a new bounded assertion that links back to the original. The
engine maintains two time axes: **transaction-time** (engine-stamped, reliable) and
**valid-time** (caller-supplied, fallible, confidence-tagged). Belief is never stored — it
is recomputed at read time from the full claim history via a deterministic canonical fold.

Key properties:

- **Provenance firewall.** Every claim carries one of three typed provenance channels
  assigned at injection time and immutable thereafter: `External` (first-hand, cheap-path
  eligible), `RecallReEntry` (engine output re-entering the write path — caught by the
  Amplification Guard to prevent belief amplification loops), or `ModelDerived`
  (model-emitted, committed down-weighted, cannot overturn existing claims until anchored
  to first-hand external). The type system enforces exhaustiveness.

- **Contested is first-class.** When contradicting claims arrive and no oracle is present to
  adjudicate, the engine surfaces `Contested` rather than picking a winner. The 12-state
  disposition model (see Core Concepts) makes every outcome observable.

- **Deterministic core, stochastic-behind-a-gate.** The engine embeds no AI model. Extractor
  and oracle ports are pluggable traits; the host supplies implementations. Stochastic
  proposals never commit without passing the deterministic adjudication gate.

- **Single-writer-per-agent\_id** is a hard structural guarantee enforced by per-agent locks
  (SQLite) or advisory locking (PostgreSQL). Two concurrent processes MUST NOT hold write
  authority for the same agent\_id.

---

## Status and roadmap

| Feature | Status | Notes |
|---|---|---|
| Rust core engine (8 deterministic components, 12-state disposition model) | ✅ Shipped | Bi-temporal append-only claim store, deterministic adjudication gate |
| SQLite persistence adapter (topology-a) | ✅ Shipped | Embedded, file-per-agent, WAL + FULL sync |
| PostgreSQL adapter (topology-b) | ✅ Shipped | sync postgres 0.19 + r2d2; PG 16 + 18 tested; NoTls |
| Cross-adapter conformance suite | ✅ Shipped | SQLite and PostgreSQL proven behaviorally identical |
| Oracle resolution loop | ✅ Shipped | `submit_adjudication` (Affirm/Deny/Unknown) + engine-enforced TTL + orphan sweep; works on both adapters |
| Valid-time succession | ✅ Shipped | Non-overlapping confident valid-time windows fold to the claim valid at the query instant |
| Python PyO3 wheel (`mempill`) | ✅ Shipped | maturin 1.14, PyO3 0.29, Python ≥ 3.11; includes Python oracle bridge |
| MCP adapter (`mempill-mcp`) | ✅ Shipped | FastMCP, 4 tools, stdio transport |
| `mempill` facade crate | ✅ Shipped | `cargo add mempill`; thin re-export of core + adapters behind `sqlite`/`postgres` features |
| Bi-temporal history read (`query_history` / `history()`) | ✅ Shipped | Full claim timeline of a subject line — values, effective valid-time windows, `Current`/`Superseded` status |
| Valid-time as-of query (`valid_at`) | ✅ Shipped (0.3.0) | Point-in-time recall ("who was CEO in 2021?"); `valid_at` is a separate axis from `as_of_tx_time` — available in Rust, Python, and MCP. |
| Date precision / granularity | ✅ Shipped (0.3.0) | Per-endpoint `DateGranularity` (Year / Month / Day / Instant) on `ValidTime.start` and `ValidTime.end` independently. Honest display: Month→"2020-03", Year→"2020", Day→"2020-03-15"; no fabricated precision. Ergonomic `remember()` infers granularity from the supplied date string; structured ingest (raw `IngestClaimRequest`, Python dict, MCP) requires explicit granularity. Legacy rows (pre-feature) have `None` granularity and display as YYYY-MM-DD. Cross-adapter conformance included. |
| Vector search / VectorPort | ⏳ Planned | Structural seam exists (NoOp); no vector retrieval yet |
| TypeScript / napi-rs bindings (`mempill-ts`) | ⏳ Planned | Empty stub crate; no binding logic |
| PostgreSQL TLS | ⏳ Planned | Currently NoTls only (local/Docker) |
| Service tier (topology-c) | ⏳ Deferred | Multi-agent shared service; not in scope yet |
| Published to crates.io / PyPI | ✅ Shipped | `cargo add mempill` (crates.io) · `pip install mempill` (PyPI) |

The HITL reference oracle and console/LangGraph agent demos live in the separate `mempill-demo` repository.

---

## Production readiness & scope

mempill 0.3.0 is designed for **embedded and early-stage** use (bi-temporal fold, ACID writes,
cross-adapter conformance, append-only integrity — 507 Rust + 155 Python + 19 MCP tests on main).
Read this before deploying it at scale.

**Safe today for:**

- Embedded, single-process, single-tenant use (e.g. the SQLite adapter / MCP server).
- Local or private-network PostgreSQL at human scale — roughly ≤ ~1k agents, ≤ a few
  hundred claims per subject-line, and modest write rates. The correctness guarantees
  hold within this envelope.

**Current limits (operational hardening is on the roadmap — see [Status and roadmap](#status-and-roadmap)):**

- **Read cost scales with history.** Belief is recomputed from the full claim history of
  a subject-line on every read (it is never stored — that is the correctness model).
  There is no snapshot/compaction yet, so a long-lived, high-churn subject-line gets
  slower over time. Comfortable at hundreds of claims per subject-line; not yet tuned
  for tens of thousands. *(v0.3: snapshotting.)*
- **SQLite serializes writes globally.** All agents' writes go through a single writer
  lock, and reads error while a write transaction is open on that agent's file. Use the
  **PostgreSQL** adapter for write concurrency across agents.
- **PostgreSQL is `NoTls` only** — do not expose the connection over an untrusted
  network. The connection pool size is fixed (20) and not yet configurable. *(v0.3: TLS,
  configurable pool.)*
- **No built-in observability** — there is no `tracing`/metrics instrumentation yet, so
  latency, error rates, and contention are not visible to an operator out of the box.
  *(v0.3.)*
- **No published load/stress benchmarks** — all 507 Rust + 155 Python + 19 MCP tests are correctness tests;
  performance at large scale is not yet characterized.

**Not recommended yet for:** public-facing multi-tenant services, high-frequency
automated write pipelines, networked PostgreSQL with real credentials (until TLS), or
very high agent cardinality (the per-agent advisory lock uses a 32-bit hash).

If your use case is outside the safe envelope, the core algorithm is implemented and tested;
the gaps above are operational, not algorithmic. Treat 0.3.0 as an early release and pin a specific version.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         EngineHandle<P, O, V>                   │
│  async boundary: clock read once, per-agent lock, spawn_blocking│
└──────────────────────┬──────────────────────────────────────────┘
                       │ sync use-cases (IngestClaim / QueryMemory
                       │                  Reconcile / Audit)
┌──────────────────────▼──────────────────────────────────────────┐
│                    mempill-core (deterministic)                  │
│  C1 Gateway  →  C6 Firewall  →  C3 Reconciler  →  C7 Gate      │
│  C4 Supersession  C2 TruthEngine (fold)  C5 Projection  C8 Audit│
└────────────────────────────────┬────────────────────────────────┘
                                 │ PersistencePort (trait)
          ┌──────────────────────┴──────────────────────┐
          │                                             │
┌─────────▼──────────┐                     ┌───────────▼──────────┐
│  mempill-sqlite    │                     │  mempill-postgres    │
│  topology-a        │                     │  topology-b          │
│  file-per-agent_id │                     │  shared DB, r2d2     │
│  single-connection │                     │  advisory lock + OCC │
└────────────────────┘                     └──────────────────────┘
```

The eight engine components:

| Component | Role |
|---|---|
| Gateway | Entry validation, provenance enforcement, ModelDerived default |
| TruthEngine | Deterministic canonical valid-time fold; never stores belief |
| Reconciler | Contradiction classifier; produces conflict-type proposals for the gate |
| Supersession | Bound-assertion writer; non-destructive; cascades PendingReview to dependents |
| Projection | Currency decay, Contested surfacing, PendingReview marker assembly |
| Amplification Guard | RecallReEntry loop detection; burst quarantine; derivation-depth cap |
| AdjudicationGate | Deterministic cheap/heavy-path split; oracle-absent → Contested |
| AuditLedger | Immutable ledger of all disposition outcomes; queryable by tx_time |

Port traits defined in `mempill-core/src/ports/`:
`PersistencePort`, `OraclePort`, `ExtractorPort`, `EmbeddingPort`, `VectorPort`.
The host supplies concrete implementations; the engine embeds none.

---

## Install

### Rust

```sh
cargo add mempill                          # SQLite backend (default)
cargo add mempill --features postgres      # PostgreSQL backend
```

or in `Cargo.toml`:

```toml
[dependencies]
mempill = "0.3"                            # SQLite (default)
# mempill = { version = "0.3", features = ["postgres"] }
```

Power users can depend on individual crates directly from crates.io by version:
`mempill-core`, `mempill-sqlite`, `mempill-postgres` are all published at `"0.3"`.

### Python wheel

```sh
pip install mempill           # Python ≥ 3.11; prebuilt wheel from PyPI
```

Contributors building from source: `cd mempill-python && maturin develop --release`

### MCP adapter

`mempill-mcp` is not on PyPI — install from source:

```sh
# Install the Python wheel first: pip install mempill
cd mempill-mcp
pip install .
```

Run the MCP server:

```sh
export MEMPILL_AGENT_ID="my-agent"
export MEMPILL_DB_PATH="/path/to/agent.db"   # omit for in-memory
mempill-mcp
```

Or equivalently: `python -m mempill_mcp`

---

## Quick start

### Rust

```rust
use mempill::{open_default_in_memory, remember, recall, RememberOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = open_default_in_memory()?;

    remember(&engine, "my-agent", "user", "city", "Berlin", RememberOptions::default()).await?;

    let r = recall(&engine, "my-agent", "user", "city").await?;
    println!("city = {:?}", r.as_str());   // Some("Berlin")
    Ok(())
}
```

For full control — explicit provenance, cardinality, confidence, derivation lineage — use the
power-user API (`mempill::engine::IngestClaimRequest` / `mempill::engine::QueryMemoryRequest`).
The ergonomic tier is additive; the rigorous core is unchanged.

For PostgreSQL topology-b, open with `mempill::postgres::open_postgres`:

```rust
use mempill::postgres::{open_postgres, PostgresEngine};
use mempill::engine::{EngineConfig, NoOpOracle, NoOpVector};

let engine: PostgresEngine<NoOpOracle, NoOpVector> = open_postgres(
    "host=localhost port=5432 user=mempill dbname=mempill password=secret",
    None,   // no oracle
    None,   // no vector
    EngineConfig::default(),
)?;
```

### Python

```python
import mempill
from mempill import ProvenanceLabel, Disposition

engine = mempill.open_in_memory()

# Ingest a claim.
resp = engine.ingest_claim({
    "agent_id": "my-agent",
    "subject": "user",
    "predicate": "city",
    "value": "Berlin",
    "provenance": ProvenanceLabel.external_user_asserted(),
    "cardinality": "Functional",
    "confidence": {"value_confidence": 0.95, "valid_time_confidence": 0.0},
    "criticality": "Medium",
    "derived_from": [],
})

print(resp["claim_ref"], resp["disposition"])
assert resp["disposition"] == Disposition.CommittedCheap

# Query the belief back.
result = engine.query_memory({
    "agent_id": "my-agent",
    "subject": "user",
    "predicate": "city",
})
print(result["belief"])
```

### MCP

Set environment variables and start the server:

```sh
export MEMPILL_AGENT_ID="my-agent"
export MEMPILL_DB_PATH="/data/my-agent.db"   # omit for in-memory (ephemeral)
mempill-mcp
```

The server exposes four tools over stdio MCP transport:

| Tool | Description |
|---|---|
| `ingest_claim` | Write a belief claim (subject, predicate, value, provenance) |
| `query_memory` | Read the canonical belief for a (subject, predicate) pair |
| `reconcile` | Trigger conflict reconciliation for a set of subject lines |
| `audit` | Query the immutable ledger for claim history |

`ingest_claim` accepts provenance as a friendly string (`"External:UserAsserted"`,
`"External:ExternalFirstHand"`, `"RecallReEntry"`, `"ModelDerived"`) or as a wire-shape dict.
Non-committed dispositions (Contested, Quarantined, etc.) include a `status_reason` field.

---

## Core concepts

### Provenance channels (3-channel enum)

```
ProvenanceLabel::External(ExternalKind::UserAsserted)     — first-hand human assertion
ProvenanceLabel::External(ExternalKind::ExternalFirstHand)— tool result / system-of-record
ProvenanceLabel::RecallReEntry                            — engine output re-entering write path
ProvenanceLabel::ModelDerived                             — model-emitted / inferred content
```

`External(*)` is the only channel eligible for the cheap (non-conflicting commit) path.
`RecallReEntry` is caught by the Amplification Guard and corroborates by identity —
it never becomes ground truth, preventing the belief-amplification loop where the engine
reads its own output, re-ingests it as fresh evidence, and inflates confidence.
`ModelDerived` is committed down-weighted and cannot overturn existing claims until anchored
to a first-hand external claim.

Provenance is assigned at injection time and immutable (set once; no operation rewrites it).

### The 12 dispositions

Every write returns one of twelve dispositions:

| Disposition | Meaning |
|---|---|
| `CommittedCheap` | New non-conflicting first-hand external fact; committed Active at low currency |
| `CommittedInferred` | ModelDerived; committed down-weighted |
| `QueuedForAdjudication` | Belief-overturning op accepted into async adjudication |
| `Contested` | External contradiction; oracle absent; no resolution yet |
| `PendingConflict` | Not enough evidence to overturn; awaiting evidence/oracle |
| `PendingReview` | A depended-on parent was superseded; dependent flagged for review |
| `PendingLowConfidence` | Ambiguous source; awaiting corroboration |
| `Quarantined` | Burst/loop signature or incoherent temporal bounds; parked, auditable |
| `Superseded` | Belief-overturning accepted; prior claim bounded and retained |
| `Invalidated` | Validity assertion marks claim as no-longer-true; retained in history |
| `Reinstated` | Valid-time reopened by first-hand assertion |
| `Rejected` | Missing/invalid provenance, malformed fact, or write-authority violation |

### Claims, validity-assertions, and the ledger

- **Claim** — an immutable triple (subject, predicate, value) with provenance, confidence,
  cardinality, and optional valid-time bounds. Written once, never updated.
- **Validity assertion** — a separate record that bounds the valid-time of an existing claim
  (supersession writes one of these). Immutable.
- **Ledger entry** — an immutable audit record of every disposition outcome, queryable by
  agent\_id, claim\_ref, and tx\_time range.

Belief is derived from the full claim + assertion history at read time (I3). Nothing is
materialized as a single "current value" row.

### Key invariants

- **Non-destruction** — writes are INSERT-only. Supersession never deletes.
- **Bi-temporal by trust** — transaction-time is engine-stamped and reliable; valid-time
  is caller-supplied, fallible, and confidence-tagged.
- **Belief derived, never stored** — recomputed via canonical fold at read time.
- **Provenance immutable** — set at write time; no operation rewrites it.
- **Stochastic proposes, never commits** — the engine embeds no model; `ExtractorPort` returns
  proposals only; proposals pass through the deterministic adjudication gate before commit.
- **Idempotent append** — recall re-entry corroborates the existing claim; never duplicates.
- **Contested first-class** — unresolved conflicts are surfaced explicitly; never silently picked.
- **Read-time canonical** — the canonical valid-time fold is the authoritative definition of belief.
- **Atomic commit unit** — {claim + bounding assertion + ledger entry} commits as one indivisible unit.
- **Fixed-history monotonicity** — belief is monotone over a fixed history.
- **Currency decay** — claims decay with age; no DELETE; only an explicit negative assertion
  yields `Invalidated`.

---

## Persistence backends

### SQLite (topology-a) — embedded default

- One database file per agent\_id; single-connection serialized writes.
- Mandatory PRAGMAs: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`.
- `synchronous=FULL` is non-negotiable: WAL + NORMAL can lose writes on power loss.
- No external process required; zero configuration for single-agent embedded use.
- Choose SQLite when: single-agent deployment, embedded library, tests, MCP sessions.

### PostgreSQL (topology-b) — shared database

- r2d2 connection pool (max 20 connections) enables concurrent cross-agent transactions.
- Same-agent write serialization via `pg_advisory_xact_lock(hashtext(agent_id)::bigint)`.
- OCC belt-and-suspenders: `UNIQUE(agent_id, stream_seq)` on `ledger_entries`.
- `requires_global_write_serialization()` returns `false` — no global lock; true per-agent
  concurrency across multiple agents sharing one PostgreSQL database.
- Schema managed by refinery migrations (V1 migration embedded at compile time).
- Tested against PostgreSQL 16 and 18.4 via testcontainers.
- Current limitation: NoTls only. Production TLS is planned.
- Choose PostgreSQL when: multi-agent deployment, shared database, production service.

**Both adapters are proven behaviorally identical** by the shared `run_persistence_conformance`
harness in `mempill-core` (feature `test-support`), which runs the same conformance test suite
against both backends.

---

## Development

### Build

```sh
cargo build --workspace
```

### Test

```sh
cargo test --workspace
```

This runs the SQLite, core, and facade tests — fast and **Docker-free** (starts 0 containers).

The PostgreSQL integration tests spin up real `postgres:16` / `postgres:18.4` containers via
testcontainers, so they require Docker and are gated behind the **`postgres-integration`**
cargo feature (sqlx-style):

```sh
cargo test -p mempill-postgres --features postgres-integration       # the 69 PG tests
cargo test --workspace --features mempill-postgres/postgres-integration   # full verification (everything)
```

testcontainers' `watchdog` is enabled, so an interrupted run (Ctrl-C / SIGTERM) cleans up its
own containers. As a fallback (e.g. after a hard `kill -9`), sweep any strays with:

```sh
docker rm -f $(docker ps -aq --filter 'label=org.testcontainers.managed-by=testcontainers')
```

### Python tests

```sh
# Install the wheel first (maturin develop)
cd mempill-python && maturin develop && cd ..

# Run Python SDK tests
cd mempill-python && python -m pytest tests/

# Run MCP adapter tests
cd mempill-mcp && python -m pytest tests/
```

The project uses a `.venv` at the workspace root (gitignored). Toolchain:
maturin 1.14.1, PyO3 0.29, mcp 1.28 (pinned `<2`).

### Cross-adapter conformance

The shared conformance harness lives in `mempill-core/src/testing/` (compiled under the
`test-support` feature flag). Both `mempill-sqlite` and `mempill-postgres` activate this
feature in `[dev-dependencies]` and run the identical suite. Behavioral parity between
adapters is a hard requirement.

---

## Project layout

| Crate / package | Language | Role | Status |
|---|---|---|---|
| `mempill-types` | Rust | Domain types: `ProvenanceLabel`, `Disposition`, `Claim`, `LedgerEntry`, etc. | ✅ Shipped |
| `mempill-core` | Rust | All 8 engine components, port traits, use-cases, DTOs, `EngineHandle` | ✅ Shipped |
| `mempill-sqlite` | Rust | SQLite `PersistencePort` adapter; `DefaultEngine` alias + constructors | ✅ Shipped |
| `mempill-postgres` | Rust | PostgreSQL `PersistencePort` adapter; `PostgresEngine` alias | ✅ Shipped |
| `mempill` (facade) | Rust | Thin re-export of core + adapters; `cargo add mempill` with `sqlite`/`postgres` features | ✅ Shipped |
| `mempill-python` | Rust + Python | PyO3/maturin wheel (`mempill`); Python SDK with `Engine`, `Disposition`, `ProvenanceLabel` | ✅ Shipped |
| `mempill-mcp` | Python | FastMCP server; 4 tools; stdio transport; `MEMPILL_AGENT_ID` + `MEMPILL_DB_PATH` env contract | ✅ Shipped |
| `mempill-ts` | Rust | napi-rs TypeScript binding stub — **not yet implemented** | ⏳ Planned |

---

## License

mempill is licensed under **Apache-2.0**. See the [LICENSE](LICENSE) file for the full text.
