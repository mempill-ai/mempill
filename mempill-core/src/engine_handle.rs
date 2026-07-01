//! EngineHandle — the sole public async entry point for mempill.
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
//! # Pending-adjudication port
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
            QueryHistoryRequest, QueryHistoryResponse, QueryMemoryRequest, QueryMemoryResponse,
            QuerySubjectRequest, QuerySubjectResponse,
            ReconcileRequest, ReconcileResponse,
        },
        ingest_claim::IngestClaimUseCase,
        query_history::QueryHistoryUseCase,
        query_memory::QueryMemoryUseCase,
        query_subject::QuerySubjectUseCase,
        reconcile::ReconcileUseCase,
        submit_adjudication::SubmitAdjudicationUseCase,
        sweep_adjudications::SweepAdjudicationsUseCase,
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
#[allow(missing_docs)]
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

    fn mark_expired_erased(
        &self,
        handle_id: uuid::Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>;

    fn list_queued_orphan_claims_erased(
        &self,
    ) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, Box<dyn std::error::Error + Send + Sync + 'static>>;
}

/// Adapter that wraps a concrete `PendingAdjudicationPort` impl as `dyn ErasedPendingStore`.
pub struct ErasedPendingStoreAdapter<S: PendingAdjudicationPort> {
    inner: S,
}

impl<S: PendingAdjudicationPort> ErasedPendingStoreAdapter<S> {
    /// Wrap a concrete `PendingAdjudicationPort` impl as `dyn ErasedPendingStore`.
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

    fn mark_expired_erased(
        &self,
        handle_id: uuid::Uuid,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.mark_expired(handle_id).map_err(|e| Box::new(e) as _)
    }

    fn list_queued_orphan_claims_erased(
        &self,
    ) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, Box<dyn std::error::Error + Send + Sync + 'static>> {
        self.inner.list_queued_orphan_claims().map_err(|e| Box::new(e) as _)
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
    /// Type-erased pending-adjudication store. `None` when no oracle queue is configured.
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

    /// Create an `EngineHandle` with a concrete pending-adjudication store.
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

    /// Subject read path: returns all resolved beliefs for every predicate under a subject.
    ///
    /// Read-only (no write lock). Reuses the existing QueryMemory fold per predicate.
    /// Clock is read ONCE here (DETERMINISM).
    pub async fn query_subject(
        &self,
        req: QuerySubjectRequest,
    ) -> Result<QuerySubjectResponse, MemError> {
        let now = Utc::now();
        let uc = QuerySubjectUseCase::new(
            Arc::clone(&self.persistence),
            self.vector.clone(),
            self.config.clone(),
        );
        task::spawn_blocking(move || uc.execute_with_time(req, now))
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }

    /// History read path: no write lock needed. Delegates to QueryHistoryUseCase.
    ///
    /// Returns the full ordered timeline for a (subject, predicate) subject-line.
    /// Each entry is tagged `Current` or `Superseded` using the same canonical fold
    /// as `query_memory` — so `history.current().value == recall primary value`.
    ///
    /// Clock read ONCE here; passed into the sync use-case (DETERMINISM).
    pub async fn query_history(
        &self,
        req: QueryHistoryRequest,
    ) -> Result<QueryHistoryResponse, MemError> {
        let now = Utc::now();
        let uc = QueryHistoryUseCase::new(
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

    /// Oracle resolution path: deliver an oracle verdict and apply it atomically.
    ///
    /// Acquires locks in the SAME ORDER as `ingest_claim` to prevent deadlock:
    ///   1. `store_write_lock`  — serializes all cross-agent SQLite writes (conditional).
    ///   2. per-agent lock      — keyed on the `agent_id` retrieved from the pending row.
    ///
    /// # Postgres / async-runtime safety
    ///
    /// The postgres sync crate (`postgres 0.19`) wraps `tokio-postgres` and calls `block_on`
    /// in `Client::drop`. Dropping a postgres `Client` while a tokio runtime is active on the
    /// current thread panics with "Cannot start a runtime from within a runtime".
    ///
    /// ALL pending-store I/O (including the agent_id resolution read) is therefore performed
    /// inside `spawn_blocking` so no `postgres::Client` is ever created or dropped on the
    /// async executor thread. This is the same discipline used by `ingest_claim`.
    ///
    /// # Protocol
    ///
    /// 1. `spawn_blocking` — resolve `agent_id` from the pending row (DB read, safe).
    /// 2. Acquire `store_write_lock` (SQLite-only) + per-agent write lock (async).
    /// 3. `spawn_blocking` — run `SubmitAdjudicationUseCase::execute` (all DB writes).
    ///
    /// # Errors
    ///
    /// - `MemError::AdjudicationHandleNotFound` — handle unknown, expired, or stale.
    /// - `MemError::PendingStore` — pending-store I/O error.
    /// - `MemError::Persistence` — DB write error during verdict apply.
    /// - `MemError::SpawnBlocking` — tokio task join error.
    pub async fn submit_adjudication(
        &self,
        handle_id: uuid::Uuid,
        response: mempill_types::AdjudicationResponse,
    ) -> Result<mempill_types::AdjudicationOutcome, MemError> {
        let now = Utc::now(); // clock read ONCE at the async boundary (DETERMINISM)

        // ── Step 1: Resolve agent_id via spawn_blocking (NO async-context DB access) ──
        //
        // The postgres sync crate calls `block_on` in `Client::drop`. Reading the pending
        // store directly in the async context would drop a postgres connection on the tokio
        // thread, causing a panic. `spawn_blocking` moves the drop to a dedicated OS thread
        // where no tokio runtime is active. The use-case re-reads the row inside its own
        // spawn_blocking (Step 3) for the authoritative state-guard.
        let pending_store = self.pending_store.as_ref()
            .ok_or(MemError::AdjudicationHandleNotFound { handle_id })?;
        let pending_store_arc = Arc::clone(pending_store);

        let resolve_result = task::spawn_blocking(move || {
            let row = pending_store_arc
                .get_pending_erased(handle_id)
                .map_err(|e| MemError::PendingStore { source: e })?
                .ok_or(MemError::AdjudicationHandleNotFound { handle_id })?;
            Ok::<_, MemError>(row)
        })
        .await
        .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })??;

        let row = resolve_result;

        // Note: TTL expiry is handled authoritatively inside SubmitAdjudicationUseCase
        // (which also writes the AdjudicationExpired ledger entry). Do NOT early-reject
        // here — the use-case must run so the audit trail is complete.
        let agent_id = row.agent_id.clone();

        // ── Step 2: Acquire locks in the same order as ingest_claim ──────────────
        let _store_lock = if self.persistence.requires_global_write_serialization() {
            Some(self.store_write_lock.lock().await)
        } else {
            None
        };
        let _guard = self.write_locks.acquire(&agent_id).await;

        // ── Step 3: Dispatch to sync use-case via spawn_blocking ─────────────────
        let pending_store_arc2 = Arc::clone(pending_store);
        let uc = SubmitAdjudicationUseCase::new(
            Arc::clone(&self.persistence),
            pending_store_arc2,
        );
        task::spawn_blocking(move || uc.execute(handle_id, response, now))
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }

    /// Read path: list all pending-adjudication rows for an agent (or all agents).
    ///
    /// This is a read-only operation — no write lock is acquired.  All DB access
    /// is performed inside `spawn_blocking` so no `postgres::Client` is created or
    /// dropped on the async executor thread (same invariant as `submit_adjudication`).
    ///
    /// Returns `Ok(vec![])` when no pending store is configured.
    pub async fn list_pending_adjudications(
        &self,
        agent_id: Option<mempill_types::AgentId>,
    ) -> Result<Vec<crate::ports::pending_adjudication::PendingAdjudicationRow>, MemError> {
        let pending_store = match &self.pending_store {
            Some(ps) => Arc::clone(ps),
            None => return Ok(vec![]),
        };

        task::spawn_blocking(move || {
            pending_store
                .list_pending_erased(agent_id.as_ref())
                .map_err(|e| MemError::PendingStore { source: e })
        })
        .await
        .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })?
    }

    /// Sweep all expired pending-adjudication rows and orphaned QueuedForAdjudication claims.
    ///
    /// For each expired pending row (expires_at <= now):
    ///   1. Acquires store_write_lock + per-agent write lock (same order as ingest_claim).
    ///   2. Atomically reverts the challenger QueuedForAdjudication → Contested + ledger entry.
    ///   3. Marks the pending row expired.
    ///
    /// Then sweeps orphan claims (QueuedForAdjudication with no pending row):
    ///   4. Per orphan: acquires locks, reverts challenger → Contested + ledger entry.
    ///
    /// Returns the total count of claims reverted (expired + orphan).
    ///
    /// The engine MUST NOT spawn a background task — the host calls this on its own schedule.
    ///
    /// If no pending store is configured, returns `Ok(0)` (sweep is a no-op without oracle queue).
    ///
    /// # Postgres / async-runtime safety
    ///
    /// ALL pending-store reads (`list_expired`, `list_queued_orphan_claims`) are performed
    /// inside `spawn_blocking` so no `postgres::Client` is created or dropped on the tokio
    /// executor thread (same invariant as `submit_adjudication`).
    pub async fn sweep_expired_adjudications(&self) -> Result<usize, MemError> {
        let now = Utc::now();

        let pending_store = match &self.pending_store {
            Some(ps) => Arc::clone(ps),
            None => return Ok(0),
        };

        // ── Phase 1: Collect expired rows via spawn_blocking (NO async-context DB access) ──
        let ps_for_list = Arc::clone(&pending_store);
        let expired_rows = task::spawn_blocking(move || {
            ps_for_list
                .list_expired_erased(now)
                .map_err(|e| MemError::PendingStore { source: e })
        })
        .await
        .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })??;

        let mut swept = 0usize;

        for row in expired_rows {
            let agent_id = row.agent_id.clone();

            let _store_lock = if self.persistence.requires_global_write_serialization() {
                Some(self.store_write_lock.lock().await)
            } else {
                None
            };
            let _guard = self.write_locks.acquire(&agent_id).await;

            let persistence = Arc::clone(&self.persistence);
            let ps = Arc::clone(&pending_store);
            let row_clone = row.clone();

            let result = task::spawn_blocking(move || {
                let uc = SweepAdjudicationsUseCase::new(persistence, ps);
                uc.revert_expired_row(&row_clone, now)
            })
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })??;

            if result {
                swept += 1;
            }
        }

        // ── Phase 2: Collect orphans via spawn_blocking (NO async-context DB access) ──
        // Detect QueuedForAdjudication claims with no matching pending row.
        let ps_for_orphans = Arc::clone(&pending_store);
        let orphans = task::spawn_blocking(move || {
            ps_for_orphans
                .list_queued_orphan_claims_erased()
                .map_err(|e| MemError::PendingStore { source: e })
        })
        .await
        .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })??;

        for orphan in orphans {
            let agent_id = orphan.agent_id.clone();

            let _store_lock = if self.persistence.requires_global_write_serialization() {
                Some(self.store_write_lock.lock().await)
            } else {
                None
            };
            let _guard = self.write_locks.acquire(&agent_id).await;

            let persistence = Arc::clone(&self.persistence);
            let ps = Arc::clone(&pending_store);
            let orphan_clone = orphan.clone();

            let result = task::spawn_blocking(move || {
                let uc = SweepAdjudicationsUseCase::new(persistence, ps);
                uc.revert_orphan(&orphan_clone, now)
            })
            .await
            .map_err(|e| MemError::SpawnBlocking { reason: e.to_string() })??;

            if result {
                swept += 1;
            }
        }

        Ok(swept)
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
