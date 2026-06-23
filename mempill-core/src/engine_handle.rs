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
    ports::{OraclePort, PersistencePort, VectorPort},
};

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
    ///   1. store_write_lock  — serializes all cross-agent SQLite writes
    ///   2. per-agent lock    — preserves same-agent serial semantics + future-Postgres compat
    pub async fn ingest_claim(
        &self,
        req: IngestClaimRequest,
    ) -> Result<IngestClaimResponse, MemError> {
        let now = Utc::now(); // clock read ONCE at the async boundary
        let _store_lock = self.store_write_lock.lock().await;
        let _guard = self.write_locks.acquire(&req.agent_id).await;
        let uc = IngestClaimUseCase::new(
            Arc::clone(&self.persistence),
            self.oracle.clone(),
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
    /// Locking order matches ingest_claim: store_write_lock first, then per-agent lock.
    pub async fn reconcile(
        &self,
        req: ReconcileRequest,
    ) -> Result<ReconcileResponse, MemError> {
        let _store_lock = self.store_write_lock.lock().await;
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
            config: self.config.clone(),
            write_locks: self.write_locks.clone(),
            store_write_lock: Arc::clone(&self.store_write_lock),
        }
    }
}
