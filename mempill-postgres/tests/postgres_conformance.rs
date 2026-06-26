//! Conformance proof: run the shared `PersistencePort` harness against the Postgres adapter.
//!
//! Proves behavioral parity with SQLite (A43): the SAME 12 sub-tests that pass against
//! `SqlitePersistenceStore` must also pass against `PostgresPersistenceStore` on real
//! PG instances started via testcontainers.
//!
//! Version matrix: PG 16 and PG 18 — both tags pinned explicitly (no `:latest`).
//! Each test function runs its own container for full isolation.

mod common;

use mempill_core::testing::conformance::{run_history_conformance, run_persistence_conformance};

/// Conformance suite against postgres:16.
///
/// Pins tag "16". Proves the Postgres adapter passes all 12 sub-tests on PG 16.
#[test]
fn postgres_conformance_pg16() {
    common::with_pg("16", |store| {
        run_persistence_conformance(&*store);
    });
}

/// Conformance suite against postgres:18.
///
/// Pins tag "18" (current latest stable: 18.4). Proves the Postgres adapter passes
/// all 12 sub-tests on PG 18 with identical behavior to PG 16.
#[test]
fn postgres_conformance_pg18() {
    common::with_pg("18", |store| {
        run_persistence_conformance(&*store);
    });
}

/// History conformance suite against postgres:16.
#[test]
fn postgres_history_conformance_pg16() {
    common::with_pg("16", |store| {
        run_history_conformance(&*store);
    });
}

/// History conformance suite against postgres:18.
#[test]
fn postgres_history_conformance_pg18() {
    common::with_pg("18", |store| {
        run_history_conformance(&*store);
    });
}
