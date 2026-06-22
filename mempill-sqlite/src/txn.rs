//! `SqliteTxn` — the concrete transaction handle wrapping a rusqlite connection (I9, §4).
//!
//! # Design (A18 — explicit transaction control)
//!
//! rusqlite's `Transaction<'conn>` is lifetime-bound to `&mut Connection`, which conflicts
//! with the `Txn: Send + 'static` bound required by the port trait (§4) and `spawn_blocking`.
//!
//! Resolution: `SqliteTxn` owns the `Connection` outright (moved out of the `Arc<Mutex<…>>`
//! in `begin_atomic`). The `SqlitePersistenceStore` uses an `Option<Arc<Mutex<Connection>>>`
//! internally; `begin_atomic` takes the connection out for the duration of the txn and returns
//! it on `commit`/`rollback`.  Because `Connection: Send` (rusqlite guarantees this), the
//! owned `SqliteTxn` is `Send + 'static`.
//!
//! A simpler, more robust alternative that avoids unsafe code: use a boxed `Connection`
//! with an explicit `BEGIN`/`COMMIT`/`ROLLBACK` sequence rather than rusqlite's
//! `Transaction` type.  This is the approach taken here.
//!
//! # DC-2 — single-writer per agent_id
//!
//! v0.1 is single-process embedded. The `AgentWriteLockMap` in mempill-core coordinates
//! writes per agent_id at the async boundary. The store itself assumes a single writer per
//! connection file; no additional locking is needed inside `SqliteTxn`.

use mempill_core::ports::persistence::Txn;
use mempill_types::identity::AgentId;
use rusqlite::Connection;

use crate::SqliteStoreError;

/// An open, uncommitted SQLite transaction scoped to one `agent_id` (I9, DC-2).
///
/// Created by `SqlitePersistenceStore::begin_atomic`; consumed by `commit` or `rollback`.
/// Owns the `Connection` for the lifetime of the transaction — the store re-acquires it
/// after `commit` or `rollback` completes.
pub struct SqliteTxn {
    agent_id: AgentId,
    /// The connection with an open `BEGIN DEFERRED` transaction.
    /// `Option` so we can move it out on commit/rollback without destructuring.
    conn: Option<Box<Connection>>,
}

// rusqlite::Connection is Send; SqliteTxn owns it exclusively.
// SAFETY guaranteed by the type system: Box<Connection>: Send.
unsafe impl Send for SqliteTxn {}

impl SqliteTxn {
    /// Begin a new transaction.  Called exclusively from `SqlitePersistenceStore::begin_atomic`.
    pub(crate) fn begin(
        agent_id: AgentId,
        conn: Box<Connection>,
    ) -> Result<Self, SqliteStoreError> {
        conn.execute_batch("BEGIN DEFERRED")?;
        Ok(Self { agent_id, conn: Some(conn) })
    }

    /// Borrow the inner connection to execute SQL (INSERT, etc.).
    pub(crate) fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("SqliteTxn: connection must be present (not yet consumed)")
    }

    /// COMMIT the transaction and return the owned connection to the caller.
    pub(crate) fn commit_and_return(mut self) -> Result<Box<Connection>, SqliteStoreError> {
        let conn = self.conn.take().expect("SqliteTxn: connection must be present");
        conn.execute_batch("COMMIT")?;
        Ok(conn)
    }

    /// ROLLBACK the transaction and return the owned connection to the caller.
    pub(crate) fn rollback_and_return(mut self) -> Result<Box<Connection>, SqliteStoreError> {
        let conn = self.conn.take().expect("SqliteTxn: connection must be present");
        conn.execute_batch("ROLLBACK")?;
        Ok(conn)
    }
}

impl Drop for SqliteTxn {
    /// If `SqliteTxn` is dropped without an explicit commit or rollback (e.g. on panic),
    /// the `Box<Connection>` is dropped here.  SQLite automatically rolls back any open
    /// transaction when the connection is closed — the append-only invariant is preserved.
    fn drop(&mut self) {
        if let Some(ref conn) = self.conn {
            // Best-effort ROLLBACK; ignore error on drop.
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
}

impl Txn for SqliteTxn {
    fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }
}
