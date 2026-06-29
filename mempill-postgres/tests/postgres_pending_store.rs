//! W3 pending-adjudication store tests for `PostgresPendingStore` (Amendment 1).
//!
//! Mirrors the `w3_sqlite_pending_*` test suite in mempill-sqlite/src/store.rs.
//! All tests run against live Postgres containers via testcontainers.
//!
//! Version matrix: PG 16 and PG 18 — both pinned explicitly (no `:latest`).
//! Each test function spawns its own container for full isolation.
//!
//! Tests covered:
//!   1. insert_pending + get_pending round-trip
//!   2. get_pending returns None for unknown handle_id
//!   3. list_pending filtered by agent_id
//!   4. list_expired filtered by expires_at
//!   5. Durability: insert row → build a NEW PostgresPersistenceStore on same DB
//!      → confirm get_pending still finds the row (proves DB-authoritative persistence)

mod common;

use std::sync::Arc;

use chrono::Utc;
use mempill_core::ports::pending_adjudication::{PendingAdjudicationPort, PendingAdjudicationRow};
use mempill_postgres::{PostgresPendingStore, PostgresPersistenceStore};
use mempill_types::{
    AdjudicationRequest, AgentId, Belief, ClaimRef, Confidence, Criticality,
    CurrencySignal, CurrencyState, ExternalAnchor, ExternalKind, Fact, OverturnReason,
    ProvenanceLabel, SubjectLineRef, TransactionTime, ValidTime,
};
use uuid::Uuid;

// ── Builder helpers ───────────────────────────────────────────────────────────

fn agent(name: &str) -> AgentId {
    AgentId(name.into())
}

fn make_adj_request(ag: &AgentId) -> AdjudicationRequest {
    let claim_ref = ClaimRef(Uuid::new_v4());
    let now = TransactionTime(Utc::now());
    AdjudicationRequest {
        subject_line: SubjectLineRef {
            agent_id: ag.clone(),
            subject: "user".into(),
            predicate: "city".into(),
        },
        incumbent: Belief {
            claim_ref: claim_ref.clone(),
            fact: Fact {
                subject: "user".into(),
                predicate: "city".into(),
                value: serde_json::json!("Berlin"),
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            transaction_time: now.clone(),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            currency_signal: CurrencySignal {
                last_refreshed_at: now.clone(),
                state: CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: Criticality::Low,
        },
        challenger: mempill_types::Claim::new(
            ClaimRef(Uuid::new_v4()),
            ag.clone(),
            Fact {
                subject: "user".into(),
                predicate: "city".into(),
                value: serde_json::json!("Paris"),
            },
            mempill_types::Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        ),
        criticality: Criticality::Low,
        reason: OverturnReason::ExternalContradiction,
    }
}

fn make_pending_row(ag: &AgentId) -> PendingAdjudicationRow {
    PendingAdjudicationRow {
        handle_id: Uuid::new_v4(),
        agent_id: ag.clone(),
        subject: "user".into(),
        predicate: "city".into(),
        challenger_claim_ref: ClaimRef(Uuid::new_v4()),
        incumbent_claim_ref: ClaimRef(Uuid::new_v4()),
        request_payload: make_adj_request(ag),
        queued_at: Utc::now(),
        expires_at: None,
        status: "pending".into(),
    }
}

fn make_pending_row_with_expiry(ag: &AgentId, expires_at: chrono::DateTime<Utc>) -> PendingAdjudicationRow {
    let mut row = make_pending_row(ag);
    row.expires_at = Some(expires_at);
    row
}

// ── Core test logic (version-agnostic, parameterized over the store) ─────────

fn run_insert_get_round_trip(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("pg-agent-roundtrip");
    let row = make_pending_row(&ag);
    let handle_id = row.handle_id;

    pending.insert_pending(&row).expect("insert_pending must succeed");

    let fetched = pending.get_pending(handle_id).expect("get_pending must succeed");
    let fetched = fetched.expect("row must be present after insert");
    assert_eq!(fetched.handle_id, handle_id);
    assert_eq!(fetched.agent_id, ag);
    assert_eq!(fetched.subject, "user");
    assert_eq!(fetched.predicate, "city");
    assert_eq!(fetched.challenger_claim_ref, row.challenger_claim_ref);
    assert_eq!(fetched.incumbent_claim_ref, row.incumbent_claim_ref);
    assert_eq!(fetched.status, "pending");
    assert!(fetched.expires_at.is_none());
}

fn run_get_nonexistent_returns_none(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let result = pending.get_pending(Uuid::new_v4()).expect("get_pending must not error");
    assert!(result.is_none(), "unknown handle_id must return None");
}

fn run_list_pending_by_agent(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("pg-agent-list");
    let ag2 = agent("pg-other-list");

    let row1 = make_pending_row(&ag);
    let row2 = make_pending_row(&ag);
    let row3 = make_pending_row(&ag2);

    pending.insert_pending(&row1).unwrap();
    pending.insert_pending(&row2).unwrap();
    pending.insert_pending(&row3).unwrap();

    let agent_rows = pending.list_pending(Some(&ag)).unwrap();
    assert_eq!(agent_rows.len(), 2, "must return exactly 2 rows for agent");

    let all_rows = pending.list_pending(None).unwrap();
    assert_eq!(all_rows.len(), 3, "list_pending(None) must return all 3 rows");
}

fn run_list_expired(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("pg-agent-expired");

    let past = Utc::now() - chrono::Duration::hours(1);
    let future = Utc::now() + chrono::Duration::hours(1);

    let expired_row = make_pending_row_with_expiry(&ag, past);
    let live_row = make_pending_row_with_expiry(&ag, future);
    let no_expiry_row = make_pending_row(&ag);

    pending.insert_pending(&expired_row).unwrap();
    pending.insert_pending(&live_row).unwrap();
    pending.insert_pending(&no_expiry_row).unwrap();

    let expired = pending.list_expired(Utc::now()).unwrap();
    assert_eq!(expired.len(), 1, "exactly one expired row must be returned");
    assert_eq!(expired[0].handle_id, expired_row.handle_id);
}

fn run_mark_resolved(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("pg-agent-resolve");
    let row = make_pending_row(&ag);
    let handle_id = row.handle_id;

    pending.insert_pending(&row).unwrap();
    pending.mark_resolved(handle_id).unwrap();

    // get_pending still finds it (status = 'resolved').
    let fetched = pending.get_pending(handle_id).unwrap().unwrap();
    assert_eq!(fetched.status, "resolved", "status must be 'resolved' after mark_resolved");

    // list_pending must NOT include it.
    let pending_rows = pending.list_pending(Some(&ag)).unwrap();
    assert!(pending_rows.is_empty(), "resolved row must not appear in list_pending");
}

fn run_durability_new_store(store: Arc<PostgresPersistenceStore>, conn_str: String) {
    // Insert via the first store instance.
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("pg-agent-durability");
    let row = make_pending_row(&ag);
    let handle_id = row.handle_id;

    pending.insert_pending(&row).expect("insert_pending must succeed");
    drop(pending);
    drop(store); // Drop the first store; the DB (container) lives on.

    // Build a BRAND NEW PostgresPersistenceStore over the SAME connection string.
    // This proves the row is DB-authoritative — no in-process state required.
    let store2 = Arc::new(
        PostgresPersistenceStore::new(&conn_str)
            .expect("second PostgresPersistenceStore must open"),
    );
    let pending2: PostgresPendingStore = store2.pending_store();

    let fetched = pending2.get_pending(handle_id).expect("get_pending on new store must not error");
    assert!(
        fetched.is_some(),
        "pending row must survive store drop and be found by a new store instance (DB-authoritative durability, Amendment 1)"
    );
    assert_eq!(fetched.unwrap().handle_id, handle_id);
}

// ── PG 16 ─────────────────────────────────────────────────────────────────────

#[test]
fn w3_pg16_pending_insert_and_get_round_trip() {
    common::with_pg("16", run_insert_get_round_trip);
}

#[test]
fn w3_pg16_pending_get_nonexistent_returns_none() {
    common::with_pg("16", run_get_nonexistent_returns_none);
}

#[test]
fn w3_pg16_pending_list_pending_by_agent() {
    common::with_pg("16", run_list_pending_by_agent);
}

#[test]
fn w3_pg16_pending_list_expired() {
    common::with_pg("16", run_list_expired);
}

#[test]
fn w3_pg16_pending_mark_resolved() {
    common::with_pg("16", run_mark_resolved);
}

/// PG 16 DB-authoritative durability: insert → drop store → open NEW store on same DB → get_pending succeeds.
/// Proves Amendment 1: pending rows survive process-state loss; only the DB matters.
#[test]
fn w3_pg16_pending_durability_new_store() {
    common::with_pg_and_conn("16", run_durability_new_store);
}

// ── PG 18 ─────────────────────────────────────────────────────────────────────

#[test]
fn w3_pg18_pending_insert_and_get_round_trip() {
    common::with_pg("18", run_insert_get_round_trip);
}

#[test]
fn w3_pg18_pending_get_nonexistent_returns_none() {
    common::with_pg("18", run_get_nonexistent_returns_none);
}

#[test]
fn w3_pg18_pending_list_pending_by_agent() {
    common::with_pg("18", run_list_pending_by_agent);
}

#[test]
fn w3_pg18_pending_list_expired() {
    common::with_pg("18", run_list_expired);
}

#[test]
fn w3_pg18_pending_mark_resolved() {
    common::with_pg("18", run_mark_resolved);
}

/// PG 18 DB-authoritative durability: insert → drop store → open NEW store on same DB → get_pending succeeds.
/// Proves Amendment 1: pending rows survive process-state loss; only the DB matters.
#[test]
fn w3_pg18_pending_durability_new_store() {
    common::with_pg_and_conn("18", run_durability_new_store);
}
