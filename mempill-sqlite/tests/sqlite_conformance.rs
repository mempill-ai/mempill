//! Conformance proof: run the shared `PersistencePort` harness against the known-good
//! SQLite adapter. If this test passes, the harness is correct (A43).

use mempill_core::testing::conformance::{
    run_disposition_scope_conformance, run_history_conformance, run_persistence_conformance,
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
