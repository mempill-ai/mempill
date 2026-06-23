# mempill

**Temporally-correct AI-agent memory — append-only, bi-temporal, provenance-aware.**

Status: 311 tests · AGPL-3.0 · v0.3 (Rust + Python wheel + MCP adapter + PostgreSQL adapter)

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
is recomputed at read time from the full claim history via a deterministic canonical fold
(invariant I3/I8).

Key properties:

- **Provenance firewall.** Every claim carries one of three typed provenance channels
  assigned at injection time and immutable thereafter: `External` (first-hand, cheap-path
  eligible), `RecallReEntry` (engine output re-entering the write path — caught by the
  Amplification Guard to prevent belief amplification loops), or `ModelDerived`
  (model-emitted, committed down-weighted). The type system enforces exhaustiveness.

- **Contested is first-class.** When contradicting claims arrive and no oracle is present to
  adjudicate, the engine surfaces `Contested` rather than picking a winner. The 12-state
  disposition model (see Core Concepts) makes every outcome observable.

- **Deterministic core, stochastic-behind-a-gate.** The engine embeds no AI model. Extractor
  and oracle ports are pluggable traits; the host supplies implementations. Stochastic
  proposals never commit without passing the deterministic adjudication gate (C7).

- **Single-writer-per-agent\_id** is a hard structural guarantee enforced by per-agent locks
  (SQLite) or advisory locking (PostgreSQL). Two concurrent processes MUST NOT hold write
  authority for the same agent\_id.

---

## Status and roadmap

| Feature | Status | Notes |
|---|---|---|
| Rust core engine (all 8 components C1–C8) | ✅ v0.1 | 290 Rust tests, 0 warnings |
| SQLite persistence adapter (topology-a) | ✅ v0.1 | Embedded, file-per-agent, WAL + FULL sync |
| Python PyO3 wheel (`mempill`) | ✅ v0.2 | maturin 1.14, PyO3 0.29, Python ≥ 3.11 |
| MCP adapter (`mempill-mcp`) | ✅ v0.2 | FastMCP, 4 tools, stdio transport |
| PostgreSQL adapter (topology-b) | ✅ v0.3 | sync postgres 0.19 + r2d2, PG 16 + 18 tested |
| Cross-adapter conformance harness | ✅ v0.3 | SQLite and Postgres proven behaviorally identical |
| Vector search / VectorPort | ⏳ Planned | Structural seam exists (NoOp); no vector retrieval yet |
| TypeScript / napi-rs bindings (`mempill-ts`) | ⏳ Planned | Empty stub crate; no binding logic |
| PostgreSQL TLS | ⏳ v0.3.1 | Currently NoTls only (local/Docker) |
| Service tier (topology-c) | ⏳ Deferred | Multi-agent shared service; not in scope yet |
| Published to crates.io / PyPI | ⏳ Planned | Not yet published; use path/git dependencies |

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

| ID | Component | Role |
|---|---|---|
| C1 | Gateway | Entry validation, provenance enforcement, ModelDerived default |
| C2 | TruthEngine | Deterministic canonical valid-time fold (I8); never stores belief |
| C3 | Reconciler | Contradiction classifier; reuses gate ConflictType/Proposal |
| C4 | Supersession | Bound-assertion writer; non-destructive (I1); cascades PendingReview |
| C5 | Projection | Currency decay (I11), Contested (I7), PendingReview surfacing |
| C6 | Firewall / AmplificationGuard | RecallReEntry loop detection; burst quarantine; OP-1 depth cap |
| C7 | AdjudicationGate | Deterministic cheap/heavy-path split; oracle-absent → Contested |
| C8 | AuditLedger | Immutable ledger of all disposition outcomes; queryable by tx_time |

Port traits defined in `mempill-core/src/ports/`:
`PersistencePort`, `OraclePort`, `ExtractorPort`, `EmbeddingPort`, `VectorPort`.
The host supplies concrete implementations; the engine embeds none.

---

## Install

### Rust

mempill is not yet published to crates.io. Add via path or git:

```toml
# Cargo.toml
[dependencies]
mempill-sqlite = { path = "../mempill/mempill-sqlite" }   # or git = "..."
mempill-core   = { path = "../mempill/mempill-core" }
```

For PostgreSQL topology-b:

```toml
mempill-postgres = { path = "../mempill/mempill-postgres" }
```

### Python wheel

Requires Python ≥ 3.11 and a Rust toolchain.

```sh
# Build and install the wheel into your environment
cd mempill-python
pip install maturin
maturin develop --release    # or: maturin build --release && pip install target/wheels/*.whl
```

### MCP adapter

```sh
# Install the wheel first (see above), then:
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
use mempill_sqlite::open_default_in_memory;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_types::{AgentId, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = open_default_in_memory()?;
    let agent = AgentId("my-agent".into());

    // Ingest a claim.
    let resp = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "city".into(),
        value: serde_json::json!("Berlin"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }).await?;

    println!("claim_ref={}, disposition={:?}", resp.claim_ref, resp.disposition);

    // Query the belief back.
    let query = engine.query_memory(QueryMemoryRequest {
        agent_id: agent,
        subject: "user".into(),
        predicate: "city".into(),
        as_of_tx_time: None,
    }).await?;

    println!("belief={:?}", query.belief);
    Ok(())
}
```

For PostgreSQL topology-b, open with `mempill_postgres::open_postgres`:

```rust
use mempill_postgres::{open_postgres, PostgresEngine};
use mempill_core::{EngineConfig, NoOpOracle, NoOpVector};

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
`RecallReEntry` is caught by the Amplification Guard (C6) and corroborates by identity —
it never becomes ground truth, preventing the belief-amplification loop where the engine
reads its own output, re-ingests it as fresh evidence, and inflates confidence.
`ModelDerived` is committed down-weighted and cannot overturn existing claims until anchored
to a first-hand external claim.

Provenance is assigned at injection time and immutable (invariant I4).

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

### Key invariants (plain language)

- **I1 Non-destruction** — writes are INSERT-only. Supersession never deletes.
- **I2 Bi-temporal by trust** — transaction-time is engine-stamped and reliable; valid-time
  is caller-supplied, fallible, and confidence-tagged.
- **I3 Belief derived, never stored** — recomputed via canonical fold at read time.
- **I4 Provenance immutable** — set at write time; no operation rewrites it.
- **I5 Stochastic proposes, never commits** — engine embeds no model; ExtractorPort returns
  proposals only; proposals pass through the deterministic gate (C7) before commit.
- **I6 Idempotent append** — recall re-entry corroborates existing claim; never duplicates.
- **I7 Contested first-class** — unresolved conflicts surfaced explicitly; never silently picked.
- **I8 Read-time canonical** — canonical valid-time fold is the authoritative definition of belief.
- **I9 Atomic commit unit** — {claim + bounding assertion + ledger entry} commits as one unit.
- **I10 Fixed-history monotonicity** — belief is monotone over fixed history.
- **I11 Currency decay** — claims decay with age; no DELETE; only explicit negative assertion
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
- Current limitation: NoTls only. Production TLS is planned for v0.3.1.
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

The PostgreSQL integration tests in `mempill-postgres` require Docker (testcontainers pulls
`postgres:16` and `postgres:18.4` automatically). The SQLite and core tests run without Docker.

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
| `mempill-types` | Rust | Domain types: `ProvenanceLabel`, `Disposition`, `Claim`, `LedgerEntry`, etc. | ✅ v0.1 |
| `mempill-core` | Rust | Engine components C1–C8, port traits, use-cases, DTOs, `EngineHandle` | ✅ v0.1 |
| `mempill-sqlite` | Rust | SQLite `PersistencePort` adapter; `DefaultEngine` alias + constructors | ✅ v0.1 |
| `mempill-postgres` | Rust | PostgreSQL `PersistencePort` adapter; `PostgresEngine` alias | ✅ v0.3 |
| `mempill-python` | Rust + Python | PyO3/maturin wheel (`mempill`); Python SDK with `Engine`, `Disposition`, `ProvenanceLabel` | ✅ v0.2 |
| `mempill-mcp` | Python | FastMCP server; 4 tools; stdio transport; `MEMPILL_AGENT_ID` + `MEMPILL_DB_PATH` env contract | ✅ v0.2 |
| `mempill-ts` | Rust | napi-rs TypeScript binding stub — **not yet implemented** | ⏳ Planned |

---

## License

mempill is licensed under **AGPL-3.0** with a **linking/binding exception**: host embedding
of the Python, TypeScript, or other binding layers is not copyleft-propagating. The exception
covers the FFI boundary only; derivative works of the core engine remain AGPL-3.0.

A `LICENSE` file is not yet present in the repository. Adding one (with the AGPL-3.0 text
and the linking exception clause) is recommended before any public release.
