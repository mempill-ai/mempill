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

use rusqlite::{Connection, Result as SqlResult};

use crate::migrations;

/// Open a **file-backed** SQLite connection at `path`, apply mandatory PRAGMAs, then run
/// any pending migrations.
///
/// # Visibility
/// This function is intentionally `pub(crate)` (not part of the public API). It accepts
/// an arbitrary path string with no per-agent enforcement, which allows two different
/// `agent_id`s to accidentally share one file — the exact footgun `open_for_agent`
/// (see `mempill-sqlite/src/lib.rs`) was introduced to close structurally. It remains
/// available for internal test use and migration tooling only.
pub(crate) fn open(path: &str) -> Result<Connection, crate::SqliteStoreError> {
    let conn = Connection::open(path)?;
    apply_pragmas(&conn)?;
    migrations::apply_migrations(&conn)?;
    Ok(conn)
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

    // ── TASK-33 / QA-A (🟡7): open_for_agent edges ──────────────────────────────

    /// (c) A whitespace-only agent_id must be rejected — it is not
    /// `[A-Za-z0-9_-]`, so it is already caught by the existing character-class
    /// validation. This test documents/locks in that coverage explicitly.
    #[test]
    fn open_for_agent_rejects_whitespace_only_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let err = open_for_agent(dir.path(), " ")
            .expect_err("whitespace-only agent_id must be rejected");
        assert!(
            matches!(err, crate::SqliteStoreError::InvalidAgentId(_)),
            "expected InvalidAgentId for whitespace-only agent_id, got {err:?}"
        );

        let err2 = derive_agent_db_path(dir.path(), "   ")
            .expect_err("multi-space agent_id must also be rejected");
        assert!(matches!(err2, crate::SqliteStoreError::InvalidAgentId(_)));
    }

    /// (b) A pre-0.4.0 legacy shared-file database placed in `base_dir` under an
    /// arbitrary filename (e.g. `showcase.db`) must be completely ignored by
    /// `open_for_agent` — it creates (and only ever touches) the derived
    /// `agent_{agent_id}.db` file, never the legacy file.
    #[test]
    fn open_for_agent_ignores_legacy_pre_0_4_0_shared_file() {
        use crate::store::SqlitePersistenceStore;
        use mempill_core::ports::persistence::PersistencePort;
        use mempill_types::{
            claim::{Cardinality, Claim, Confidence, Criticality, Fact},
            identity::{AgentId, ClaimRef},
            provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
            time::{TransactionTime, ValidTime},
        };

        let dir = tempfile::tempdir().expect("tempdir should create");
        let legacy_path = dir.path().join("showcase.db");

        // Build a legacy shared-file DB via the internal (test-only) `open()` and write
        // one claim into it directly, simulating a pre-0.4.0 deployment.
        let legacy_conn = open(legacy_path.to_str().unwrap())
            .expect("legacy shared-file open must succeed (uses the same internal path)");
        let legacy_store = SqlitePersistenceStore::new(legacy_conn);
        let legacy_agent = AgentId("legacy-shared-agent".into());
        let legacy_claim = Claim::new(
            ClaimRef::new_random(),
            legacy_agent.clone(),
            Fact { subject: "legacy".into(), predicate: "marker".into(), value: serde_json::json!("untouched") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(chrono::Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        );
        let mut txn = legacy_store.begin_atomic(&legacy_agent).expect("legacy begin_atomic");
        legacy_store.append_claim(&mut txn, &legacy_claim).expect("legacy append_claim");
        legacy_store.commit(txn).expect("legacy commit");
        drop(legacy_store);

        let legacy_size_before = fs::metadata(&legacy_path).expect("legacy file must exist").len();

        // Now open a per-agent database in the SAME base_dir under a DIFFERENT agent_id.
        let per_agent_conn = open_for_agent(dir.path(), "modern-agent")
            .expect("open_for_agent must succeed alongside an unrelated legacy file");
        let per_agent_store = SqlitePersistenceStore::new(per_agent_conn);

        let expected_per_agent_path = dir.path().join("agent_modern-agent.db");
        assert!(expected_per_agent_path.exists(), "open_for_agent must create its own derived file");
        assert_ne!(expected_per_agent_path, legacy_path, "the derived per-agent path must never equal the legacy shared-file path");

        // The legacy file must be byte-for-byte untouched (size unchanged — no writes) and
        // still contain exactly the one claim written above (readable, not corrupted).
        let legacy_size_after = fs::metadata(&legacy_path).expect("legacy file must still exist").len();
        assert_eq!(legacy_size_after, legacy_size_before, "open_for_agent must not modify the legacy shared-file's size");

        let reopened_legacy = open(legacy_path.to_str().unwrap()).expect("legacy file must still be a valid, openable SQLite DB");
        let reopened_legacy_store = SqlitePersistenceStore::new(reopened_legacy);
        let loaded = reopened_legacy_store
            .load_claim(&legacy_agent, legacy_claim.claim_ref())
            .expect("legacy load_claim must not error")
            .expect("legacy claim must still be present, untouched");
        assert_eq!(loaded.fact().value, serde_json::json!("untouched"));

        // The new per-agent file must start with ZERO claims — open_for_agent must never
        // read/migrate rows FROM the legacy file into the new per-agent file.
        let per_agent_claims = per_agent_store
            .load_subject_line(&AgentId("modern-agent".into()), "legacy", "marker", None)
            .expect("per-agent load_subject_line must not error");
        assert!(per_agent_claims.is_empty(), "open_for_agent must never silently migrate legacy rows into the new per-agent file");

        let _ = fs::remove_file(dir.path().join("agent_modern-agent.db-wal"));
        let _ = fs::remove_file(dir.path().join("agent_modern-agent.db-shm"));
        let _ = fs::remove_file(legacy_path.with_extension("db-wal"));
        let _ = fs::remove_file(legacy_path.with_extension("db-shm"));
    }

    /// KNOWN DEFECT (found by this QA pass, NOT fixed here — no production-code changes
    /// permitted for this test module): `open_for_agent` (and the underlying `open` /
    /// `apply_migrations`) is NOT safe to call concurrently from two threads/processes
    /// against the SAME brand-new (never-before-migrated) file.
    ///
    /// `migrations::apply_migrations` reads `PRAGMA user_version` (TOCTOU check), then
    /// applies DDL, then writes the new `user_version` AFTER commit (see migrations.rs
    /// module docs — this ordering is intentional for crash-safety, but is NOT safe
    /// against a second concurrent connection racing the SAME read-before-write window).
    /// Two threads opening the SAME fresh file concurrently can both observe
    /// `user_version=0`, both run `ALTER TABLE ... ADD COLUMN` from `v3_date_granularity`,
    /// and the second application fails with `"duplicate column name"` — surfaced here as
    /// an `open_for_agent` error, not a panic/corruption, but it DOES mean a caller that
    /// races two first-opens of the same never-before-seen agent_id can get a spurious
    /// error instead of two usable handles.
    ///
    /// `#[ignore]`d (not part of the default green run) — this documents/reproduces the
    /// defect for the maintainers; it is a migration-bootstrap concurrency bug, tracked
    /// separately from the (passing) steady-state concurrent-open test below.
    #[test]
    #[ignore = "KNOWN DEFECT: apply_migrations has a TOCTOU race on user_version when two                 threads race the FIRST open_for_agent of a brand-new per-agent file —                 second connection's DDL can fail with 'duplicate column name'. See doc                 comment. Requires a production-code fix (out of scope for this test-only PR)."]
    fn open_for_agent_concurrent_first_open_migration_race_is_unsafe() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let dir_path: std::path::PathBuf = dir.path().to_path_buf();
        let agent_id = "first-open-race-agent";

        let spawn_first_opener = |dir_path: std::path::PathBuf| {
            std::thread::spawn(move || open_for_agent(&dir_path, agent_id).map(|_conn| ()))
        };
        let h1 = spawn_first_opener(dir_path.clone());
        let h2 = spawn_first_opener(dir_path.clone());
        let r1 = h1.join().expect("thread 1 must not panic");
        let r2 = h2.join().expect("thread 2 must not panic");

        // Documents the CURRENT (defective) behavior: at least one side fails. If this
        // assertion ever fails (i.e. both succeed), the underlying race has been fixed
        // upstream — remove the #[ignore] and this comment.
        assert!(
            r1.is_err() || r2.is_err(),
            "expected the known migration TOCTOU race to surface as an error on at least              one concurrent first-open; both succeeded — the defect may be fixed, please              remove #[ignore] from this test"
        );

        let _ = fs::remove_file(dir_path.join(format!("agent_{agent_id}.db-wal")));
        let _ = fs::remove_file(dir_path.join(format!("agent_{agent_id}.db-shm")));
    }

    /// (a) Concurrent `open_for_agent` calls for the SAME `agent_id` from two OS threads,
    /// against an ALREADY-migrated per-agent file (the realistic steady-state scenario —
    /// see the `#[ignore]`d test above for the separate first-open migration-race defect).
    /// Each thread opens an independent `Connection` to the SAME on-disk file; both
    /// handles must remain usable; concurrent writes must serialize correctly (SQLite
    /// file-level locking under WAL) with no corruption — final row count must equal the
    /// sum of both threads' writes, and every written claim must be individually readable
    /// back.
    #[test]
    fn open_for_agent_same_agent_id_concurrent_open_from_two_threads_no_corruption() {
        use crate::store::SqlitePersistenceStore;
        use mempill_core::ports::persistence::PersistencePort;
        use mempill_types::{
            claim::{Cardinality, Claim, Confidence, Criticality, Fact},
            identity::{AgentId, ClaimRef},
            provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
            time::{TransactionTime, ValidTime},
        };
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir should create");
        let dir_path: std::path::PathBuf = dir.path().to_path_buf();
        let agent_id = "concurrent-shared-agent";
        const WRITES_PER_THREAD: usize = 15;

        // Pre-warm: run the one-time migration bootstrap SINGLE-THREADED first (avoids
        // the separate, known migration-bootstrap race documented in the #[ignore]d test
        // above — this test targets the realistic STEADY-STATE concurrent-open scenario).
        drop(open_for_agent(&dir_path, agent_id).expect("pre-warm open_for_agent must succeed"));

        let make_claim = |i: usize, thread_tag: &str| {
            Claim::new(
                ClaimRef::new_random(),
                AgentId(agent_id.into()),
                Fact { subject: format!("concurrent-subj-{thread_tag}-{i}"), predicate: "p".into(), value: serde_json::json!(i) },
                Cardinality::Functional,
                ProvenanceLabel::External(ExternalKind::UserAsserted),
                ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
                TransactionTime(chrono::Utc::now()),
                ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
                Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
                Criticality::Low,
                vec![],
                None,
                None,
            )
        };

        let spawn_writer = |thread_tag: &'static str, dir_path: std::path::PathBuf| {
            std::thread::spawn(move || -> Vec<ClaimRef> {
                // File is already migrated (pre-warmed above) — this open must succeed
                // immediately for both threads; no migration DDL race is possible here.
                let conn = open_for_agent(&dir_path, agent_id)
                    .unwrap_or_else(|e| panic!("thread {thread_tag}: open_for_agent on an already-migrated file must succeed: {e:?}"));
                let store = SqlitePersistenceStore::new(conn);
                let agent = AgentId(agent_id.into());
                let mut refs = Vec::with_capacity(WRITES_PER_THREAD);
                for i in 0..WRITES_PER_THREAD {
                    let claim = make_claim(i, thread_tag);
                    let claim_ref = claim.claim_ref().clone();
                    // Retry on transient SQLITE_BUSY from cross-connection contention —
                    // both connections point at the SAME file; WAL allows concurrent
                    // readers with one writer, but two independent Connections can still
                    // race on BEGIN IMMEDIATE. A bounded retry loop tolerates that without
                    // masking a genuine correctness bug (verified via final row-count).
                    let mut attempts = 0;
                    loop {
                        let mut txn = match store.begin_atomic(&agent) {
                            Ok(t) => t,
                            Err(_) if attempts < 200 => { attempts += 1; std::thread::yield_now(); continue; }
                            Err(e) => panic!("thread {thread_tag}: begin_atomic failed after retries: {e:?}"),
                        };
                        // On append_claim failure the txn must be explicitly rolled back
                        // BEFORE retrying — silently dropping it here (e.g. via a chained
                        // `.and_then(|_| store.commit(txn))`) would leak the connection out
                        // of the store's single-connection slot and leave every subsequent
                        // call on this store permanently failing with `TxnAlreadyOpen`.
                        match store.append_claim(&mut txn, &claim) {
                            Ok(_) => match store.commit(txn) {
                                Ok(()) => break,
                                Err(_) if attempts < 200 => { attempts += 1; std::thread::yield_now(); continue; }
                                Err(e) => panic!("thread {thread_tag}: commit failed after retries: {e:?}"),
                            },
                            Err(_) if attempts < 200 => {
                                let _ = store.rollback(txn);
                                attempts += 1;
                                std::thread::yield_now();
                                continue;
                            }
                            Err(e) => panic!("thread {thread_tag}: append_claim failed after retries: {e:?}"),
                        }
                    }
                    refs.push(claim_ref);
                }
                refs
            })
        };

        let h1 = spawn_writer("t1", dir_path.clone());
        let h2 = spawn_writer("t2", dir_path.clone());
        let refs1 = h1.join().expect("thread t1 must not panic");
        let refs2 = h2.join().expect("thread t2 must not panic");

        assert_eq!(refs1.len(), WRITES_PER_THREAD);
        assert_eq!(refs2.len(), WRITES_PER_THREAD);

        // Reopen fresh and verify final state: no corruption, every claim from both
        // threads is present and individually readable, no duplicates, no loss.
        let verify_conn = open_for_agent(&dir_path, agent_id).expect("final reopen must succeed");
        let verify_store = SqlitePersistenceStore::new(verify_conn);
        let agent = AgentId(agent_id.into());

        let all_refs: Vec<Arc<ClaimRef>> = refs1.iter().chain(refs2.iter()).map(|r| Arc::new(r.clone())).collect();
        assert_eq!(all_refs.len(), WRITES_PER_THREAD * 2, "sanity: expected 2*WRITES_PER_THREAD total refs collected");
        let unique_refs: std::collections::HashSet<ClaimRef> = refs1.iter().chain(refs2.iter()).cloned().collect();
        assert_eq!(unique_refs.len(), WRITES_PER_THREAD * 2, "no duplicate claim_refs across threads (HashSet-verified uniqueness, not just vector length)");

        for r in refs1.iter().chain(refs2.iter()) {
            let loaded = verify_store
                .load_claim(&agent, r)
                .unwrap_or_else(|e| panic!("final verify load_claim must not error: {e:?}"))
                .unwrap_or_else(|| panic!("claim {r:?} written by a concurrent thread must be present after both threads complete — possible corruption/lost write"));
            assert_eq!(&loaded.claim_ref().clone(), r);
        }

        let _ = fs::remove_file(dir_path.join(format!("agent_{agent_id}.db-wal")));
        let _ = fs::remove_file(dir_path.join(format!("agent_{agent_id}.db-shm")));
    }
}
