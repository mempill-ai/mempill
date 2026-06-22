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
//! This is Wave 2 (W2): the foundation layer. Engine business logic (C1–C8, `engine/`),
//! the application layer (`application/`), and `EngineHandle` are implemented in W3–W7.
//!
//! ## Sync Core Convention (F1, A20)
//!
//! All port traits are synchronous. Async lives ONLY at the `EngineHandle` boundary (W7)
//! via `tokio::task::spawn_blocking`. Do NOT add `async fn` to any trait in this crate.

pub mod config;
pub mod error;
pub mod noop;
pub mod ports;

// ── Key public re-exports ─────────────────────────────────────────────────────

pub use config::EngineConfig;
pub use error::{BeliefResult, MemError, WriteResult};
pub use noop::{NoOpOracle, NoOpVector};
pub use ports::{
    EmbeddingPort, ExtractorPort, OraclePort, PersistencePort, Txn, VectorPort,
};
