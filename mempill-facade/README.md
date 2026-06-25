# mempill

Temporally-correct memory for AI agents — bi-temporal claim store with Contested-first conflict surfacing and oracle resolution.

This crate is a thin facade over [`mempill-core`](../mempill-core) and the persistence adapters.

## Usage

```toml
[dependencies]
mempill = "0.2"                          # default = SQLite backend
# or:
mempill = { version = "0.2", features = ["postgres"] }
```

## Quick start

```rust
use mempill::sqlite::open_default_in_memory;
use mempill::{IngestClaimRequest, QueryMemoryRequest};
use mempill_types::{AgentId, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = mempill::open_default_in_memory()?;
    let agent  = AgentId("my-agent".into());

    let resp = engine.ingest_claim(IngestClaimRequest {
        agent_id:    agent.clone(),
        subject:     "user".into(),
        predicate:   "city".into(),
        value:       serde_json::json!("Berlin"),
        provenance:  ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time:  None,
        confidence:  Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }).await?;

    println!("disposition: {:?}", resp.disposition);
    Ok(())
}
```

## Feature flags

| Feature | Default | Description |
|---------|---------|-------------|
| `sqlite` | yes | Embedded SQLite adapter (topology-a, file-per-agent) |
| `postgres` | no | Shared PostgreSQL adapter (topology-b, r2d2 pool, advisory locking) |

## License

Apache-2.0. See [LICENSE](../LICENSE) for the full text.
