//! `PostgresTxn` — the concrete transaction handle wrapping a pooled Postgres connection (I9, §3).
//!
//! # Design (A39 — own the pooled connection, manual BEGIN/COMMIT/ROLLBACK)
//!
//! `postgres::Client::transaction()` returns `Transaction<'_>` that borrows `&mut Client`.
//! This conflicts with `Txn: Send + 'static`. Resolution (identical to `SqliteTxn`): own the
//! pooled connection outright and issue `BEGIN`/`COMMIT`/`ROLLBACK` via `batch_execute`.
//!
//! `PooledConnection<PostgresConnectionManager<NoTls>>` is `Send` (r2d2 guarantees).
//! `PostgresTxn` is therefore `Send + 'static` without any `unsafe`.
//!
//! # Advisory lock (A40)
//!
//! After `BEGIN`, the first statement is:
//! ```sql
//! SELECT pg_advisory_xact_lock(hashtext($1)::bigint)
//! ```
//! This serializes same-agent_id writes at the DB level. The lock is transaction-scoped:
//! auto-released on COMMIT or ROLLBACK (no leak risk on panic).
//!
//! # `as_deref_mut` verification
//!
//! `r2d2_postgres::PooledConnection<M>` implements `DerefMut<Target = postgres::Client>`.
//! `Option<PooledConnection<M>>::as_deref_mut()` yields `Option<&mut postgres::Client>`.
//! This is used in `client()` and confirmed to compile under r2d2_postgres 0.18.

use r2d2::PooledConnection;
use r2d2_postgres::PostgresConnectionManager;
use postgres::NoTls;
use mempill_core::ports::persistence::Txn;
use mempill_types::identity::AgentId;

use crate::connection::PostgresStoreError;

/// An open, uncommitted Postgres transaction scoped to one `agent_id` (I9, DC-2).
///
/// Created by `PostgresPersistenceStore::begin_atomic`; consumed by `commit` or `rollback`.
/// Owns the pooled connection for the duration of the transaction.
/// The connection returns to the r2d2 pool on `Drop`.
pub struct PostgresTxn {
    agent_id: AgentId,
    /// Pooled connection with an open transaction.
    /// `Option` so we can move it out on commit/rollback without destructuring.
    /// Connection returns to pool when `PooledConnection` is dropped.
    conn: Option<PooledConnection<PostgresConnectionManager<NoTls>>>,
}

// PooledConnection<PostgresConnectionManager<NoTls>>: Send (r2d2 guarantees).
// PostgresTxn therefore: Send + 'static. No unsafe needed.

impl PostgresTxn {
    /// Begin a new transaction. Called exclusively from `PostgresPersistenceStore::begin_atomic`.
    ///
    /// Issues `BEGIN` then acquires the per-agent_id advisory lock (A40).
    pub(crate) fn begin(
        agent_id: AgentId,
        mut conn: PooledConnection<PostgresConnectionManager<NoTls>>,
    ) -> Result<Self, PostgresStoreError> {
        conn.batch_execute("BEGIN")?;
        conn.execute(
            "SELECT pg_advisory_xact_lock(hashtext($1)::bigint)",
            &[&agent_id.0.as_str()],
        )?;
        Ok(Self { agent_id, conn: Some(conn) })
    }

    /// Borrow the inner `postgres::Client` for SQL execution.
    ///
    /// `Option<PooledConnection<M>>::as_deref_mut()` yields `Option<&mut postgres::Client>`
    /// via `DerefMut` on `PooledConnection` (r2d2_postgres 0.18 implements `DerefMut<Target = postgres::Client>`).
    /// This is the W0-T2 verification point: confirmed to compile.
    pub(crate) fn client(&mut self) -> &mut postgres::Client {
        self.conn
            .as_deref_mut()
            .expect("PostgresTxn: connection consumed — cannot call client() after commit/rollback")
    }

    /// COMMIT the transaction. The pooled connection returns to the r2d2 pool on drop.
    pub(crate) fn commit_and_drop(mut self) -> Result<(), PostgresStoreError> {
        let mut conn = self.conn.take().expect("PostgresTxn: connection consumed");
        conn.batch_execute("COMMIT")?;
        // conn drops here → returns to pool via r2d2 PooledConnection Drop impl
        Ok(())
    }

    /// ROLLBACK the transaction. The pooled connection returns to the r2d2 pool on drop.
    pub(crate) fn rollback_and_drop(mut self) -> Result<(), PostgresStoreError> {
        let mut conn = self.conn.take().expect("PostgresTxn: connection consumed");
        conn.batch_execute("ROLLBACK")?;
        // conn drops here → returns to pool
        Ok(())
    }
}

impl Drop for PostgresTxn {
    /// Best-effort ROLLBACK on panic or drop without explicit commit (append-only invariant).
    fn drop(&mut self) {
        if let Some(ref mut conn) = self.conn {
            // Best-effort; ignore error on drop — the open transaction will be
            // rolled back by Postgres when the connection is returned to the pool anyway.
            let _ = conn.batch_execute("ROLLBACK");
            // conn returned to pool after this block via PooledConnection Drop
        }
    }
}

impl Txn for PostgresTxn {
    fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }
}
