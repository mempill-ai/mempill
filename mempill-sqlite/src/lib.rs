//! `mempill-sqlite` — SQLite persistence adapter for mempill.
//!
//! This crate provides the SQLite-backed implementation of the `PersistencePort` trait
//! defined in `mempill-core`.  It owns the database schema (DDL + indexes), the
//! idempotent schema migration runner, and the write path for Wave 5.
//!
//! # Crate organisation
//!
//! - [`connection`] — connection lifecycle: open file or in-memory, apply mandatory
//!   PRAGMAs (`journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`), run migrations.
//! - [`migrations`] — deterministic, idempotent schema migration runner; embeds DDL via
//!   `include_str!`.
//! - [`txn`] — [`SqliteTxn`]: the concrete `Txn` handle scoped to one `agent_id` (I9).
//! - [`store`] — [`SqlitePersistenceStore`]: `impl PersistencePort` — WRITE methods (W5).
//!   READ methods are stubbed with `todo!("W6 — …")` pending Wave 6.
//! - `fold.rs` — canonical valid-time fold SQL query (Wave 6, not yet implemented).
//!
//! # PRAGMA contract (applied at connection open — before migrations or any DML)
//!
//! ```sql
//! PRAGMA journal_mode = WAL;     -- concurrent reads during writes
//! PRAGMA synchronous  = FULL;    -- full-durability writes (DC-D, CONSTRAINTS.md §D)
//! PRAGMA foreign_keys = ON;      -- enforce FK constraints defined in DDL
//! ```
//!
//! # Wave scope
//!
//! W5 (this wave): connection lifecycle, `SqliteTxn`, `SqlitePersistenceStore` WRITE path.
//! W6: READ path (`load_subject_line`, `load_claim`, `load_validity_assertions_for`,
//!     `load_ledger`, `load_edges_for`, `load_injected_claims`, `load_lineage`) + `fold.rs`.

pub mod connection;
pub mod migrations;
pub mod store;
pub mod txn;

pub use store::SqlitePersistenceStore;

// ── Crate-level error type ────────────────────────────────────────────────────

/// Error type for all `mempill-sqlite` operations.
#[derive(Debug, thiserror::Error)]
pub enum SqliteStoreError {
    /// A rusqlite-level database error.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A schema migration error.
    #[error("Migration error: {0}")]
    Migration(#[from] migrations::MigrationError),

    /// A domain-type ↔ column mapping error (serialization / unknown enum value).
    #[error("Mapping error: {0}")]
    Mapping(String),

    /// `begin_atomic` called while a transaction is already active on this store instance.
    #[error("a transaction is already open on this store; commit or rollback before beginning a new one")]
    TxnAlreadyOpen,
}

// Compile-time assertion: SqliteStoreError must be Send + Sync to satisfy
// the `PersistencePort::Error: Send + Sync + 'static` bound.
const _: () = {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    fn check() { assert_send_sync::<SqliteStoreError>(); }
};
