//! Conformance proof: run the shared `PersistencePort` harness against the known-good
//! SQLite adapter. If this test passes, the harness is correct (A43).

use mempill_core::testing::conformance::{
    run_assert_validity_conformance,
    run_disposition_scope_conformance, run_granularity_conformance, run_history_conformance,
    run_history_granularity_conformance, run_persistence_conformance,
    run_reconcile_incumbent_selection_matches_query_memory_primary_conformance,
    run_sweep_resolves_then_supersession_happens_only_via_submit_adjudication_conformance,
    run_valid_at_conformance,
};
use mempill_sqlite::{connection::open_in_memory, store::SqlitePersistenceStore};

#[test]
fn sqlite_passes_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = SqlitePersistenceStore::new(conn);
    run_persistence_conformance(&store);
}

#[test]
fn sqlite_passes_history_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = SqlitePersistenceStore::new(conn);
    run_history_conformance(&store);
}

#[test]
fn sqlite_passes_disposition_scope_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_disposition_scope_conformance(&store);
}

/// valid_at point-in-time query conformance suite against SQLite.
///
/// Proves that the SQLite adapter + fold correctly implement bi-temporal
/// valid_at selection across a three-slot CEO succession (Alice/Bob/Carol)
/// and that valid_at composes with as_of_tx_time per the D2 independence rule.
#[test]
fn sqlite_passes_valid_at_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = SqlitePersistenceStore::new(conn);
    run_valid_at_conformance(&store);
}

/// `DateGranularity` persistence conformance suite against SQLite.
///
/// Proves that `start_granularity` and `end_granularity` round-trip identically
/// on the SQLite adapter across three scenarios: Month/open, Day/Year, and None/None.
#[test]
fn sqlite_passes_granularity_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = SqlitePersistenceStore::new(conn);
    run_granularity_conformance(&store);
}

/// `HistoryEntry` granularity + derived-endpoint conformance suite against SQLite (TASK-32).
///
/// Proves `valid_from_granularity` / `valid_until_granularity` round-trip honestly through
/// `QueryHistoryUseCase`, including the supersession case where `valid_until_granularity`
/// must carry the SUCCESSOR's `start_granularity`, not the predecessor's own `end_granularity`.
#[test]
fn sqlite_passes_history_granularity_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_history_granularity_conformance(&store);
}

/// DIAG_silent_succession §6(b): reconcile's incumbent selection must agree with
/// query_memory's primary/status view on a genuine 2-claim overlap (SQLite).
#[test]
fn sqlite_reconcile_incumbent_selection_matches_query_memory_primary() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_reconcile_incumbent_selection_matches_query_memory_primary_conformance(&store);
}

/// DIAG_silent_succession §6(b): sweep reverts without superseding; only
/// submit_adjudication(Affirm) may supersede the incumbent (SQLite).
#[test]
fn sqlite_sweep_resolves_then_supersession_happens_only_via_submit_adjudication() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_sweep_resolves_then_supersession_happens_only_via_submit_adjudication_conformance(&store);
}

/// TASK-33 E2: `assert_validity` + `end_fact` conformance suite against SQLite.
#[test]
fn sqlite_passes_assert_validity_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_assert_validity_conformance(&store);
}
