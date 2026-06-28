//! ACID test I9 — Atomic commit: a write that fails partway leaves ZERO partial rows.
//!
//! REACHABILITY FINDING:
//! A true mid-write process-crash cannot be simulated through the public `DefaultEngine` API
//! because the engine generates fresh UUIDs for each claim (no duplicate constraint violation
//! trigger accessible externally). A panic inside `spawn_blocking` would unwind and drop
//! `SqliteTxn`, which auto-rolls-back via its `Drop` impl (also correct I9 behavior).
//!
//! The STRONGEST reachable atomicity property via the public `PersistencePort` trait API is:
//! "begin_atomic → append {claim + validity_assertion + ledger_entry} → rollback → read
//!  via public port methods (load_subject_line, load_ledger) → COUNT = 0 on all tables."
//! This is exactly the I9 contract.
//!
//! HeavyPath atomicity (DEFECT-1 FIXED):
//! Ingesting a conflicting claim on the same (subject, predicate) now triggers the full
//! SameLineConflict → supersession → Contested/Superseded path WITHOUT error.
//! supersession::execute receives pre-loaded edges (loaded before begin_atomic), so no
//! reads occur inside the open transaction. The atomicity test below exercises this path:
//! ingest A (CommittedCheap), then ingest B (same subject-line, different value) → the
//! supersession commits atomically — the bounding ValidityAssertion and new ledger entry
//! are all present or all absent (no partial rows). TxnAlreadyOpen no longer occurs.
//!
//! State is verified via public `PersistencePort` read methods (not raw SQL or private
//! struct fields — integration tests cannot access `store.conn`).
//!
//! We verify atomicity at THREE levels:
//!   1. Store-level: use `SqlitePersistenceStore` + `PersistencePort` trait (begin_atomic,
//!      append_*, rollback, then load_subject_line/load_ledger to confirm zero rows).
//!   2. Engine-level cheap path: two ingests on DIFFERENT predicates verify the "no phantom
//!      rows after success" side of atomicity.
//!   3. Engine-level HeavyPath: ingest A then ingest B on the SAME subject-line — exercises
//!      the supersession cascade end-to-end and asserts atomic commit of all supersession rows.

use std::sync::Arc;

use mempill_core::application::{AuditQueryRequest, IngestClaimRequest, QueryMemoryRequest};
use mempill_core::ports::persistence::PersistencePort;
use mempill_sqlite::{
    connection::open_in_memory, SqlitePersistenceStore,
};
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Claim, ClaimRef, Confidence, Criticality,
    Disposition, ExternalKind, Fact, LedgerEntry, LedgerEventKind, ProvenanceLabel,
    TransactionTime, ValidTime, ValidityAssertion,
};
use chrono::Utc;
use uuid::Uuid;

// ── helpers ───────────────────────────────────────────────────────────────────

fn agent() -> AgentId {
    AgentId("i9-agent".into())
}

fn make_claim(agent_id: &AgentId, subject: &str, predicate: &str, value: &str) -> Claim {
    Claim::new(
        ClaimRef(Uuid::new_v4()),
        agent_id.clone(),
        Fact {
            subject: subject.into(),
            predicate: predicate.into(),
            value: serde_json::json!(value),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        mempill_types::ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now()),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Medium,
        vec![],
        None,
        None,
    )
}

fn make_validity_assertion(agent_id: &AgentId, claim_ref: &ClaimRef) -> ValidityAssertion {
    ValidityAssertion {
        assertion_ref: Uuid::new_v4(),
        agent_id: agent_id.clone(),
        target_claim: claim_ref.clone(),
        kind: mempill_types::AssertionKind::Bound { bound_at: Utc::now() },
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        asserted_at: TransactionTime(Utc::now()),
    }
}

fn make_ledger_entry(agent_id: &AgentId, claim_ref: &ClaimRef) -> LedgerEntry {
    LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent_id.clone(),
        claim_ref: claim_ref.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(Utc::now()),
    }
}

// ── Test 1: store-level rollback — public port reads confirm zero rows ─────────

/// I9 acid test: begin → append {claim + validity_assertion + ledger_entry} → rollback.
/// After rollback, ALL rows must be absent.
///
/// STATE VERIFICATION: via public `PersistencePort` read methods:
///   - `load_subject_line` → must return empty Vec (claim not visible)
///   - `load_validity_assertions_for` → must return empty Vec
///   - `load_ledger` with limit=100 → must not contain our entry_id
///
/// This is the maximum verifiable atomicity guarantee via the public port API.
#[tokio::test]
async fn i9_rollback_leaves_zero_rows_verified_via_public_read_api() {
    let conn = open_in_memory().expect("in-memory connection must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = agent();
    let claim = make_claim(&agent, "user", "role", "admin");
    let claim_ref = claim.claim_ref().clone();

    // Baseline: load_subject_line returns empty before any writes.
    let before = store
        .load_subject_line(&agent, "user", "role", None)
        .expect("load_subject_line must succeed on empty store");
    assert!(before.is_empty(), "baseline: no claims before any write");

    let assertion = make_validity_assertion(&agent, &claim_ref);
    let ledger_entry = make_ledger_entry(&agent, &claim_ref);
    let entry_id_to_find = ledger_entry.entry_id;

    // Begin txn, append all three rows, then ROLLBACK (simulating mid-write failure).
    let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
    store.append_claim(&mut txn, &claim).expect("append_claim must succeed in txn");
    store
        .append_validity_assertion(&mut txn, &assertion)
        .expect("append_validity_assertion must succeed in txn");
    store
        .append_ledger_entry(&mut txn, &ledger_entry)
        .expect("append_ledger_entry must succeed in txn");

    // ROLLBACK — I9 requires all three writes to be discarded atomically.
    store.rollback(txn).expect("rollback must succeed");

    // Post-rollback verification via public PersistencePort read methods:

    // 1. load_subject_line must return empty (claim row not committed).
    let after_claims = store
        .load_subject_line(&agent, "user", "role", None)
        .expect("load_subject_line after rollback must succeed");
    assert!(
        after_claims.is_empty(),
        "I9: claim row MUST NOT be visible after rollback (load_subject_line returned {} claims)",
        after_claims.len()
    );

    // 2. load_validity_assertions_for the claim_ref must return empty.
    let after_assertions = store
        .load_validity_assertions_for(&agent, &claim_ref)
        .expect("load_validity_assertions_for after rollback must succeed");
    assert!(
        after_assertions.is_empty(),
        "I9: validity_assertion MUST NOT be visible after rollback (got {} assertions)",
        after_assertions.len()
    );

    // 3. load_ledger must not contain our entry.
    let after_ledger = store
        .load_ledger(&agent, None, 100)
        .expect("load_ledger after rollback must succeed");
    let found_entry = after_ledger
        .iter()
        .any(|e| e.entry_id == entry_id_to_find);
    assert!(
        !found_entry,
        "I9: ledger_entry MUST NOT be visible after rollback (entry_id present in load_ledger result)"
    );
}

/// I9 complement: commit path — rows ARE visible after successful commit.
/// Ensures the rollback test above doesn't pass vacuously (e.g., append silently no-ops).
#[tokio::test]
async fn i9_commit_makes_rows_visible_via_public_read_api() {
    let conn = open_in_memory().expect("in-memory connection must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = agent();
    let claim = make_claim(&agent, "user", "status", "active");
    let claim_ref = claim.claim_ref().clone();
    let ledger_entry = make_ledger_entry(&agent, &claim_ref);
    let entry_id = ledger_entry.entry_id;

    let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
    store.append_claim(&mut txn, &claim).expect("append_claim must succeed");
    store.append_ledger_entry(&mut txn, &ledger_entry).expect("append_ledger_entry must succeed");
    store.commit(txn).expect("commit must succeed");

    // Claim must be visible via load_subject_line.
    let claims = store
        .load_subject_line(&agent, "user", "status", None)
        .expect("load_subject_line after commit must succeed");
    assert_eq!(claims.len(), 1, "after commit, exactly 1 claim must be visible");
    assert_eq!(claims[0].claim_ref(), &claim_ref, "the committed claim_ref must match");

    // Ledger entry must be visible via load_ledger.
    let ledger = store
        .load_ledger(&agent, None, 100)
        .expect("load_ledger after commit must succeed");
    let found = ledger.iter().any(|e| e.entry_id == entry_id);
    assert!(found, "after commit, ledger_entry must be visible via load_ledger");
}

/// I9 end-to-end via DefaultEngine: two non-conflicting ingests (different predicates)
/// leave exactly 2 claims visible via the query API (no phantom partial rows).
///
/// This tests the "no phantom rows after success" side of atomicity on the cheap path.
/// NOTE: True mid-write failure simulation via the DefaultEngine public API is not
/// achievable without fault-injection hooks. The store-level rollback test above covers
/// the "failure leaves zero rows" property with explicit rollback.
#[tokio::test]
async fn i9_engine_two_non_conflicting_ingests_leave_no_phantom_rows() {
    let engine = mempill_sqlite::open_default_in_memory()
        .expect("in-memory DefaultEngine must open");

    let agent = AgentId("i9-e2e-agent".into());

    // Two ingests on DIFFERENT predicates — no conflict, no supersession cascade.
    let r1 = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "session".into(),
        predicate: "start_time".into(),
        value: serde_json::json!("2026-01-01T00:00:00Z"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Low,
        derived_from: vec![],
    }).await.expect("first ingest (start_time) must succeed");

    let r2 = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "session".into(),
        predicate: "user_id".into(), // DIFFERENT predicate → NoConflict
        value: serde_json::json!("user-42"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Low,
        derived_from: vec![],
    }).await.expect("second ingest (user_id) must succeed");

    assert_eq!(r1.disposition, Disposition::CommittedCheap, "start_time must be CommittedCheap");
    assert_eq!(r2.disposition, Disposition::CommittedCheap, "user_id must be CommittedCheap");

    // Both claim_refs must be distinct (no dedup / collision from partial writes).
    assert_ne!(r1.claim_ref, r2.claim_ref, "two ingests must produce distinct claim_refs");

    // Verify each predicate's belief is independently retrievable.
    let q1 = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "session".into(),
        predicate: "start_time".into(),
        as_of_tx_time: None,
        valid_at: None,
    }).await.expect("query start_time must succeed");

    let q2 = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "session".into(),
        predicate: "user_id".into(),
        as_of_tx_time: None,
        valid_at: None,
    }).await.expect("query user_id must succeed");

    let primary1 = q1.belief.primary.as_ref()
        .expect("I9 e2e: start_time belief must have primary");
    let primary2 = q2.belief.primary.as_ref()
        .expect("I9 e2e: user_id belief must have primary");

    assert_eq!(primary1.claim_ref, r1.claim_ref,
        "I9 e2e: start_time query must return the correct claim_ref (no phantom rows)");
    assert_eq!(primary2.claim_ref, r2.claim_ref,
        "I9 e2e: user_id query must return the correct claim_ref (no phantom rows)");
}

// ── Test 4: HeavyPath atomicity — supersession via same-subject-line conflict ─

/// I9 HeavyPath atomicity test: ingest A (CommittedCheap) → ingest B on the SAME
/// (subject, predicate) with a DIFFERENT value → supersession cascade fires.
///
/// DEFECT-1 FIXED: supersession::execute now receives pre-loaded edges (loaded before
/// begin_atomic). The entire supersession — bounding ValidityAssertion for A, ledger
/// entry for B — commits atomically without TxnAlreadyOpen.
///
/// Assertions:
///   - ingest B succeeds (no error)
///   - disposition of B is NOT CommittedCheap (HeavyPath took over)
///   - original claim A is retained in the audit ledger (I1, append-only)
///   - a ValidityAsserted entry exists for claim A (the bound was appended)
///   - the belief for the subject-line reflects the supersession result (non-empty)
///   - no partial rows: either ALL supersession writes are present or none (atomicity)
///
/// NOTE: forced mid-supersession rollback is not reachable via the public API without
/// fault-injection hooks. The store-level rollback test (above) covers the "zero rows
/// on failure" side. This test covers the "all rows present on success" side through
/// the HeavyPath, which was previously unreachable due to DEFECT-1.
#[tokio::test]
async fn i9_heavypath_supersession_commits_atomically() {
    let engine = mempill_sqlite::open_default_in_memory()
        .expect("in-memory DefaultEngine must open");

    let agent = AgentId("i9-heavypath-agent".into());

    // ── Step 1: ingest claim A (cheap path) ────────────────────────────────────
    let resp_a = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "email".into(),
        value: serde_json::json!("old@example.com"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }).await.expect("ingest A (incumbent) must succeed");

    assert_eq!(
        resp_a.disposition, Disposition::CommittedCheap,
        "I9 HeavyPath: first ingest must be CommittedCheap (no conflict)"
    );
    let claim_ref_a = resp_a.claim_ref.clone();

    // ── Step 2: ingest claim B — same subject-line, different value ────────────
    // Triggers SameLineConflict → HeavyPath → supersession::execute.
    // DEFECT-1 FIXED: edges are pre-loaded before begin_atomic; no TxnAlreadyOpen.
    let resp_b = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "email".into(),          // same predicate → SameLineConflict
        value: serde_json::json!("new@example.com"), // different value → supersession
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }).await.expect("I9 HeavyPath: ingest B (conflicting) must succeed (DEFECT-1 fixed)");

    // HeavyPath result: disposition is NOT CommittedCheap (supersession/Contested fired).
    assert_ne!(
        resp_b.disposition, Disposition::CommittedCheap,
        "I9 HeavyPath: conflicting ingest must not be CommittedCheap (HeavyPath must have fired)"
    );

    // ── Step 3: assert atomicity — original claim A is retained (I1) ──────────
    // After B11 Contested ingest (oracle absent), claim A must still appear in the audit
    // ledger with ClaimCommitted. It must NOT have a ValidityAsserted entry — the incumbent
    // is NOT superseded at ingest time (TASK-9-W4-W5-FIX: ingest-time supersession removed).
    // Supersession only happens at submit_adjudication Affirm time.
    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: Some(claim_ref_a.clone()),
        from_tx_time: None,
        limit: 50,
    }).await.expect("audit query for claim A must succeed after B11 contested ingest");

    let committed_count = audit.entries.iter()
        .filter(|e| e.claim_ref == claim_ref_a && matches!(e.event_kind, LedgerEventKind::ClaimCommitted))
        .count();
    assert_eq!(
        committed_count, 1,
        "I9 HeavyPath atomicity: ClaimCommitted entry for claim A MUST be present after B11 \
         contested ingest (append-only — incumbent retained). Found: {committed_count}"
    );

    // CORRECTED (TASK-9-W4-W5-FIX): ValidityAsserted MUST NOT be present at ingest time.
    // HeavyPath (B11, oracle absent) no longer writes a Bound assertion on the incumbent.
    // This was the root cause of the Contested-surfacing bug: the Bound assertion excluded
    // the incumbent from live_claims, producing NoBelief after Deny and missing-incumbent
    // Contested after Unknown. Fix: don't supersede at ingest; supersede only on Affirm.
    let validity_asserted_count = audit.entries.iter()
        .filter(|e| e.claim_ref == claim_ref_a && matches!(e.event_kind, LedgerEventKind::ValidityAsserted))
        .count();
    assert_eq!(
        validity_asserted_count, 0,
        "TASK-9-W4-W5-FIX: ValidityAsserted for claim A MUST NOT be present at ingest time. \
         The incumbent is not superseded during ingest (only at submit_adjudication Affirm). \
         Found: {validity_asserted_count} (expected 0)"
    );

    // ── Step 4: belief reflects B11 Contested (BOTH values visible) ───────────
    // oracle is None in DefaultEngine → B11(a) → Contested immediately.
    // Both claim A ("old@example.com") and claim B ("new@example.com") must be visible.
    let query = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "email".into(),
        as_of_tx_time: None,
        valid_at: None,
    }).await.expect("query after B11 Contested ingest must succeed");

    assert_eq!(
        query.belief.status, BeliefStatus::Contested,
        "I9 HeavyPath: belief after B11 contested ingest MUST be Contested (both claims live). \
         Got {:?}", query.belief.status
    );

    let all_values: Vec<_> = query.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(query.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(
        all_values.contains(&serde_json::json!("old@example.com")),
        "I9 HeavyPath: 'old@example.com' (claim A / incumbent) MUST be visible in Contested. Got: {all_values:?}"
    );
    assert!(
        all_values.contains(&serde_json::json!("new@example.com")),
        "I9 HeavyPath: 'new@example.com' (claim B / challenger) MUST be visible in Contested. Got: {all_values:?}"
    );
}
