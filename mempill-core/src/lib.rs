//! # mempill-core
//!
//! Domain engine port traits, configuration, error model, and NoOp stubs for the
//! mempill temporally-correct AI-agent memory engine.
//!
//! ## Crate Organization
//!
//! - `ports/` — Hexagonal port traits (sync; no async fn). Public: visible to adapter crates.
//!   - [`ports::PersistencePort`] — INSERT-only, agent_id-first persistence seam.
//!   - [`ports::OraclePort`]      — Pull-based, non-blocking adjudication port.
//!   - [`ports::ExtractorPort`]   — Stochastic proposer port (returns proposals, never commits).
//!   - [`ports::EmbeddingPort`]   — BYO-embedding port for fuzzy candidate coverage.
//!   - [`ports::VectorPort`]      — v0.1 compile-time seam (unimplemented; v0.2 sqlite-vec).
//! - `config`  — [`EngineConfig`] struct with all OP-3 tuning parameters.
//! - `error`   — [`MemError`] enum (thiserror), [`WriteResult`], [`BeliefResult`] aliases.
//! - `noop`    — [`noop::NoOpOracle`], [`noop::NoOpVector`] — do-nothing stubs for tests.
//!
//! ## Wave Scope
//!
//! W3 adds the engine's first business logic modules:
//! - `engine/gateway` (C1) — ingestion gateway, provenance stamping, tx-time assignment.
//! - `engine/gate` (C7) — deterministic adjudication gate (pure function).
//! - `concurrency/agent_lock` — per-agent_id write lock (single-writer enforcement, A22).
//!
//! ## Sync Core Convention (F1, A20)
//!
//! All port traits and engine domain functions are synchronous. Async lives ONLY at the
//! `EngineHandle` boundary (W7) via `tokio::task::spawn_blocking`. The concurrency module
//! uses `tokio::sync` primitives (Mutex/RwLock) because the lock map is acquired by async
//! Tokio tasks — this is the lock layer, not the domain layer (A22).

pub mod config;
pub mod error;
pub mod noop;
pub mod ports;

pub(crate) mod engine;
pub(crate) mod concurrency;

// ── Key public re-exports ─────────────────────────────────────────────────────

pub use config::EngineConfig;
pub use error::{BeliefResult, MemError, WriteResult};
pub use noop::{NoOpOracle, NoOpVector};
pub use ports::{
    EmbeddingPort, ExtractorPort, OraclePort, PersistencePort, Txn, VectorPort,
};
