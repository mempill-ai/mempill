//! Connection lifecycle for mempill-sqlite.
//!
//! Every connection — whether backed by a file or opened in-memory — MUST have the
//! mandatory PRAGMAs applied **before** migrations run or any write is served.
//!
//! # PRAGMA contract
//!
//! ```sql
//! PRAGMA journal_mode = WAL;      -- WAL for concurrent reads during writes
//! PRAGMA synchronous  = FULL;     -- full-durability write path (WAL+NORMAL can lose writes on power loss)
//! PRAGMA foreign_keys = ON;       -- enforce FK constraints from v1_initial.sql
//! ```
//!
//! ## In-memory WAL caveat
//! SQLite silently downgrades `journal_mode` to `memory` for `:memory:` connections
//! because WAL requires a real file (it writes a `-wal` and `-shm` sidecar).
//! This is expected and documented behaviour. The durability guarantees (`synchronous=FULL`
//! and `foreign_keys=ON`) are still applied and tested for in-memory connections.
//! WAL mode is tested separately against a temporary file-backed database.

use rusqlite::{Connection, Result as SqlResult};

use crate::migrations;

/// Open a **file-backed** SQLite connection at `path`, apply mandatory PRAGMAs, then run
/// any pending migrations.
///
/// This is the production path for per-agent_id databases (one file per agent).
pub fn open(path: &str) -> Result<Connection, crate::SqliteStoreError> {
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    migrations::apply_migrations(&conn)?;
    Ok(conn)
}

/// Open an **in-memory** SQLite connection, apply mandatory PRAGMAs (except WAL — see
/// module-level caveat), then run migrations.
///
/// Used for tests and ephemeral engine contexts.
pub fn open_in_memory() -> Result<Connection, crate::SqliteStoreError> {
    let conn = Connection::open_in_memory()?;
    apply_pragmas(&conn)?;
    migrations::apply_migrations(&conn)?;
    Ok(conn)
}

/// Apply the mandatory connection-level PRAGMAs.
///
/// Must be called before any DDL or DML on a freshly-opened connection.
/// The order matters: `foreign_keys=ON` must precede any INSERT that references FKs.
fn apply_pragmas(conn: &Connection) -> SqlResult<()> {
    // journal_mode returns the active mode as a result row; we discard it.
    // For :memory: connections SQLite returns "memory" instead of "wal" — expected.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\
         PRAGMA synchronous  = FULL;\
         PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Query a single-column, single-row PRAGMA and return the string value.
    fn pragma_str(conn: &Connection, pragma: &str) -> String {
        conn.query_row(
            &format!("PRAGMA {pragma}"),
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| String::new())
    }

    /// Query a PRAGMA that returns an integer.
    fn pragma_int(conn: &Connection, pragma: &str) -> i64 {
        conn.query_row(
            &format!("PRAGMA {pragma}"),
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(-1)
    }

    // ── in-memory PRAGMA tests ────────────────────────────────────────────────

    /// `synchronous=FULL` corresponds to integer value 2 in SQLite.
    /// SQLite synchronous levels: 0=OFF, 1=NORMAL, 2=FULL, 3=EXTRA.
    #[test]
    fn in_memory_synchronous_is_full() {
        let conn = open_in_memory().expect("in-memory open should succeed");
        let sync_val = pragma_int(&conn, "synchronous");
        assert_eq!(sync_val, 2, "synchronous must be FULL (2) on in-memory connection");
    }

    #[test]
    fn in_memory_foreign_keys_is_on() {
        let conn = open_in_memory().expect("in-memory open should succeed");
        let fk_val = pragma_int(&conn, "foreign_keys");
        assert_eq!(fk_val, 1, "foreign_keys must be ON (1) on in-memory connection");
    }

    /// WAL is not possible on :memory: — SQLite returns "memory". Document + assert.
    #[test]
    fn in_memory_journal_mode_is_memory_not_wal() {
        let conn = open_in_memory().expect("in-memory open should succeed");
        let mode = pragma_str(&conn, "journal_mode");
        // "memory" is the expected value; "wal" is structurally impossible for :memory:.
        // This test documents the known caveat (see module-level doc).
        assert_eq!(
            mode, "memory",
            "in-memory SQLite must use journal_mode=memory (WAL not supported on :memory:)"
        );
    }

    // ── file-backed PRAGMA tests (WAL) ────────────────────────────────────────

    #[test]
    fn file_backed_journal_mode_is_wal() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let path = dir.path().join("test_wal.db");
        let path_str = path.to_str().unwrap();

        let conn = open(path_str).expect("file-backed open should succeed");
        // After PRAGMA journal_mode=WAL, rusqlite executes it and SQLite returns the new mode.
        // We need to re-query it because execute_batch discards the result row.
        let mode = pragma_str(&conn, "journal_mode");
        assert_eq!(mode, "wal", "file-backed connection must be WAL");

        drop(conn);
        // Clean up WAL sidecar files.
        let _ = fs::remove_file(path.with_extension("db-wal"));
        let _ = fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn file_backed_synchronous_is_full() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let path = dir.path().join("test_sync.db");
        let conn = open(path.to_str().unwrap()).expect("file-backed open should succeed");
        let sync_val = pragma_int(&conn, "synchronous");
        // SQLite synchronous levels: 0=OFF, 1=NORMAL, 2=FULL, 3=EXTRA.
        assert_eq!(sync_val, 2, "synchronous must be FULL (2) on file-backed connection");
    }

    #[test]
    fn file_backed_foreign_keys_is_on() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let path = dir.path().join("test_fk.db");
        let conn = open(path.to_str().unwrap()).expect("file-backed open should succeed");
        let fk_val = pragma_int(&conn, "foreign_keys");
        assert_eq!(fk_val, 1, "foreign_keys must be ON (1) on file-backed connection");
    }

    // ── migration applied by connection constructor ────────────────────────────

    #[test]
    fn open_in_memory_runs_migrations() {
        let conn = open_in_memory().expect("in-memory open should succeed");
        // The claims table must exist after construction — migrations ran automatically.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='claims'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "claims table must exist after open_in_memory");
    }
}
