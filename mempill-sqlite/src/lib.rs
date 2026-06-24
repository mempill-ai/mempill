//! `mempill-sqlite` — SQLite persistence adapter for mempill.
//!
//! This crate provides the SQLite-backed implementation of the `PersistencePort` trait
//! defined in `mempill-core`.  It owns the database schema (DDL + indexes), the
//! idempotent schema migration runner, and the full read + write path.
//!
//! # Crate organisation
//!
//! - [`connection`] — connection lifecycle: open file or in-memory, apply mandatory
//!   PRAGMAs (`journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`), run migrations.
//! - [`migrations`] — deterministic, idempotent schema migration runner; embeds DDL via
//!   `include_str!`.
//! - [`txn`] — [`SqliteTxn`]: the concrete `Txn` handle scoped to one `agent_id` (I9).
//! - [`store`] — [`SqlitePersistenceStore`]: `impl PersistencePort` — full read + write path.
//! - [`DefaultEngine`] — convenience type alias + constructors for the most common setup.
//!
//! # PRAGMA contract (applied at connection open — before migrations or any DML)
//!
//! ```sql
//! PRAGMA journal_mode = WAL;     -- concurrent reads during writes
//! PRAGMA synchronous  = FULL;    -- full-durability writes (DC-D, CONSTRAINTS.md §D)
//! PRAGMA foreign_keys = ON;      -- enforce FK constraints defined in DDL
//! ```
//!
//! # DefaultEngine
//!
//! For the common case (SQLite store, no oracle, no vector), use:
//! ```rust,ignore
//! let engine = mempill_sqlite::open_default_in_memory();
//! ```

pub mod connection;
pub mod migrations;
pub mod store;
pub mod txn;

pub use store::{SqlitePendingStore, SqlitePersistenceStore};

// Re-export OraclePort bound so callers can write the `open_with_oracle` constraint
// without adding a direct dependency on mempill-core in their Cargo.toml.
pub use mempill_core::ports::OraclePort;

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

// ── DefaultEngine — convenience type alias (E3/E4, A27) ──────────────────────
//
// Lives here in mempill-sqlite to preserve the dependency direction:
//   mempill-sqlite → mempill-core  (allowed)
//   mempill-core   → mempill-sqlite  (FORBIDDEN)

/// The default concrete engine type: SQLite persistence, no oracle, no vector.
///
/// Suitable for single-process embedded use without oracle or vector search.
/// For production with an oracle, construct `EngineHandle` directly with your port impls.
pub type DefaultEngine = mempill_core::EngineHandle<
    SqlitePersistenceStore,
    mempill_core::NoOpOracle,
    mempill_core::NoOpVector,
>;

// ── OracleEngine — type alias for a SQLite engine with a real oracle ──────────

/// An `EngineHandle` backed by SQLite persistence, a caller-supplied oracle, and no vector.
///
/// Use `open_with_oracle` or `open_with_oracle_in_memory` to obtain one.
pub type OracleEngine<O> = mempill_core::EngineHandle<
    SqlitePersistenceStore,
    O,
    mempill_core::NoOpVector,
>;

// ── open_with_oracle constructors ─────────────────────────────────────────────

/// Open a **file-backed** SQLite engine wired with a real oracle.
///
/// The pending-adjudication store is constructed from the same SQLite connection,
/// enabling full W4–W5 oracle resolution. `open_default` / `DefaultEngine` remain unchanged.
///
/// # Errors
/// Returns `SqliteStoreError` if the connection cannot be opened or migrations fail.
pub fn open_with_oracle<O>(
    path: &str,
    oracle: std::sync::Arc<O>,
) -> Result<OracleEngine<O>, SqliteStoreError>
where
    O: OraclePort + Send + Sync + 'static,
{
    let conn = connection::open(path)?;
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    let pending_store: std::sync::Arc<dyn mempill_core::ErasedPendingStore> =
        std::sync::Arc::new(mempill_core::ErasedPendingStoreAdapter::new(store.pending_store()));
    Ok(mempill_core::EngineHandle::new_with_pending_store::<()>(
        store,
        Some(oracle),
        None::<std::sync::Arc<mempill_core::NoOpVector>>,
        pending_store,
        mempill_core::EngineConfig::default(),
    ))
}

/// Open an **in-memory** SQLite engine wired with a real oracle.
///
/// Useful for integration tests and ephemeral oracle-enabled contexts.
///
/// # Errors
/// Returns `SqliteStoreError` if the connection cannot be opened or migrations fail.
pub fn open_with_oracle_in_memory<O>(
    oracle: std::sync::Arc<O>,
) -> Result<OracleEngine<O>, SqliteStoreError>
where
    O: OraclePort + Send + Sync + 'static,
{
    let conn = connection::open_in_memory()?;
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    let pending_store: std::sync::Arc<dyn mempill_core::ErasedPendingStore> =
        std::sync::Arc::new(mempill_core::ErasedPendingStoreAdapter::new(store.pending_store()));
    Ok(mempill_core::EngineHandle::new_with_pending_store::<()>(
        store,
        Some(oracle),
        None::<std::sync::Arc<mempill_core::NoOpVector>>,
        pending_store,
        mempill_core::EngineConfig::default(),
    ))
}

// ── DefaultEngine constructors ────────────────────────────────────────────────

/// Open a file-backed `DefaultEngine` at the given path.
///
/// The connection is fully initialised (PRAGMAs + migrations) before the handle is returned.
///
/// # Errors
/// Returns `SqliteStoreError` if the connection cannot be opened or migrations fail.
pub fn open_default(path: &str) -> Result<DefaultEngine, SqliteStoreError> {
    let conn = connection::open(path)?;
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    Ok(mempill_core::EngineHandle::new(
        store,
        None,
        None,
        mempill_core::EngineConfig::default(),
    ))
}

/// Open an **in-memory** `DefaultEngine`.
///
/// Useful for tests and ephemeral engine contexts.
///
/// # Errors
/// Returns `SqliteStoreError` if the connection cannot be opened or migrations fail.
pub fn open_default_in_memory() -> Result<DefaultEngine, SqliteStoreError> {
    let conn = connection::open_in_memory()?;
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    Ok(mempill_core::EngineHandle::new(
        store,
        None,
        None,
        mempill_core::EngineConfig::default(),
    ))
}

// ── End-to-end smoke tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
    use mempill_types::{
        AgentId, BeliefStatus, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel,
    };

    /// E2E smoke test: ingest a claim then query it back.
    ///
    /// This is the first real end-to-end proof: full write path (C1→C6→C3→C7 + I9 Txn)
    /// followed by the full read path (C2→C5 projection). No mocks.
    #[tokio::test]
    async fn e2e_ingest_then_query_returns_belief() {
        let engine = open_default_in_memory().expect("in-memory engine must open");
        let agent = AgentId("e2e-agent".into());

        // Ingest a claim.
        let ingest_req = IngestClaimRequest {
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
        };

        let ingest_resp = engine.ingest_claim(ingest_req).await
            .expect("ingest must succeed");
        assert!(!ingest_resp.claim_ref.0.is_nil(), "claim_ref must be non-nil");
        assert_eq!(ingest_resp.disposition, mempill_types::Disposition::CommittedCheap,
            "first External claim must be CommittedCheap");

        // Query the belief back.
        let query_req = QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "city".into(),
            as_of_tx_time: None,
        };
        let query_resp = engine.query_memory(query_req).await
            .expect("query must succeed");

        // The belief must reflect the ingested claim.
        assert!(
            matches!(
                query_resp.belief.status,
                BeliefStatus::Resolved | BeliefStatus::TimingUncertain
            ),
            "belief status must be Resolved or TimingUncertain after ingest, got {:?}",
            query_resp.belief.status
        );
        assert!(query_resp.belief.primary.is_some(), "primary belief must be present");
        let primary = query_resp.belief.primary.unwrap();
        assert_eq!(primary.fact.value, serde_json::json!("Berlin"),
            "fact value must match the ingested value");
        assert_eq!(primary.claim_ref, ingest_resp.claim_ref,
            "queried claim_ref must match the ingested claim_ref");
    }

    /// Confirm DefaultEngine type alias is in mempill-sqlite (not mempill-core).
    /// This verifies no mempill-core → mempill-sqlite dependency was introduced.
    #[test]
    fn default_engine_type_alias_exists_in_mempill_sqlite() {
        // This test compiles only if DefaultEngine is defined in this crate.
        fn assert_is_default_engine(_: &DefaultEngine) {}
        let engine = open_default_in_memory().unwrap();
        assert_is_default_engine(&engine);
    }
}
