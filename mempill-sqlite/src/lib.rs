//! `mempill-sqlite` — SQLite persistence adapter for mempill.
//!
//! This crate provides the SQLite-backed implementation of the persistence layer.
//! It owns the database schema (DDL + indexes), schema migration runner, and will
//! host the [`PersistencePort`] implementation in Wave 5.
//!
//! # Crate organisation
//!
//! - [`migrations`] — deterministic, idempotent schema migration runner; embeds DDL via
//!   `include_str!`. Call [`migrations::apply_migrations`] once per connection before use.
//!
//! # Not yet implemented (Wave 5)
//!
//! - `connection.rs` — per-agent_id connection lifecycle + PRAGMA initialisation
//!   (`journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`).
//! - `store.rs` — `SqlitePersistenceStore`: impl of `mempill_core::ports::PersistencePort`.
//! - `txn.rs` — `SqliteTxn`: the `Txn` handle scoped to one `agent_id`.
//! - `fold.rs` — canonical valid-time fold SQL query (recursive CTE lineage traversal).
//!
//! # PRAGMA intent (applied at connection open — connection.rs, W5)
//!
//! ```sql
//! PRAGMA journal_mode=WAL;     -- concurrent reads during writes
//! PRAGMA synchronous=FULL;     -- full-durability writes (DC-D, CONSTRAINTS.md §D)
//! PRAGMA foreign_keys=ON;      -- enforce FK constraints defined in DDL
//! ```

pub mod migrations;
