//! Schema migration runner for mempill-sqlite.
//!
//! Applies versioned DDL to a rusqlite [`Connection`] in a deterministic, idempotent manner.
//! Schema version is tracked via SQLite's built-in `user_version` PRAGMA.
//!
//! # Intended PRAGMA environment (applied at connection open in connection.rs)
//! - `PRAGMA journal_mode=WAL;`  — write-ahead log for concurrent reads during writes
//! - `PRAGMA synchronous=FULL;`  — full durability (mandatory; WAL+NORMAL can lose writes on power loss)
//! - `PRAGMA foreign_keys=ON;`   — enforce FK constraints defined in DDL

use rusqlite::{Connection, Result};

/// The target schema version this runner brings the database to.
/// Increment this constant (and add a new migration step) for every future DDL change.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Embedded DDL — the 4-table append-only schema (§5).
const V1_INITIAL_SQL: &str = include_str!("schema/v1_initial.sql");

/// Embedded index definitions (§5).
const INDEXES_SQL: &str = include_str!("schema/indexes.sql");

/// Embedded DDL — oracle adjudication queue (pending_adjudications table).
const V2_PENDING_ADJUDICATIONS_SQL: &str = include_str!("schema/v2_pending_adjudications.sql");

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
/// Each migration step runs inside its own transaction so a partial failure leaves the
/// database at a consistent version boundary (each migration step is fully atomic).
///
/// Connection lifecycle and PRAGMA initialisation (`journal_mode=WAL`, `synchronous=FULL`,
/// `foreign_keys=ON`) are the caller's responsibility (implemented in `connection.rs`).
pub fn apply_migrations(conn: &Connection) -> Result<(), MigrationError> {
    let current = user_version(conn)?;

    if current < 1 {
        apply_v1(conn)?;
    }

    if current < 2 {
        apply_v2(conn)?;
    }

    Ok(())
}

/// Read the SQLite `user_version` PRAGMA (0 = fresh/uninitialized database).
fn user_version(conn: &Connection) -> Result<u32, MigrationError> {
    let v: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(v)
}

/// Set the SQLite `user_version` PRAGMA.
///
/// This PRAGMA write is intentionally NOT inside the DDL transaction because SQLite
/// does not allow PRAGMA user_version inside a transaction on all versions. We set it
/// after the DDL transaction commits, so a crash between DDL commit and PRAGMA write is
/// safe: the DDL tables already exist and `CREATE TABLE IF NOT EXISTS` makes the next
/// migration run a no-op even if user_version is still 0.
fn set_user_version(conn: &Connection, version: u32) -> Result<(), MigrationError> {
    conn.execute_batch(&format!("PRAGMA user_version = {version};"))?;
    Ok(())
}

/// Migration v1: create the 4 append-only tables and all structural indexes.
fn apply_v1(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(V1_INITIAL_SQL)?;
    conn.execute_batch(INDEXES_SQL)?;
    set_user_version(conn, 1)?;
    Ok(())
}

/// Migration v2: create the oracle adjudication queue table and its indexes.
fn apply_v2(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(V2_PENDING_ADJUDICATIONS_SQL)?;
    set_user_version(conn, 2)?;
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
}
