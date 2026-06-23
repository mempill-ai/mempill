# mempill-core

The deterministic engine core for mempill — port traits, all 8 engine components (C1–C8),
use-cases, DTOs, and the async `EngineHandle` entry point.

See the [root README](../README.md) for the full architecture, quick start, and invariants.

## Crate organization

```
mempill-core/src/
  ports/           — PersistencePort, OraclePort, ExtractorPort, EmbeddingPort, VectorPort traits
  engine/          — C1 gateway, C2 truth_engine, C3 reconciler, C4 supersession,
                     C5 projection, C6 firewall, C7 gate, C8 audit_ledger
  application/     — use-cases (IngestClaim, QueryMemory, Reconcile, Audit) + public DTOs
  engine_handle.rs — EngineHandle<P,O,V>: async entry point, spawn_blocking bridge
  config.rs        — EngineConfig (OP-3 tuning parameters)
  error.rs         — MemError enum (thiserror)
  noop.rs          — NoOpOracle, NoOpVector (do-nothing stubs)
  concurrency/     — AgentWriteLockMap (per-agent_id write lock)
  testing/         — shared conformance harness (feature = "test-support")
```

## Public API surface

```rust
// EngineHandle — sole public async entry point
pub struct EngineHandle<P: PersistencePort, O: OraclePort, V: VectorPort>;

impl<P, O, V> EngineHandle<P, O, V> {
    pub fn new(persistence: Arc<P>, oracle: Option<Arc<O>>, vector: Option<Arc<V>>, config: EngineConfig) -> Self;
    pub async fn ingest_claim(&self, req: IngestClaimRequest) -> Result<IngestClaimResponse, MemError>;
    pub async fn query_memory(&self, req: QueryMemoryRequest) -> Result<QueryMemoryResponse, MemError>;
    pub async fn reconcile(&self, req: ReconcileRequest) -> Result<ReconcileResponse, MemError>;
    pub async fn query_audit(&self, req: AuditQueryRequest) -> Result<AuditQueryResponse, MemError>;
}
```

Adapter crates (`mempill-sqlite`, `mempill-postgres`) provide concrete `PersistencePort`
implementations and expose convenience constructors (`open_default`, `open_postgres`).
`mempill-core` has no dependency on either adapter — the dependency direction is one-way.

## Feature flags

- `test-support` — compiles the shared `run_persistence_conformance` harness used by both
  adapter crates in `[dev-dependencies]`.

## License

AGPL-3.0 with linking exception. See the [root README](../README.md) for details.
