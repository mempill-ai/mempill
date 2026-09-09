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
//!
//! ## Migrating a pre-0.4.0 shared-file database
//! Before 0.4.0, the (then-public) `open(path)` function let callers point any number of
//! `agent_id`s at one shared SQLite file. `open_for_agent(base_dir, agent_id)` does not
//! read or migrate such a file automatically — pre-0.4.0 shared-file databases are left
//! untouched on disk. To migrate manually:
//! 1. Open the old shared file with the internal `open(path)` (test/migration-tooling use
//!    only — not part of the public API).
//! 2. For each distinct `agent_id` present in that file's `claims` / `validity_assertions`
//!    / `ledger_entries` / `claim_edges` tables, `SELECT * ... WHERE agent_id = ?` and
//!    `INSERT` the rows into a fresh per-agent file opened via
//!    `open_for_agent(base_dir, agent_id)`.
//! 3. Verify row counts match per table per `agent_id` before deleting the old shared file.
//! No automated migration tool ships in 0.4.0; this is a manual, one-time step for anyone
//! upgrading from a pre-0.4.0 shared-file deployment.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, Result as SqlResult};

use crate::migrations;

/// Bound on retries for the brand-new-file WAL-initialization race handled by [`open`].
/// Linear backoff (`5ms * attempt`) totals well under 1s for the full retry budget, which
/// is generous relative to the sub-millisecond DDL runs this races against.
const OPEN_MAX_ATTEMPTS: u32 = 10;

/// Open a **file-backed** SQLite connection at `path`, apply mandatory PRAGMAs, then run
/// any pending migrations.
///
/// # Concurrent first-open retry
/// SQLite's very first conversion of a brand-new file to `journal_mode=WAL` (the `-wal` /
/// `-shm` sidecar creation) has a documented corner case: a connection reading through a
/// concurrently-initializing WAL file can get `SQLITE_BUSY` WITHOUT that wait going through
/// the normal `sqlite3_busy_timeout` retry loop — verified empirically (busy_timeout alone
/// leaves this failing in well under 1ms, i.e. not retried at all). This is distinct from,
/// and in addition to, the `BEGIN IMMEDIATE` transaction in `migrations::apply_migrations`,
/// which correctly serializes the version-check + DDL + version-bump once a connection has
/// gotten past this initial WAL setup. To make concurrent first opens of a brand-new file
/// reliable end to end, `open` retries the whole open-and-migrate sequence (fresh
/// `Connection`, since a `SQLITE_BUSY` mid-sequence can leave the old one transaction-broken)
/// on `SQLITE_BUSY` specifically, up to [`OPEN_MAX_ATTEMPTS`] times with a short linear
/// backoff, before propagating the error.
///
/// # Visibility
/// This function is intentionally `pub(crate)` (not part of the public API). It accepts
/// an arbitrary path string with no per-agent enforcement, which allows two different
/// `agent_id`s to accidentally share one file — the exact footgun `open_for_agent`
/// (see `mempill-sqlite/src/lib.rs`) was introduced to close structurally. It remains
/// available for internal test use and migration tooling only.
pub(crate) fn open(path: &str) -> Result<Connection, crate::SqliteStoreError> {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match open_once(path) {
            Ok(conn) => return Ok(conn),
            Err(err) if attempt < OPEN_MAX_ATTEMPTS && is_sqlite_busy(&err) => {
                std::thread::sleep(Duration::from_millis(5 * u64::from(attempt)));
            }
            Err(err) => return Err(err),
        }
    }
}

/// One attempt at opening + PRAGMA-initializing + migrating `path`. See [`open`] for the
/// retry wrapper around this.
fn open_once(path: &str) -> Result<Connection, crate::SqliteStoreError> {
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    migrations::apply_migrations(&conn)?;
    Ok(conn)
}

/// True if `err` ultimately wraps a `rusqlite` `SQLITE_BUSY` failure, whether it arrived via
/// `SqliteStoreError::Sqlite` (raw connection/PRAGMA errors) or
/// `SqliteStoreError::Migration` (errors from inside `migrations::apply_migrations`).
fn is_sqlite_busy(err: &crate::SqliteStoreError) -> bool {
    let inner = match err {
        crate::SqliteStoreError::Sqlite(e) => Some(e),
        crate::SqliteStoreError::Migration(migrations::MigrationError::Sqlite(e)) => Some(e),
        _ => None,
    };
    matches!(
        inner,
        Some(rusqlite::Error::SqliteFailure(sqlite_err, _)) if sqlite_err.code == ErrorCode::DatabaseBusy
    )
}

/// Derive the per-agent database file path as `base_dir/agent_{agent_id}.db`.
///
/// Validates that `agent_id` is filesystem-safe so that two distinct `agent_id`s can
/// never normalize to the same on-disk filename. An `agent_id` is accepted only if it is
/// non-empty and contains solely ASCII alphanumeric characters, `-`, or `_` — this rules
/// out path separators (`/`, `\`), `..` traversal, NUL bytes, and any character that a
/// filesystem could collapse or reject, which is what would otherwise let two different
/// `agent_id`s collide on one file.
///
/// # Errors
/// Returns [`crate::SqliteStoreError::InvalidAgentId`] if `agent_id` is empty or contains
/// any character outside `[A-Za-z0-9_-]`.
pub(crate) fn derive_agent_db_path(
    base_dir: &Path,
    agent_id: &str,
) -> Result<PathBuf, crate::SqliteStoreError> {
    if agent_id.is_empty() {
        return Err(crate::SqliteStoreError::InvalidAgentId(
            "agent_id must not be empty".to_string(),
        ));
    }
    if !agent_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(crate::SqliteStoreError::InvalidAgentId(format!(
            "agent_id {agent_id:?} contains characters outside [A-Za-z0-9_-]; \
             this restriction guarantees two distinct agent_ids can never normalize \
             to the same database filename"
        )));
    }
    Ok(base_dir.join(format!("agent_{agent_id}.db")))
}

/// Open the per-agent SQLite database under `base_dir` for `agent_id`.
///
/// This is the production entry point for per-agent_id databases (one file per agent),
/// and the only public way to obtain a file-backed connection. The file path is derived
/// automatically as `base_dir/agent_{agent_id}.db` — there is no way to point two
/// different `agent_id`s at the same file through this API.
///
/// # Errors
/// Returns [`crate::SqliteStoreError::InvalidAgentId`] if `agent_id` contains characters
/// that could cause a filename collision (see [`derive_agent_db_path`]). Returns other
/// [`crate::SqliteStoreError`] variants if the connection cannot be opened or migrations fail.
pub fn open_for_agent(
    base_dir: &Path,
    agent_id: &str,
) -> Result<Connection, crate::SqliteStoreError> {
    let path = derive_agent_db_path(base_dir, agent_id)?;
    // path is always valid UTF-8 derived input joined onto a caller-supplied base_dir;
    // fall back to a lossy conversion only in the pathological case of a non-UTF8 base_dir.
    let path_str = path.to_str().ok_or_else(|| {
        crate::SqliteStoreError::InvalidAgentId(format!(
            "derived database path {path:?} is not valid UTF-8"
        ))
    })?;
    open(path_str)
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
    // Explicit, self-documenting busy timeout (rusqlite already sets 5000ms by default on
    // every new connection, but we pin it here so migrations.rs's BEGIN IMMEDIATE race
    // window is guaranteed to have somewhere to wait, independent of rusqlite's default).
    // A second connection racing the first open_for_agent() of a brand-new file blocks here
    // (inside SQLite's busy handler) rather than failing outright with SQLITE_BUSY.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
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

    // ── open_for_agent / derive_agent_db_path ───────────────────────────────────

    #[test]
    fn open_for_agent_creates_one_file_per_agent_under_base_dir() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let conn = open_for_agent(dir.path(), "agent-a")
            .expect("open_for_agent should succeed for a valid agent_id");
        drop(conn);

        let expected = dir.path().join("agent_agent-a.db");
        assert!(
            expected.exists(),
            "expected per-agent db file at {expected:?}"
        );

        let _ = fs::remove_file(dir.path().join("agent_agent-a.db-wal"));
        let _ = fs::remove_file(dir.path().join("agent_agent-a.db-shm"));
    }

    #[test]
    fn open_for_agent_gives_two_different_agent_ids_two_different_files() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let conn_a = open_for_agent(dir.path(), "agent-a")
            .expect("open_for_agent(agent-a) should succeed");
        let conn_b = open_for_agent(dir.path(), "agent-b")
            .expect("open_for_agent(agent-b) should succeed");
        drop(conn_a);
        drop(conn_b);

        let path_a = dir.path().join("agent_agent-a.db");
        let path_b = dir.path().join("agent_agent-b.db");
        assert!(path_a.exists(), "agent-a file must exist");
        assert!(path_b.exists(), "agent-b file must exist");
        assert_ne!(path_a, path_b, "different agent_ids must map to different files");

        for suffix in ["-wal", "-shm"] {
            let _ = fs::remove_file(dir.path().join(format!("agent_agent-a.db{suffix}")));
            let _ = fs::remove_file(dir.path().join(format!("agent_agent-b.db{suffix}")));
        }
    }

    #[test]
    fn derive_agent_db_path_rejects_empty_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let err = derive_agent_db_path(dir.path(), "")
            .expect_err("empty agent_id must be rejected");
        assert!(matches!(err, crate::SqliteStoreError::InvalidAgentId(_)));
    }

    #[test]
    fn derive_agent_db_path_rejects_path_separator_in_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        // "../evil" and "a/b" must be rejected — both could otherwise let a
        // maliciously or accidentally crafted agent_id escape base_dir or collide
        // with another agent's file.
        let err = derive_agent_db_path(dir.path(), "../evil")
            .expect_err("agent_id with path separators must be rejected");
        assert!(matches!(err, crate::SqliteStoreError::InvalidAgentId(_)));

        let err = derive_agent_db_path(dir.path(), "a/b")
            .expect_err("agent_id with a forward slash must be rejected");
        assert!(matches!(err, crate::SqliteStoreError::InvalidAgentId(_)));
    }

    #[test]
    fn derive_agent_db_path_two_agent_ids_that_would_collide_are_rejected_not_silently_merged() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        // Two agent_ids that would otherwise normalize to the same filename if slashes
        // were allowed (e.g. "a/b" and "a-b" both naively suggesting "agent_a-b.db")
        // must not silently collide: "a/b" is rejected outright by validation, so the
        // only way to get "agent_a-b.db" is the single valid agent_id "a-b".
        let rejected = derive_agent_db_path(dir.path(), "a/b");
        assert!(rejected.is_err(), "path-unsafe agent_id must fail loudly");

        let accepted = derive_agent_db_path(dir.path(), "a-b")
            .expect("agent_id with only [A-Za-z0-9_-] must be accepted");
        assert_eq!(accepted, dir.path().join("agent_a-b.db"));
    }

    #[test]
    fn derive_agent_db_path_accepts_alphanumeric_dash_underscore() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let path = derive_agent_db_path(dir.path(), "Agent_123-test")
            .expect("alphanumeric + dash + underscore agent_id must be accepted");
        assert_eq!(path, dir.path().join("agent_Agent_123-test.db"));
    }
}
