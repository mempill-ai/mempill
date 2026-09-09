//! TASK-33 / QA-A: cross-agent scope-isolation + scale-tenancy + ledger-pagination
//! conformance suite against the SQLite adapter (in-memory).
//!
//! `run_agent_isolation_conformance` proves that two agents (A, B) sharing ONE store
//! never see or affect each other's data through ingest_claim, query_memory,
//! query_history, query_subject, query_ledger (audit), reconcile, submit_adjudication,
//! or sweep_adjudications.
//!
//! `run_scale_tenancy_isolation_conformance` extends the existing >10k-row scale proof
//! to two agents: A floods its ledger past 10k rows, B keeps a small ledger sharing
//! subject/predicate NAMES with A — proves neither pollutes the other.
//!
//! `run_ledger_pagination_conformance` proves `load_ledger` limit+from_tx_time
//! continuation achieves full coverage (no skips) across page boundaries, including a
//! tie-boundary where two entries share the exact same `recorded_at`.

use mempill_core::testing::conformance::{
    run_agent_isolation_conformance, run_ledger_pagination_conformance,
    run_scale_tenancy_isolation_conformance,
};
use mempill_sqlite::{connection::open_in_memory, store::SqlitePersistenceStore};

#[test]
fn sqlite_passes_agent_isolation_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_agent_isolation_conformance(&store);
}

/// Slow (>10k-row flood x2 agents) — proves scale does not erode cross-agent isolation.
#[test]
fn sqlite_passes_scale_tenancy_isolation_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = std::sync::Arc::new(SqlitePersistenceStore::new(conn));
    run_scale_tenancy_isolation_conformance(&store);
}

#[test]
fn sqlite_passes_ledger_pagination_conformance() {
    let conn = open_in_memory().expect("in-memory SQLite connection must open");
    let store = SqlitePersistenceStore::new(conn);
    run_ledger_pagination_conformance(&store);
}
