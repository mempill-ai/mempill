//! # mempill-core
//!
//! Domain engine port traits, configuration, error model, NoOp stubs, use-cases, DTOs,
//! and the async EngineHandle for the mempill temporally-correct AI-agent memory engine.
//!
//! ## Crate Organization
//!
//! - `ports/` — Hexagonal port traits (sync; no async fn). Public: visible to adapter crates.
//!   - [`ports::PersistencePort`] — INSERT-only, agent_id-first persistence seam.
//!   - [`ports::OraclePort`]      — Pull-based, non-blocking adjudication port.
//!   - [`ports::ExtractorPort`]   — Stochastic proposer port (returns proposals, never commits).
//!   - [`ports::EmbeddingPort`]   — BYO-embedding port for fuzzy candidate coverage.
//!   - [`ports::VectorPort`]      — v0.1 compile-time seam (unimplemented; v0.2 sqlite-vec).
//! - `config`  — [`EngineConfig`] struct with all tunable engine parameters.
//! - `error`   — [`MemError`] enum (thiserror), [`WriteResult`], [`BeliefResult`] aliases.
//! - `noop`    — [`noop::NoOpOracle`], [`noop::NoOpVector`] — do-nothing stubs for tests.
//! - `application/` — use-cases (IngestClaim, QueryMemory, Reconcile, Audit) + public DTOs.
//! - `engine_handle` — [`EngineHandle`] async public entry point; bridges async callers to sync core.
//!
//! ## Sync Core Convention
//!
//! All port traits and engine domain functions are synchronous. Async lives ONLY at the
//! `EngineHandle` boundary via `tokio::task::spawn_blocking`. The concurrency module
//! uses `tokio::sync` primitives (Mutex/RwLock) because the lock map is acquired by async
//! Tokio tasks — this is the lock layer, not the domain layer.

pub mod application;
pub mod config;
pub mod engine_handle;
pub mod error;
pub mod noop;
pub mod ports;

pub(crate) mod concurrency;
pub(crate) mod engine;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

// ── Key public re-exports ─────────────────────────────────────────────────────

pub use application::{
    AuditQueryRequest, AuditQueryResponse, AuditUseCase, IngestClaimRequest, IngestClaimResponse,
    IngestClaimUseCase, QueryMemoryRequest, QueryMemoryResponse, QueryMemoryUseCase,
    ReconcileRequest, ReconcileResponse, ReconcileUseCase,
};
pub use config::EngineConfig;
pub use engine_handle::{EngineHandle, ErasedPendingStore, ErasedPendingStoreAdapter};
pub use error::{BeliefResult, MemError, WriteResult};
pub use noop::{NoOpOracle, NoOpVector};
pub use ports::{
    EmbeddingPort, ExtractorPort, OraclePort, PendingAdjudicationPort, PendingAdjudicationRow,
    PersistencePort, Txn, VectorPort,
};
