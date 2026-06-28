//! Conformance proof: run the shared `PersistencePort` harness against the Postgres adapter.
//!
//! Proves behavioral parity with SQLite (A43): the SAME sub-tests that pass against
//! `SqlitePersistenceStore` must also pass against `PostgresPersistenceStore` on real
//! PG instances started via testcontainers.
//!
//! Version matrix: PG 16 and PG 18 — both tags pinned explicitly (no `:latest`).
//! Each test function runs its own container for full isolation.

mod common;

use mempill_core::testing::conformance::{
    run_disposition_scope_conformance, run_granularity_conformance, run_history_conformance,
    run_persistence_conformance, run_valid_at_conformance,
};

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

/// Disposition-scope correctness suite against postgres:16.
#[test]
fn postgres_disposition_scope_conformance_pg16() {
    common::with_pg("16", |store| {
        run_disposition_scope_conformance(&*store);
    });
}

/// Disposition-scope correctness suite against postgres:18.
#[test]
fn postgres_disposition_scope_conformance_pg18() {
    common::with_pg("18", |store| {
        run_disposition_scope_conformance(&*store);
    });
}

/// valid_at point-in-time query conformance suite against postgres:16.
///
/// Mirrors `sqlite_passes_valid_at_conformance` exactly — same scenarios,
/// different adapter. Proves SQLite and Postgres return identical results
/// for bi-temporal valid_at queries across the CEO succession timeline.
#[test]
fn postgres_valid_at_conformance_pg16() {
    common::with_pg("16", |store| {
        run_valid_at_conformance(&*store);
    });
}

/// valid_at point-in-time query conformance suite against postgres:18.
///
/// Mirrors `sqlite_passes_valid_at_conformance` exactly — same scenarios,
/// different adapter and Postgres major version.
#[test]
fn postgres_valid_at_conformance_pg18() {
    common::with_pg("18", |store| {
        run_valid_at_conformance(&*store);
    });
}

/// `DateGranularity` persistence conformance suite against postgres:16.
///
/// Proves that `start_granularity` and `end_granularity` round-trip identically
/// on the Postgres adapter — confirming cross-adapter parity with SQLite for the
/// three conformance scenarios: Month/open, Day/Year, and None/None.
#[test]
fn postgres_granularity_conformance_pg16() {
    common::with_pg("16", |store| {
        run_granularity_conformance(&*store);
    });
}

/// `DateGranularity` persistence conformance suite against postgres:18.
///
/// Mirrors `postgres_granularity_conformance_pg16` on Postgres 18 to confirm
/// there is no version-specific regression in granularity column handling.
#[test]
fn postgres_granularity_conformance_pg18() {
    common::with_pg("18", |store| {
        run_granularity_conformance(&*store);
    });
}
