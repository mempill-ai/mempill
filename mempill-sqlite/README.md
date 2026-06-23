# mempill-sqlite

SQLite persistence adapter for mempill (topology-a).

See the [root README](../README.md) for the full architecture, quick start, and invariants.

## What this crate provides

- `SqlitePersistenceStore` — `impl PersistencePort` backed by a single rusqlite connection.
- `DefaultEngine` — type alias for `EngineHandle<SqlitePersistenceStore, NoOpOracle, NoOpVector>`.
- `open_default(path)` — open a file-backed engine at the given path.
- `open_default_in_memory()` — open an ephemeral in-memory engine (tests, MCP sessions).

## Usage

```rust
use mempill_sqlite::open_default_in_memory;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_types::{AgentId, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = open_default_in_memory()?;
    let agent = AgentId("my-agent".into());

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

    println!("{:?}", resp.disposition);
    Ok(())
}
```

## PRAGMA contract

Applied at connection open, before any DML:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous  = FULL;   -- mandatory: WAL+NORMAL can lose writes on power loss
PRAGMA foreign_keys = ON;
```

## When to use SQLite vs PostgreSQL

- Single-agent deployment, embedded library, tests, MCP sessions → **SQLite (this crate)**
- Multi-agent shared database, production service → **mempill-postgres**

Both adapters implement the same `PersistencePort` trait and are proven behaviorally
identical by the shared conformance harness.

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
