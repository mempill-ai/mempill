//! # mempill-postgres
//!
//! PostgreSQL-backed `PersistencePort` adapter for mempill (topology-b, A38).
//!
//! ## Usage
//!
//! ```no_run
//! use mempill_postgres::{open_postgres, PostgresEngine};
//! use mempill_core::{EngineConfig, NoOpOracle, NoOpVector};
//! use std::sync::Arc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let conn_str = "host=localhost port=5432 user=mempill dbname=mempill password=secret";
//! let engine: PostgresEngine<NoOpOracle, NoOpVector> = open_postgres(conn_str, None, None, EngineConfig::default())?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Topology-b vs topology-a
//!
//! One backend per deployment: downstream consumers depend on EITHER `mempill-sqlite`
//! OR `mempill-postgres`, never both.
//!
//! ## Concurrency model (A39–A42)
//!
//! - r2d2 pool (max 20 connections) enables concurrent cross-agent transactions.
//! - `pg_advisory_xact_lock(hashtext(agent_id)::bigint)` serializes same-agent writes (A40).
//! - `UNIQUE(agent_id, stream_seq)` on `ledger_entries` provides OCC belt-and-suspenders (A41).
//! - `requires_global_write_serialization()` returns `false` — EngineHandle skips the
//!   global write lock, enabling true Postgres concurrency across agents (A42).

// Embed migrations at compile time; no live DB needed to compile.
refinery::embed_migrations!("migrations");

mod connection;
mod store;
mod txn;

pub use connection::{PostgresPersistenceStore, PostgresStoreError};
pub use store::open_postgres;
pub use txn::PostgresTxn;

/// Type alias for an `EngineHandle` backed by `PostgresPersistenceStore`.
///
/// `O` = OraclePort implementation, `V` = VectorPort implementation.
pub type PostgresEngine<O, V> = mempill_core::EngineHandle<PostgresPersistenceStore, O, V>;
