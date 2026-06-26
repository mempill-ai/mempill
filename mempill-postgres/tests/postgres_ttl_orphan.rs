//! W6 TTL/sweep Postgres live tests — `mark_expired` and `list_queued_orphan_claims`.
//!
//! Proves the two W6 methods that were never run against a real Postgres DB:
//!   1. `mark_expired` — sets status to 'expired'.
//!   2. `list_queued_orphan_claims` — correlated subquery that finds QueuedForAdjudication
//!      claims with no matching pending row; returns OrphanedQueuedClaim.
//!
//! Both tests run against live PG 16 and PG 18 containers via testcontainers.
//!
//! Test catalogue (6 tests × 2 PG versions = 12 total):
//!   w6_pg{16,18}_mark_expired_sets_status
//!   w6_pg{16,18}_list_queued_orphan_present
//!   w6_pg{16,18}_list_queued_orphan_not_present

mod common;

use std::sync::Arc;

use chrono::Utc;
use mempill_core::ports::{
    pending_adjudication::{PendingAdjudicationPort, PendingAdjudicationRow},
    PersistencePort,
};
use mempill_postgres::{PostgresPendingStore, PostgresPersistenceStore};
use mempill_types::{
    AdjudicationRequest, AgentId, Belief, Cardinality, Claim, ClaimRef, Confidence, Criticality,
    CurrencySignal, CurrencyState, ExternalAnchor, ExternalKind, Fact, LedgerEntry,
    LedgerEventKind, OverturnReason, ProvenanceLabel, SubjectLineRef, TransactionTime, ValidTime,
};
use uuid::Uuid;

// ── Builder helpers ───────────────────────────────────────────────────────────

fn agent(name: &str) -> AgentId {
    AgentId(name.into())
}

fn make_adj_request(ag: &AgentId) -> AdjudicationRequest {
    let now = TransactionTime(Utc::now());
    AdjudicationRequest {
        subject_line: SubjectLineRef {
            agent_id: ag.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
        },
        incumbent: Belief {
            claim_ref: ClaimRef(Uuid::new_v4()),
            fact: Fact {
                subject: "acme".into(),
                predicate: "ceo".into(),
                value: serde_json::json!("alice"),
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            transaction_time: now.clone(),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            currency_signal: CurrencySignal {
                last_refreshed_at: now.clone(),
                state: CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: Criticality::High,
        },
        challenger: Claim::new(
            ClaimRef(Uuid::new_v4()),
            ag.clone(),
            Fact {
                subject: "acme".into(),
                predicate: "ceo".into(),
                value: serde_json::json!("bob"),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::High,
            vec![],
            None,
            None,
        ),
        criticality: Criticality::High,
        reason: OverturnReason::ExternalContradiction,
    }
}

fn make_pending_row(ag: &AgentId, challenger_ref: ClaimRef, incumbent_ref: ClaimRef) -> PendingAdjudicationRow {
    PendingAdjudicationRow {
        handle_id: Uuid::new_v4(),
        agent_id: ag.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        challenger_claim_ref: challenger_ref,
        incumbent_claim_ref: incumbent_ref,
        request_payload: make_adj_request(ag),
        queued_at: Utc::now(),
        expires_at: None,
        status: "pending".into(),
    }
}

/// Insert a claim + its first ledger entry in a single transaction.
fn seed_claim_with_disposition(
    store: &PostgresPersistenceStore,
    ag: &AgentId,
    claim: &Claim,
    disposition: mempill_types::Disposition,
    event_kind: LedgerEventKind,
) {
    let mut txn = store
        .begin_atomic(ag)
        .expect("begin_atomic must succeed");
    store
        .append_claim(&mut txn, claim)
        .expect("append_claim must succeed");
    store
        .append_ledger_entry(
            &mut txn,
            &LedgerEntry {
                entry_id: Uuid::new_v4(),
                agent_id: ag.clone(),
                claim_ref: claim.claim_ref().clone(),
                event_kind,
                disposition,
                rationale: None,
                recorded_at: TransactionTime(claim.transaction_time().0),
            },
        )
        .expect("append_ledger_entry must succeed");
    store.commit(txn).expect("commit must succeed");
}

fn make_claim(ag: &AgentId, value: &str, ts_offset_secs: i64) -> Claim {
    let now = Utc::now() + chrono::Duration::seconds(ts_offset_secs);
    Claim::new(
        ClaimRef(Uuid::new_v4()),
        ag.clone(),
        Fact {
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!(value),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(now),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::High,
        vec![],
        None,
        None,
    )
}

// ── Test 1: mark_expired sets status to 'expired' ────────────────────────────

fn run_mark_expired_sets_status(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("w6-mark-expired-agent");

    let challenger_ref = ClaimRef(Uuid::new_v4());
    let incumbent_ref = ClaimRef(Uuid::new_v4());
    let row = make_pending_row(&ag, challenger_ref, incumbent_ref);
    let handle_id = row.handle_id;

    pending.insert_pending(&row).expect("insert_pending must succeed");

    // Confirm it starts as 'pending' and appears in list_pending.
    let before = pending.list_pending(Some(&ag)).expect("list_pending must succeed");
    assert_eq!(before.len(), 1, "must have 1 pending row before mark_expired");
    assert_eq!(before[0].status, "pending");

    // Mark expired.
    pending.mark_expired(handle_id).expect("mark_expired must succeed");

    // get_pending must still find the row (status = 'expired').
    let fetched = pending
        .get_pending(handle_id)
        .expect("get_pending must not error")
        .expect("row must still exist after mark_expired");
    assert_eq!(fetched.status, "expired", "status must be 'expired' after mark_expired");

    // list_pending (which filters status='pending') must NOT return it.
    let after = pending.list_pending(Some(&ag)).expect("list_pending must succeed");
    assert!(
        after.is_empty(),
        "expired row must not appear in list_pending (status='pending' filter)"
    );

    // list_expired must also NOT return it (its status is now 'expired' but
    // list_expired filters status='pending' — the row is no longer sweepable).
    let expired = pending
        .list_expired(Utc::now())
        .expect("list_expired must succeed");
    assert!(
        expired.is_empty(),
        "already-expired row must not re-appear in list_expired"
    );
}

// ── Test 2: list_queued_orphan_claims — orphan IS present ────────────────────

fn run_list_queued_orphan_present(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("w6-orphan-present-agent");

    // Seed incumbent (CommittedCheap, earlier tx_time).
    let incumbent_claim = make_claim(&ag, "alice", -10);
    let incumbent_ref = incumbent_claim.claim_ref().clone();
    seed_claim_with_disposition(
        &store,
        &ag,
        &incumbent_claim,
        mempill_types::Disposition::CommittedCheap,
        LedgerEventKind::ClaimCommitted,
    );

    // Seed challenger (QueuedForAdjudication, later tx_time) — NO pending row.
    let challenger_claim = make_claim(&ag, "bob", 0);
    let challenger_ref = challenger_claim.claim_ref().clone();
    seed_claim_with_disposition(
        &store,
        &ag,
        &challenger_claim,
        mempill_types::Disposition::QueuedForAdjudication,
        LedgerEventKind::ClaimCommitted,
    );

    // No pending row inserted — this is the orphan scenario.

    let orphans = pending
        .list_queued_orphan_claims()
        .expect("list_queued_orphan_claims must succeed");

    assert_eq!(
        orphans.len(),
        1,
        "must detect exactly 1 orphaned QueuedForAdjudication claim; got {:?}",
        orphans.iter().map(|o| &o.challenger_claim_ref).collect::<Vec<_>>()
    );

    let orphan = &orphans[0];
    assert_eq!(
        orphan.challenger_claim_ref, challenger_ref,
        "orphan challenger_claim_ref must match the seeded challenger"
    );
    assert_eq!(
        orphan.incumbent_claim_ref,
        Some(incumbent_ref),
        "orphan incumbent_claim_ref must identify the CommittedCheap incumbent"
    );
    assert_eq!(orphan.agent_id, ag);
    assert_eq!(orphan.subject, "acme");
    assert_eq!(orphan.predicate, "ceo");
}

// ── Test 3: list_queued_orphan_claims — NOT an orphan (pending row exists) ────

fn run_list_queued_orphan_not_present(store: Arc<PostgresPersistenceStore>) {
    let pending: PostgresPendingStore = store.pending_store();
    let ag = agent("w6-orphan-absent-agent");

    // Seed incumbent (CommittedCheap).
    let incumbent_claim = make_claim(&ag, "alice", -10);
    let incumbent_ref = incumbent_claim.claim_ref().clone();
    seed_claim_with_disposition(
        &store,
        &ag,
        &incumbent_claim,
        mempill_types::Disposition::CommittedCheap,
        LedgerEventKind::ClaimCommitted,
    );

    // Seed challenger (QueuedForAdjudication).
    let challenger_claim = make_claim(&ag, "bob", 0);
    let challenger_ref = challenger_claim.claim_ref().clone();
    seed_claim_with_disposition(
        &store,
        &ag,
        &challenger_claim,
        mempill_types::Disposition::QueuedForAdjudication,
        LedgerEventKind::ClaimCommitted,
    );

    // Insert a matching pending row with status='pending' — NOT an orphan.
    let row = make_pending_row(&ag, challenger_ref.clone(), incumbent_ref.clone());
    pending.insert_pending(&row).expect("insert_pending must succeed");

    let orphans = pending
        .list_queued_orphan_claims()
        .expect("list_queued_orphan_claims must succeed");

    assert!(
        orphans.is_empty(),
        "QueuedForAdjudication claim WITH a matching pending row must NOT appear as orphan; got {:?}",
        orphans.iter().map(|o| &o.challenger_claim_ref).collect::<Vec<_>>()
    );

    // Additional sub-case: a Contested claim (resolved/Contested disposition) must also not appear.
    let ag2 = agent("w6-orphan-contested-agent");
    let contested_claim = make_claim(&ag2, "carol", 0);
    seed_claim_with_disposition(
        &store,
        &ag2,
        &contested_claim,
        mempill_types::Disposition::Contested,
        LedgerEventKind::AdjudicationResolved,
    );

    // list_queued_orphan_claims scans for QueuedForAdjudication only — Contested must be absent.
    let orphans2 = pending
        .list_queued_orphan_claims()
        .expect("list_queued_orphan_claims must succeed (second check)");

    let contested_appears = orphans2
        .iter()
        .any(|o| o.challenger_claim_ref == *contested_claim.claim_ref());
    assert!(
        !contested_appears,
        "Contested claim must NOT appear in list_queued_orphan_claims"
    );
}

// ── PG 16 ─────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn w6_pg16_mark_expired_sets_status() {
    common::with_pg("16", run_mark_expired_sets_status);
}

#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn w6_pg16_list_queued_orphan_present() {
    common::with_pg("16", run_list_queued_orphan_present);
}

#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn w6_pg16_list_queued_orphan_not_present() {
    common::with_pg("16", run_list_queued_orphan_not_present);
}

// ── PG 18 ─────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn w6_pg18_mark_expired_sets_status() {
    common::with_pg("18", run_mark_expired_sets_status);
}

#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn w6_pg18_list_queued_orphan_present() {
    common::with_pg("18", run_list_queued_orphan_present);
}

#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn w6_pg18_list_queued_orphan_not_present() {
    common::with_pg("18", run_list_queued_orphan_not_present);
}
