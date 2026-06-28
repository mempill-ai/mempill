//! Conformance proof: run the shared `PersistencePort` harness against the known-good
//! SQLite adapter. If this test passes, the harness is correct (A43).

use mempill_core::testing::conformance::{
    run_disposition_scope_conformance, run_granularity_conformance, run_history_conformance,
    run_persistence_conformance, run_valid_at_conformance,
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
    let store = SqlitePersistenceStore::new(conn);
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
