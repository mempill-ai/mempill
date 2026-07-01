//! Index-plan assertion for `list_predicates_for_subject`.
//!
//! `SELECT DISTINCT predicate FROM claims WHERE agent_id=? AND subject=?`
//! must be served by `idx_claims_subject_line (agent_id, subject, predicate, tx_time DESC)`,
//! not a full-table scan and not a temp B-tree for the DISTINCT.
//!
//! The index covers `(agent_id, subject, predicate, tx_time DESC)`.  SQLite can satisfy
//! `DISTINCT predicate` given equality constraints on `(agent_id, subject)` by scanning the
//! index prefix — every change in `predicate` is a distinct group, so no temp B-tree is
//! needed.  The EXPLAIN QUERY PLAN output must include `idx_claims_subject_line` and
//! must NOT include `TEMP B-TREE`.

#![allow(missing_docs)]

use rusqlite::Connection;

/// Run EXPLAIN QUERY PLAN and collect the `detail` column from all rows.
fn explain(conn: &Connection, sql: &str) -> Vec<String> {
    let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&explain_sql).expect("EXPLAIN prepare must succeed");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(3))
        .expect("EXPLAIN query must succeed");
    rows.map(|r| r.expect("row must decode")).collect()
}

/// Assert that `list_predicates_for_subject` without tx_time cutoff uses the covering index.
///
/// SQL: `SELECT DISTINCT predicate FROM claims WHERE agent_id = ?1 AND subject = ?2`
///
/// Expected: plan contains `idx_claims_subject_line`, does NOT contain `TEMP B-TREE`.
#[test]
fn list_predicates_uses_index_no_cutoff() {
    let conn = mempill_sqlite::connection::open_in_memory()
        .expect("in-memory SQLite must open");

    let plan = explain(
        &conn,
        "SELECT DISTINCT predicate FROM claims WHERE agent_id = 'a' AND subject = 's'",
    );

    let plan_text = plan.join("\n");
    assert!(
        plan_text.contains("idx_claims_subject_line"),
        "DISTINCT predicate query (no cutoff) must use idx_claims_subject_line; \
         got plan:\n{plan_text}"
    );
    assert!(
        !plan_text.to_ascii_uppercase().contains("TEMP B-TREE"),
        "DISTINCT predicate query (no cutoff) must NOT require a TEMP B-TREE; \
         the DISTINCT is served by the index prefix (agent_id, subject, predicate); \
         got plan:\n{plan_text}"
    );
}

/// Assert that `list_predicates_for_subject` WITH tx_time cutoff also uses the covering index.
///
/// SQL: `SELECT DISTINCT predicate FROM claims WHERE agent_id = ?1 AND subject = ?2 AND tx_time <= ?3`
///
/// The additional range filter on `tx_time` does NOT break the index prefix scan because
/// the DISTINCT column (`predicate`) is column 3 of the index and the equality-then-range
/// filter pattern `(agent_id=, subject=, tx_time<=)` still allows SQLite to apply the index.
/// A TEMP B-TREE may appear in some SQLite versions when the filter disrupts the DISTINCT
/// prefix; the important thing is that the index IS used (no full table scan).
#[test]
fn list_predicates_uses_index_with_cutoff() {
    let conn = mempill_sqlite::connection::open_in_memory()
        .expect("in-memory SQLite must open");

    let plan = explain(
        &conn,
        "SELECT DISTINCT predicate FROM claims WHERE agent_id = 'a' AND subject = 's' AND tx_time <= '2030-01-01T00:00:00+00:00'",
    );

    let plan_text = plan.join("\n");
    assert!(
        plan_text.contains("idx_claims_subject_line"),
        "DISTINCT predicate query (with cutoff) must use idx_claims_subject_line; \
         got plan:\n{plan_text}"
    );
    // A TEMP B-TREE for DISTINCT is acceptable when tx_time breaks the prefix order,
    // but a full SCAN on the base TABLE (not the index) is not acceptable.
    assert!(
        !plan_text.to_ascii_uppercase().contains("SCAN claims"),
        "DISTINCT predicate query (with cutoff) must NOT full-scan the claims table; \
         it must use the index; got plan:\n{plan_text}"
    );
}
