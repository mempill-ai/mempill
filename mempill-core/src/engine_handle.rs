//! EngineHandle — sole public async entry point for mempill (§4a, A20, A22, F1).
//!
//! Owns `Arc<impl Port>` references plus the per-agent_id write lock map.
//! Every public method:
//!   1. Reads the clock ONCE at the async boundary (`now = Utc::now()`).
//!   2. Acquires the per-agent_id write lock for write operations.
//!   3. Delegates to the sync use-case via `tokio::task::spawn_blocking`.
//!   4. Maps `JoinError` → `MemError::SpawnBlocking`.
//!
//! The use-case layer is fully synchronous — no async code below this file.
//!
//! # Pending-adjudication port (W3)
//!
//! `EngineHandle` carries an optional `Arc<dyn ErasedPendingStore>` for the oracle queue.
//! Use `EngineHandle::new` for the standard case (no pending store) and
//! `EngineHandle::new_with_pending_store` when wiring in a concrete adapter.
//! The type-erasure lets `EngineHandle<P, O, V>` keep its existing 3-param signature.

use std::sync::Arc;

use chrono::Utc;
use tokio::task;

use crate::{
    application::{
        audit::AuditUseCase,
        dto::{
            AuditQueryRequest, AuditQueryResponse, IngestClaimRequest, IngestClaimResponse,
            QueryMemoryRequest, QueryMemoryResponse, ReconcileRequest, ReconcileResponse,
        },
        ingest_claim::IngestClaimUseCase,
        query_memory::QueryMemoryUseCase,
        reconcile::ReconcileUseCase,
    },
    concurrency::agent_lock::AgentWriteLockMap,
    config::EngineConfig,
    error::MemError,
    ports::{OraclePort, PendingAdjudicationPort, PersistencePort, VectorPort},
};

// ── Type-erased pending store ─────────────────────────────────────────────────
//
// `PendingAdjudicationPort` is NOT object-safe in its generic form because `Self::Error`
// is an associated type. We introduce a thin object-safe erasing wrapper that boxes errors.

/// Object-safe erasing wrapper for `PendingAdjudicationPort`.
///
/// Adapters implement `PendingAdjudicationPort`; this wrapper is created via
/// `ErasedPendingStoreAdapter::new(concrete_store)` and stored as `Arc<dyn ErasedPendingStore>`.
pub trait ErasedPendingStore: Send + Sync + 'static {
    fn insert_pending_erased(
        &self,
        row: &crate::ports::pending_adjudication::PendingAdjudicationRow,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>;

    fn get_pending_erased(
        &self,
        handle_id: uuid::Uuid,
    ) -> Result<Option<crate::ports::pending_adjudication::PendingAdjudicationRow>, Box<dyn std::error::Error + Send + Sync + 'static>>;

    fn list_pending_erased(
        &self,
        agent_id: Option<&mempill_types::AgentId>,
    ) -> Result<Vec<crate::ports::pending_adjudication::PendingAdjudicationRow>, Box<dyn std::error::Error + Send + Sync + 'static>>;

    fn list_expired_erased(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::ports::pending_adjudication::PendingAdjudicationRow>, Box<dyn std::error::Error + Send + Sync + 'static>>;

    fn mark_resolved_erased(
        &self,
        handle_id: uuid::Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>;
}

/// Adapter that wraps a concrete `PendingAdjudicationPort` impl as `dyn ErasedPendingStore`.
pub struct ErasedPendingStoreAdapter<S: PendingAdjudicationPort> {
    inner: S,
}

impl<S: PendingAdjudicationPort> ErasedPendingStoreAdapter<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: PendingAdjudicationPort> ErasedPendingStore for ErasedPendingStoreAdapter<S> {
    fn insert_pending_erased(
        &self,
        row: &crate::ports::pending_adjudication::PendingAdjudicationRow,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.insert_pending(row).map_err(|e| Box::new(e) as _)
    }

    fn get_pending_erased(
        &self,
        handle_id: uuid::Uuid,
    ) -> Result<Option<crate::ports::pending_adjudication::PendingAdjudicationRow>, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.get_pending(handle_id).map_err(|e| Box::new(e) as _)
    }

    fn list_pending_erased(
        &self,
        agent_id: Option<&mempill_types::AgentId>,
    ) -> Result<Vec<crate::ports::pending_adjudication::PendingAdjudicationRow>, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.list_pending(agent_id).map_err(|e| Box::new(e) as _)
    }

    fn list_expired_erased(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::ports::pending_adjudication::PendingAdjudicationRow>, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.list_expired(now).map_err(|e| Box::new(e) as _)
    }

    fn mark_resolved_erased(
        &self,
        handle_id: uuid::Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.mark_resolved(handle_id).map_err(|e| Box::new(e) as _)
    }
}

// ── EngineHandle ──────────────────────────────────────────────────────────────

/// The sole public async entry point for mempill.
///
/// Callers: mempill-py, mempill-node, mempill-mcp, integration tests.
/// Cloneable: all fields are `Arc`-wrapped; clones share the same lock map and port state.
pub struct EngineHandle<P, O, V>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    oracle: Option<Arc<O>>,
    vector: Option<Arc<V>>,
    /// Type-erased pending-adjudication store (W3). `None` when no oracle queue is configured.
    pending_store: Option<Arc<dyn ErasedPendingStore>>,
    config: EngineConfig,
    write_locks: AgentWriteLockMap,
    /// Store-level write lock: serializes ALL writes across agent_ids to prevent
    /// concurrent SQLite transactions from different agents on the same connection.
    /// Reads (query_memory, query_audit) never acquire this lock.
    store_write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl<P, O, V> EngineHandle<P, O, V>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    /// Create an `EngineHandle` without a pending-adjudication store.
    ///
    /// QueuedForAdjudication claims will still be committed with the correct disposition,
    /// but no `pending_adjudications` row will be written. Suitable for tests that don't
    /// exercise oracle queue persistence, and for the `DefaultEngine` alias.
    pub fn new(
        persistence: Arc<P>,
        oracle: Option<Arc<O>>,
        vector: Option<Arc<V>>,
        config: EngineConfig,
    ) -> Self {
        Self {
            persistence,
            oracle,
            vector,
            pending_store: None,
            config,
            write_locks: AgentWriteLockMap::new(),
            store_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Create an `EngineHandle` with a concrete pending-adjudication store (W3).
    ///
    /// The store is type-erased via [`ErasedPendingStoreAdapter`] so `EngineHandle` keeps
    /// its 3-param signature unchanged.
    ///
    /// Typical usage in adapter crates (e.g. mempill-sqlite):
    /// ```rust,ignore
    /// let engine = EngineHandle::new_with_pending_store(
    ///     Arc::new(persistence_store),
    ///     Some(Arc::new(oracle)),
    ///     None::<Arc<NoOpVector>>,
    ///     Arc::new(ErasedPendingStoreAdapter::new(sqlite_pending_store)),
    ///     EngineConfig::default(),
    /// );
    /// ```
    pub fn new_with_pending_store<S>(
        persistence: Arc<P>,
        oracle: Option<Arc<O>>,
        vector: Option<Arc<V>>,
        pending_store: Arc<dyn ErasedPendingStore>,
        config: EngineConfig,
    ) -> Self {
        Self {
            persistence,
            oracle,
            vector,
            pending_store: Some(pending_store),
            config,
            write_locks: AgentWriteLockMap::new(),
            store_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Write path: async, acquires per-agent_id lock, delegates to IngestClaimUseCase.
    ///
    /// Clock is read ONCE here (DETERMINISM): `now` flows into the use-case as a parameter.
    ///
    /// Locking order (must be consistent across all write methods to avoid deadlock):
    ///   1. store_write_lock  — serializes all cross-agent SQLite writes (conditional; Postgres skips)
    ///   2. per-agent lock    — preserves same-agent serial semantics + Postgres compat
    pub async fn ingest_claim(
        &self,
        req: IngestClaimRequest,
    ) -> Result<IngestClaimResponse, MemError> {
        let now = Utc::now(); // clock read ONCE at the async boundary
        // Acquire global write lock only when the adapter requires it (SQLite=yes, Postgres=no).
        let _store_lock = if self.persistence.requires_global_write_serialization() {
            Some(self.store_write_lock.lock().await)
        } else {
            None
        };
        let _guard = self.write_locks.acquire(&req.agent_id).await;
        let uc = IngestClaimUseCase::new(
            Arc::clone(&self.persistence),
            self.oracle.clone(),
            self.pending_store.clone(),
            self.config.clone(),
        );
        task::spawn_blocking(move || uc.execute_with_time(req, now))
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }

    /// Read path: no write lock needed. Delegates to QueryMemoryUseCase.
    ///
    /// Clock read ONCE here; passed into the sync use-case.
    pub async fn query_memory(
        &self,
        req: QueryMemoryRequest,
    ) -> Result<QueryMemoryResponse, MemError> {
        let now = Utc::now();
        let uc = QueryMemoryUseCase::new(
            Arc::clone(&self.persistence),
            self.vector.clone(),
            self.config.clone(),
        );
        task::spawn_blocking(move || uc.execute_with_time(req, now))
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }

    /// Reconcile path: acquires write lock per agent_id in the request.
    ///
    /// Locking order matches ingest_claim: store_write_lock first (conditional), then per-agent lock.
    pub async fn reconcile(
        &self,
        req: ReconcileRequest,
    ) -> Result<ReconcileResponse, MemError> {
        // Acquire global write lock only when the adapter requires it (SQLite=yes, Postgres=no).
        let _store_lock = if self.persistence.requires_global_write_serialization() {
            Some(self.store_write_lock.lock().await)
        } else {
            None
        };
        let _guard = self.write_locks.acquire(&req.agent_id).await;
        let uc = ReconcileUseCase::new(
            Arc::clone(&self.persistence),
            self.oracle.clone(),
            self.config.clone(),
        );
        task::spawn_blocking(move || uc.execute(req))
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }

    /// Audit read path: no write lock.
    pub async fn query_audit(
        &self,
        req: AuditQueryRequest,
    ) -> Result<AuditQueryResponse, MemError> {
        let uc = AuditUseCase::new(Arc::clone(&self.persistence));
        task::spawn_blocking(move || uc.execute(req))
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }
}

impl<P, O, V> Clone for EngineHandle<P, O, V>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            persistence: Arc::clone(&self.persistence),
            oracle: self.oracle.clone(),
            vector: self.vector.clone(),
            pending_store: self.pending_store.clone(),
            config: self.config.clone(),
            write_locks: self.write_locks.clone(),
            store_write_lock: Arc::clone(&self.store_write_lock),
        }
    }
}
