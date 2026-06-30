//! Index-plan assertion for `list_predicates_for_subject` on Postgres.
//!
//! `SELECT DISTINCT predicate FROM claims WHERE agent_id=$1 AND subject=$2`
//! must use an Index Scan or Index Only Scan on `idx_claims_subject_line`.
//! A Seq Scan (full table scan) is not acceptable.
//!
//! Postgres `EXPLAIN (FORMAT JSON)` is used to inspect the plan.  The `Node Type`
//! field in the JSON plan is checked for "Seq Scan" absence and "Index" presence.
//!
//! Version matrix: PG 16 and PG 18.

mod common;

use postgres::Client;

/// Collect the full EXPLAIN JSON text from Postgres for a given SQL string.
///
/// Uses a fresh `postgres::Client` (not the pool) so we can issue EXPLAIN
/// directly without going through `PostgresPersistenceStore`.
fn explain_json(conn_str: &str, sql: &str) -> String {
    let mut client = Client::connect(conn_str, postgres::NoTls)
        .expect("postgres::Client must connect for EXPLAIN");
    let rows = client
        .query(&format!("EXPLAIN (FORMAT JSON) {sql}"), &[])
        .expect("EXPLAIN must succeed");
    // EXPLAIN (FORMAT JSON) returns a single row with a single TEXT column.
    rows.first()
        .expect("EXPLAIN must return at least one row")
        .get::<_, String>(0)
}

/// Assert EXPLAIN plan does not contain "Seq Scan" and uses an Index scan.
fn assert_index_scan(plan_json: &str, context: &str) {
    let lower = plan_json.to_ascii_lowercase();
    assert!(
        !lower.contains("\"seq scan\""),
        "{context}: EXPLAIN must not show a Seq Scan; got plan:\n{plan_json}"
    );
    assert!(
        lower.contains("\"index scan\"") || lower.contains("\"index only scan\""),
        "{context}: EXPLAIN must show an Index Scan or Index Only Scan on idx_claims_subject_line; \
         got plan:\n{plan_json}"
    );
    assert!(
        plan_json.contains("idx_claims_subject_line"),
        "{context}: EXPLAIN must reference idx_claims_subject_line; got plan:\n{plan_json}"
    );
}

/// Run the EXPLAIN assertions on both the no-cutoff and with-cutoff SQL variants.
fn run_explain_assertions(conn_str: &str) {
    // No tx_time cutoff.
    let plan_no_cutoff = explain_json(
        conn_str,
        "SELECT DISTINCT predicate FROM claims WHERE agent_id = 'test-agent' AND subject = 'alice'",
    );
    assert_index_scan(&plan_no_cutoff, "list_predicates_for_subject (no cutoff)");

    // With tx_time cutoff — same query plus an AND on the TEXT column.
    let plan_with_cutoff = explain_json(
        conn_str,
        "SELECT DISTINCT predicate FROM claims \
         WHERE agent_id = 'test-agent' AND subject = 'alice' AND tx_time <= '2030-01-01T00:00:00+00:00'",
    );
    // The cutoff adds a filter on tx_time; Postgres may still use the index for the
    // (agent_id, subject) prefix then filter.  A Seq Scan is NOT acceptable.
    assert!(
        !plan_with_cutoff.to_ascii_lowercase().contains("\"seq scan\""),
        "list_predicates_for_subject (with cutoff): EXPLAIN must not show a Seq Scan; \
         got plan:\n{plan_with_cutoff}"
    );
    assert!(
        plan_with_cutoff.contains("idx_claims_subject_line"),
        "list_predicates_for_subject (with cutoff): EXPLAIN must reference idx_claims_subject_line; \
         got plan:\n{plan_with_cutoff}"
    );
}

#[test]
fn postgres_list_predicates_uses_index_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_explain_assertions(&conn_str);
    });
}

#[test]
fn postgres_list_predicates_uses_index_pg18() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_explain_assertions(&conn_str);
    });
}
