//! TASK-33 / QA-A: cross-agent scope-isolation + scale-tenancy + ledger-pagination
//! conformance suite against the Postgres adapter.
//!
//! Mirrors `mempill-sqlite/tests/scope_isolation_conformance.rs` exactly — same
//! scenarios, different adapter and Postgres major version (16 / 18).

mod common;

use mempill_core::testing::conformance::{
    run_agent_isolation_conformance, run_ledger_pagination_conformance,
    run_scale_tenancy_isolation_conformance,
};

/// Cross-agent isolation conformance suite against postgres:16.
#[test]
fn postgres_agent_isolation_conformance_pg16() {
    common::with_pg("16", |store| {
        run_agent_isolation_conformance(&store);
    });
}

/// Cross-agent isolation conformance suite against postgres:18.
#[test]
fn postgres_agent_isolation_conformance_pg18() {
    common::with_pg("18", |store| {
        run_agent_isolation_conformance(&store);
    });
}

/// Scale x tenancy isolation conformance suite against postgres:16 (SLOW — >10k-row
/// flood for agent A).
#[test]
fn postgres_scale_tenancy_isolation_conformance_pg16() {
    common::with_pg("16", |store| {
        run_scale_tenancy_isolation_conformance(&store);
    });
}

/// Scale x tenancy isolation conformance suite against postgres:18 (SLOW).
#[test]
fn postgres_scale_tenancy_isolation_conformance_pg18() {
    common::with_pg("18", |store| {
        run_scale_tenancy_isolation_conformance(&store);
    });
}

/// Ledger pagination conformance suite against postgres:16.
#[test]
fn postgres_ledger_pagination_conformance_pg16() {
    common::with_pg("16", |store| {
        run_ledger_pagination_conformance(&*store);
    });
}

/// Ledger pagination conformance suite against postgres:18.
#[test]
fn postgres_ledger_pagination_conformance_pg18() {
    common::with_pg("18", |store| {
        run_ledger_pagination_conformance(&*store);
    });
}
