//! Schema migration runner for mempill-sqlite.
//!
//! Applies versioned DDL to a rusqlite [`Connection`] in a deterministic, idempotent manner.
//! Schema version is tracked via SQLite's built-in `user_version` PRAGMA.
//!
//! # Intended PRAGMA environment (applied at connection open in connection.rs)
//! - `PRAGMA journal_mode=WAL;`  — write-ahead log for concurrent reads during writes
//! - `PRAGMA synchronous=FULL;`  — full durability (mandatory; WAL+NORMAL can lose writes on power loss)
//! - `PRAGMA foreign_keys=ON;`   — enforce FK constraints defined in DDL

use rusqlite::{Connection, Result, Transaction, TransactionBehavior};

/// The target schema version this runner brings the database to.
/// Increment this constant (and add a new migration step) for every future DDL change.
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

/// Embedded DDL — the 4-table append-only schema (§5).
const V1_INITIAL_SQL: &str = include_str!("schema/v1_initial.sql");

/// Embedded index definitions (§5).
const INDEXES_SQL: &str = include_str!("schema/indexes.sql");

/// Embedded DDL — oracle adjudication queue (pending_adjudications table).
const V2_PENDING_ADJUDICATIONS_SQL: &str = include_str!("schema/v2_pending_adjudications.sql");

/// Embedded DDL — per-endpoint date-granularity columns on claims.
const V3_DATE_GRANULARITY_SQL: &str = include_str!("schema/v3_date_granularity.sql");

/// Embedded DDL — bound_at_granularity column on validity_assertions.
const V4_BOUND_GRANULARITY_SQL: &str = include_str!("schema/v4_bound_granularity.sql");

/// Migration error wrapper.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// A rusqlite error occurred during schema migration.
    #[error("SQLite error during migration: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Apply all pending migrations to `conn` up to [`CURRENT_SCHEMA_VERSION`].
///
/// Idempotent: calling this function on a fully-migrated database is a no-op.
///
/// The version check, all pending DDL steps, and the final `user_version` write run inside a
/// single `BEGIN IMMEDIATE` transaction. `BEGIN IMMEDIATE` acquires SQLite's RESERVED lock
/// up front (instead of lazily on first write, like the default DEFERRED behavior), so two
/// connections racing the first `open_for_agent` of a brand-new per-agent file serialize at
/// BEGIN time: the second connection blocks until the first commits (bounded by the
/// `busy_timeout` set in `connection.rs::apply_pragmas`), then re-reads `user_version` inside
/// its own transaction and observes it already at [`CURRENT_SCHEMA_VERSION`] — a correct
/// no-op — instead of both connections reading `user_version = 0` concurrently and both
/// issuing v3's `ALTER TABLE claims ADD COLUMN`, which fails on the second racer with
/// `duplicate column name`. A partial failure inside the transaction rolls the whole thing
/// back (the `Transaction`'s default drop behavior), so the database is never left at an
/// inconsistent, partially-migrated version.
///
/// Connection lifecycle and PRAGMA initialisation (`journal_mode=WAL`, `synchronous=FULL`,
/// `foreign_keys=ON`, `busy_timeout`) are the caller's responsibility (implemented in
/// `connection.rs`).
pub fn apply_migrations(conn: &Connection) -> Result<(), MigrationError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;

    let current = user_version(&tx)?;

    if current < 1 {
        apply_v1(&tx)?;
    }

    if current < 2 {
        apply_v2(&tx)?;
    }

    if current < 3 {
        apply_v3(&tx)?;
    }

    if current < 4 {
        apply_v4(&tx)?;
    }

    tx.commit()?;
    Ok(())
}

/// Read the SQLite `user_version` PRAGMA (0 = fresh/uninitialized database).
fn user_version(conn: &Connection) -> Result<u32, MigrationError> {
    let v: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(v)
}

/// Set the SQLite `user_version` PRAGMA.
///
/// `PRAGMA user_version` writes to the database header page, which participates in the
/// enclosing transaction like any other page write — so this call is safe to issue inside
/// the same `BEGIN IMMEDIATE` transaction as the DDL in [`apply_migrations`]: both the
/// schema change and the version bump commit (or roll back) atomically together.
fn set_user_version(conn: &Connection, version: u32) -> Result<(), MigrationError> {
    conn.execute_batch(&format!("PRAGMA user_version = {version};"))?;
    Ok(())
}

/// Migration v1: create the 4 append-only tables and all structural indexes.
pub(crate) fn apply_v1(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(V1_INITIAL_SQL)?;
    conn.execute_batch(INDEXES_SQL)?;
    set_user_version(conn, 1)?;
    Ok(())
}

/// Migration v2: create the oracle adjudication queue table and its indexes.
pub(crate) fn apply_v2(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(V2_PENDING_ADJUDICATIONS_SQL)?;
    set_user_version(conn, 2)?;
    Ok(())
}

/// Migration v3: add `valid_time_start_granularity` and `valid_time_end_granularity`
/// nullable TEXT columns to the `claims` table.
///
/// Old rows upgrade cleanly: the new columns default to NULL, which the read path maps to
/// `None` on `ValidTime::start_granularity` and `ValidTime::end_granularity`.
pub(crate) fn apply_v3(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(V3_DATE_GRANULARITY_SQL)?;
    set_user_version(conn, 3)?;
    Ok(())
}

/// Migration v4: add `bound_at_granularity` nullable TEXT column to `validity_assertions`
/// (TASK-33-W5-LIB-R2, DIAG-5).
///
/// Old rows upgrade cleanly: the new column defaults to NULL, which the read path maps to
/// `None` on `AssertionKind::Bound::bound_at_granularity`.
pub(crate) fn apply_v4(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(V4_BOUND_GRANULARITY_SQL)?;
    set_user_version(conn, 4)?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_memory() -> Connection {
        Connection::open_in_memory().expect("in-memory database should open")
    }

    /// Helper: collect the column names for a given table from sqlite_master PRAGMA.
    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// Helper: check whether an index exists in sqlite_master.
    fn index_exists(conn: &Connection, index_name: &str) -> bool {
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [index_name],
                |row| row.get(0),
            )
            .unwrap_or(0);
        count > 0
    }

    /// Helper: check whether a table exists in sqlite_master.
    fn table_exists(conn: &Connection, table_name: &str) -> bool {
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table_name],
                |row| row.get(0),
            )
            .unwrap_or(0);
        count > 0
    }

    #[test]
    fn all_four_tables_exist_after_migration() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        assert!(table_exists(&conn, "claims"), "claims table must exist");
        assert!(
            table_exists(&conn, "validity_assertions"),
            "validity_assertions table must exist"
        );
        assert!(
            table_exists(&conn, "ledger_entries"),
            "ledger_entries table must exist"
        );
        assert!(
            table_exists(&conn, "claim_edges"),
            "claim_edges table must exist"
        );
    }

    #[test]
    fn claims_table_has_expected_columns() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "claims");
        for expected in &[
            "claim_id",
            "agent_id",
            "subject",
            "predicate",
            "value",
            "cardinality",
            "provenance_label",
            "nearest_external_anchor_id",
            "derivation_depth",
            "tx_time",
            "valid_time_start",
            "valid_time_end",
            "valid_time_confidence",
            "value_confidence",
            "criticality",
            "derived_from",
            "metadata",
            "snapshot_schema_version",
            "embedding_model_id",
            // v3 — date granularity
            "valid_time_start_granularity",
            "valid_time_end_granularity",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "claims table missing column: {expected}"
            );
        }
    }

    #[test]
    fn validity_assertions_table_has_expected_columns() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "validity_assertions");
        for expected in &[
            "assertion_id",
            "agent_id",
            "target_claim_id",
            "assertion_kind",
            "bound_at",
            "reopen_at",
            "provenance_label",
            "value_confidence",
            "valid_time_confidence",
            "asserted_at",
            // v4 — bound_at display-precision granularity (TASK-33-W5-LIB-R2)
            "bound_at_granularity",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "validity_assertions table missing column: {expected}"
            );
        }
    }

    #[test]
    fn ledger_entries_table_has_expected_columns() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "ledger_entries");
        for expected in &[
            "entry_id",
            "agent_id",
            "claim_id",
            "event_kind",
            "disposition",
            "rationale",
            "recorded_at",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "ledger_entries table missing column: {expected}"
            );
        }
    }

    #[test]
    fn claim_edges_table_has_expected_columns() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "claim_edges");
        for expected in &[
            "edge_id",
            "agent_id",
            "from_claim_id",
            "to_claim_id",
            "edge_kind",
            "created_at",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "claim_edges table missing column: {expected}"
            );
        }
    }

    #[test]
    fn structural_subject_line_index_exists() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        assert!(
            index_exists(&conn, "idx_claims_subject_line"),
            "primary structural subject-line index must exist"
        );
    }

    #[test]
    fn all_indexes_exist() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let expected_indexes = [
            "idx_claims_subject_line",
            "idx_validity_assertions_target",
            "idx_ledger_agent_time",
            "idx_edges_from",
            "idx_edges_to",
            "idx_claims_provenance",
        ];
        for idx in &expected_indexes {
            assert!(
                index_exists(&conn, idx),
                "index missing after migration: {idx}"
            );
        }
    }

    #[test]
    fn apply_migrations_is_idempotent() {
        let conn = open_memory();
        apply_migrations(&conn).expect("first migration should succeed");
        apply_migrations(&conn).expect("second migration must not error (idempotent)");
        apply_migrations(&conn).expect("third migration must not error (idempotent)");

        // Tables and indexes must still be present after repeated runs.
        assert!(table_exists(&conn, "claims"));
        assert!(table_exists(&conn, "claim_edges"));
        assert!(index_exists(&conn, "idx_claims_subject_line"));
    }

    #[test]
    fn reserved_columns_exist_on_claims() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "claims");
        assert!(
            cols.contains(&"metadata".to_string()),
            "reserved column 'metadata' must exist on claims"
        );
        assert!(
            cols.contains(&"snapshot_schema_version".to_string()),
            "reserved column 'snapshot_schema_version' must exist on claims"
        );
        assert!(
            cols.contains(&"embedding_model_id".to_string()),
            "reserved column 'embedding_model_id' must exist on claims"
        );
    }

    #[test]
    fn schema_version_is_set_after_migration() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let v = user_version(&conn).expect("user_version should be readable");
        assert_eq!(
            v, CURRENT_SCHEMA_VERSION,
            "user_version PRAGMA must equal CURRENT_SCHEMA_VERSION after migration"
        );
    }

    #[test]
    fn pending_adjudications_table_exists_after_migration() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");
        assert!(
            table_exists(&conn, "pending_adjudications"),
            "pending_adjudications table must exist after v2 migration"
        );
    }

    #[test]
    fn pending_adjudications_table_has_expected_columns() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "pending_adjudications");
        for expected in &[
            "handle_id",
            "agent_id",
            "subject",
            "predicate",
            "challenger_claim_ref",
            "incumbent_claim_ref",
            "request_payload",
            "queued_at",
            "expires_at",
            "status",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "pending_adjudications table missing column: {expected}"
            );
        }
    }

    #[test]
    fn pending_adjudications_indexes_exist_after_migration() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        // Agent-id lookup index (oracle poller).
        assert!(
            index_exists(&conn, "idx_pending_adj_agent_id"),
            "idx_pending_adj_agent_id must exist after v2 migration"
        );
        // Partial TTL index (WHERE expires_at IS NOT NULL AND status = 'pending').
        assert!(
            index_exists(&conn, "idx_pending_adj_expires_at"),
            "idx_pending_adj_expires_at must exist after v2 migration"
        );
    }

    #[test]
    fn apply_migrations_v2_is_idempotent() {
        let conn = open_memory();
        apply_migrations(&conn).expect("first migration should succeed");
        apply_migrations(&conn).expect("second migration must not error (idempotent)");
        apply_migrations(&conn).expect("third migration must not error (idempotent)");

        assert!(table_exists(&conn, "pending_adjudications"));
        assert!(index_exists(&conn, "idx_pending_adj_agent_id"));
        assert!(index_exists(&conn, "idx_pending_adj_expires_at"));
    }

    // ── v3 migration tests ────────────────────────────────────────────────────

    /// v3 adds the two nullable granularity columns to the claims table.
    #[test]
    fn v3_granularity_columns_exist_after_migration() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "claims");
        assert!(
            cols.contains(&"valid_time_start_granularity".to_string()),
            "claims table missing column: valid_time_start_granularity (added in v3)"
        );
        assert!(
            cols.contains(&"valid_time_end_granularity".to_string()),
            "claims table missing column: valid_time_end_granularity (added in v3)"
        );
    }

    /// v3 upgrade invariant: a DB at v2 can be upgraded to v3, and old rows (NULL columns)
    /// still read back cleanly.
    #[test]
    fn v3_upgrade_from_v2_succeeds() {
        // Start from scratch and apply only v1 + v2.
        let conn = open_memory();
        apply_v1(&conn).expect("v1 must succeed");
        apply_v2(&conn).expect("v2 must succeed");
        assert_eq!(user_version(&conn).unwrap(), 2, "after v2 version must be 2");

        // Verify granularity columns don't exist yet.
        let cols_before = column_names(&conn, "claims");
        assert!(
            !cols_before.contains(&"valid_time_start_granularity".to_string()),
            "granularity column must not exist before v3"
        );

        // Now upgrade to v3.
        apply_v3(&conn).expect("v3 upgrade must succeed");
        assert_eq!(user_version(&conn).unwrap(), 3, "after v3 version must be 3");

        let cols_after = column_names(&conn, "claims");
        assert!(
            cols_after.contains(&"valid_time_start_granularity".to_string()),
            "granularity column must exist after v3"
        );
        assert!(
            cols_after.contains(&"valid_time_end_granularity".to_string()),
            "granularity column must exist after v3"
        );
    }

    /// Running apply_migrations on a v2 database upgrades it to v3.
    #[test]
    fn apply_migrations_upgrades_v2_to_v3() {
        let conn = open_memory();
        apply_v1(&conn).expect("v1 must succeed");
        apply_v2(&conn).expect("v2 must succeed");

        // Simulate an existing v2 DB being opened with the new library.
        apply_migrations(&conn).expect("apply_migrations must succeed on v2 db");

        let v = user_version(&conn).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION, "version must be CURRENT_SCHEMA_VERSION after upgrade");

        let cols = column_names(&conn, "claims");
        assert!(cols.contains(&"valid_time_start_granularity".to_string()));
        assert!(cols.contains(&"valid_time_end_granularity".to_string()));
    }

    // ── v4 migration tests (TASK-33-W5-LIB-R2, DIAG-5) ───────────────────────

    /// v4 adds the nullable `bound_at_granularity` column to validity_assertions.
    #[test]
    fn v4_bound_granularity_column_exists_after_migration() {
        let conn = open_memory();
        apply_migrations(&conn).expect("migrations should succeed");

        let cols = column_names(&conn, "validity_assertions");
        assert!(
            cols.contains(&"bound_at_granularity".to_string()),
            "validity_assertions table missing column: bound_at_granularity (added in v4)"
        );
    }

    /// v4 upgrade invariant: a DB at v3 (pre-existing rows, written before this field existed)
    /// can be upgraded to v4, and the old row's new column reads back as NULL — the exact
    /// "legacy row deserializes without the field" guarantee at the storage layer.
    #[test]
    fn v4_upgrade_from_v3_preserves_legacy_rows_as_null() {
        let conn = open_memory();
        apply_v1(&conn).expect("v1 must succeed");
        apply_v2(&conn).expect("v2 must succeed");
        apply_v3(&conn).expect("v3 must succeed");
        assert_eq!(user_version(&conn).unwrap(), 3, "after v3 version must be 3");

        // Seed a claim + a v3-era Bound validity_assertion row (no bound_at_granularity column
        // exists yet at this point — the INSERT simply doesn't reference it).
        conn.execute(
            "INSERT INTO claims (
                claim_id, agent_id, subject, predicate, value, cardinality,
                provenance_label, derivation_depth, tx_time,
                valid_time_start, valid_time_end, valid_time_confidence,
                value_confidence, criticality, derived_from, snapshot_schema_version
            ) VALUES ('c1', 'agent-1', 's', 'p', '\"v\"', 'Functional',
                'External_UserAsserted', 0, '2024-01-01T00:00:00Z',
                NULL, NULL, 0.5, 0.5, 'Medium', '[]', 1)",
            [],
        ).expect("legacy claim insert must succeed");
        conn.execute(
            "INSERT INTO validity_assertions (
                assertion_id, agent_id, target_claim_id, assertion_kind, bound_at, reopen_at,
                provenance_label, value_confidence, valid_time_confidence, asserted_at
            ) VALUES ('a1', 'agent-1', 'c1', 'Bound', '2024-09-01T00:00:00Z', NULL,
                'External_ExternalFirstHand', 1.0, 1.0, '2024-09-01T00:00:00Z')",
            [],
        ).expect("legacy Bound assertion insert (no bound_at_granularity column yet) must succeed");

        // Upgrade to v4.
        apply_v4(&conn).expect("v4 upgrade must succeed");
        assert_eq!(user_version(&conn).unwrap(), 4, "after v4 version must be 4");

        let cols = column_names(&conn, "validity_assertions");
        assert!(cols.contains(&"bound_at_granularity".to_string()), "column must exist after v4");

        let gran: Option<String> = conn
            .query_row(
                "SELECT bound_at_granularity FROM validity_assertions WHERE assertion_id = 'a1'",
                [],
                |row| row.get(0),
            )
            .expect("legacy row must still be readable after v4 upgrade");
        assert_eq!(gran, None, "legacy pre-v4 row must read back bound_at_granularity = NULL");
    }

    /// Running apply_migrations on a v3 database upgrades it to v4 (CURRENT_SCHEMA_VERSION).
    #[test]
    fn apply_migrations_upgrades_v3_to_v4() {
        let conn = open_memory();
        apply_v1(&conn).expect("v1 must succeed");
        apply_v2(&conn).expect("v2 must succeed");
        apply_v3(&conn).expect("v3 must succeed");

        apply_migrations(&conn).expect("apply_migrations must succeed on v3 db");

        let v = user_version(&conn).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION, "version must be CURRENT_SCHEMA_VERSION after upgrade");
        assert_eq!(v, 4);

        let cols = column_names(&conn, "validity_assertions");
        assert!(cols.contains(&"bound_at_granularity".to_string()));
    }
}
