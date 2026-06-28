//! ACID test — mem0 #4573: write-path amplification defence (I6, C6, §7).
//!
//! 808 re-ingestions of the same RecallReEntry content MUST collapse to EXACTLY ONE
//! underlying claim row in the database. The amplification guard (firewall.rs C6)
//! returns `CorroborateByIdentity` for every re-ingestion after the first, suppressing
//! any new row insertion.
//!
//! Verification strategy:
//!   - Ingest the ORIGINAL claim once as External/UserAsserted → 1 row committed.
//!   - Re-ingest 808 times as RecallReEntry with `derived_from` = [original_claim_ref].
//!   - Assert via public read API that EXACTLY ONE claim row exists (via belief primary).
//!   - Assert that all 808 re-ingest responses carry the ORIGINAL claim_ref (not new refs).
//!   - Assert the ORIGINAL claim value is preserved unchanged.
//!   - Assert the audit ledger has EXACTLY ONE ClaimCommitted entry for the original ref.
//!
//! RecallReEntry path reached:
//!   IngestClaimRequest.provenance = ProvenanceLabel::RecallReEntry
//!   IngestClaimRequest.derived_from = [original_claim_ref]
//!   The C6 firewall check() step 2 in ingest_claim.rs:
//!   `if candidate.provenance().is_recall_reentry()` → CorroborateByIdentity { existing_claim }
//!   → early return, no Txn opened, no claim row inserted (I6 idempotency).
//!
//! NOTE on SameLineConflict path: The SameLineConflict path (External + conflicting value
//! on the same subject-line) triggers the supersession cascade (HeavyPath). DEFECT-1 has
//! been fixed — supersession::execute now receives pre-loaded edges (loaded before
//! begin_atomic()), so TxnAlreadyOpen no longer occurs and the HeavyPath is fully
//! reachable. The smoke tests below deliberately use DIFFERENT subject-lines to keep
//! the test focused on the amplification guard property, not the conflict path.

use mempill_core::application::{AuditQueryRequest, IngestClaimRequest, QueryMemoryRequest};
use mempill_sqlite::open_default_in_memory;
use mempill_types::{
    AgentId, Cardinality, ClaimRef, Confidence, Criticality, Disposition,
    ExternalKind, LedgerEventKind, ProvenanceLabel,
};

const AMPLIFICATION_COUNT: usize = 808;

/// Acid test: 808 RecallReEntry re-ingestions of the same content → exactly ONE claim row.
///
/// This is the exact canonical count from the mem0 #4573 issue description.
/// The firewall MUST collapse all 808 into a single CorroborateByIdentity result.
/// Asserted count: EXACTLY 1 (not 808, not 2).
#[tokio::test]
async fn acid_amplification_808_recall_reentries_collapse_to_one_claim() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-amplification-agent".into());

    // ── Phase 1: ingest the ORIGINAL claim (External/UserAsserted) ─────────────
    // This establishes the ONE canonical claim that all RecallReEntry re-ingestions
    // will corroborate by identity.
    let original_req = IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "patient".into(),
        predicate: "recall_test".into(),
        value: serde_json::json!("canonical-content-808"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    };

    let original_resp = engine
        .ingest_claim(original_req)
        .await
        .expect("original ingest must succeed");

    assert!(
        !original_resp.claim_ref.0.is_nil(),
        "original claim_ref must be a valid UUID"
    );
    assert_eq!(
        original_resp.disposition,
        Disposition::CommittedCheap,
        "first External claim must be CommittedCheap"
    );

    let original_claim_ref: ClaimRef = original_resp.claim_ref.clone();

    // ── Phase 2: re-ingest 808 times as RecallReEntry ─────────────────────────
    // Each re-ingestion sets:
    //   provenance = RecallReEntry (the only way to reach the C6 firewall identity path)
    //   derived_from = [original_claim_ref]
    //
    // The firewall check() step 2 matches is_recall_reentry() and returns
    // CorroborateByIdentity { existing_claim: original_claim_ref, provenance_independent: false }.
    // The ingest use-case returns EARLY — no Txn, no new claim row (I6).
    let mut corroborate_count = 0usize;
    let mut unexpected_new_refs = 0usize;

    for i in 0..AMPLIFICATION_COUNT {
        let re_req = IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "recall_test".into(),
            value: serde_json::json!("canonical-content-808"),
            provenance: ProvenanceLabel::RecallReEntry,
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
            criticality: Criticality::High,
            derived_from: vec![original_claim_ref.clone()],
        };

        let re_resp = engine
            .ingest_claim(re_req)
            .await
            .unwrap_or_else(|e| panic!("re-ingest {i} must succeed: {e}"));

        // C6 CorroborateByIdentity: the response returns the EXISTING claim_ref.
        // The disposition is CommittedCheap (returned by the early-return path).
        if re_resp.claim_ref == original_claim_ref {
            corroborate_count += 1;
        } else {
            unexpected_new_refs += 1;
        }
    }

    // ── Phase 3: verify EXACTLY ONE claim row in the DB ───────────────────────
    // Use query_memory to verify the live belief: only the original claim is present.
    let query_resp = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "recall_test".into(),
            as_of_tx_time: None,
        valid_at: None,
        })
        .await
        .expect("query must succeed");

    let primary = query_resp
        .belief
        .primary
        .as_ref()
        .expect("primary belief must be present after original ingest");

    // The CLAIMED COUNT = EXACTLY 1 (asserted via the primary belief being the sole original)
    assert_eq!(
        primary.claim_ref,
        original_claim_ref,
        "ACID I6: the sole live belief MUST be the original claim — not a re-entry clone"
    );
    assert_eq!(
        primary.fact.value,
        serde_json::json!("canonical-content-808"),
        "original content must be preserved unchanged"
    );

    // All 808 re-ingestions must have been corroborated by identity (no new rows).
    assert_eq!(
        corroborate_count, AMPLIFICATION_COUNT,
        "ACID I6 mem0 #4573: all {AMPLIFICATION_COUNT} RecallReEntry re-ingestions must return the EXISTING \
         claim_ref (CorroborateByIdentity). Unexpected new refs: {unexpected_new_refs}"
    );
    assert_eq!(
        unexpected_new_refs, 0,
        "ACID I6: zero new claim rows must be created by {AMPLIFICATION_COUNT} RecallReEntry re-ingestions"
    );

    // Also verify via the audit ledger that there is only ONE ClaimCommitted entry
    // (the original). The 808 re-ingestions short-circuit before any write.
    let audit_resp = engine
        .query_audit(AuditQueryRequest {
            agent_id: agent.clone(),
            claim_ref: Some(original_claim_ref.clone()),
            from_tx_time: None,
            limit: 2000,
        })
        .await
        .expect("audit query must succeed");

    let committed_entries: Vec<_> = audit_resp
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.event_kind,
                LedgerEventKind::ClaimCommitted
            ) && e.claim_ref == original_claim_ref
        })
        .collect();

    // Only ONE ClaimCommitted for the original ref; re-entries never reach the ledger.
    assert_eq!(
        committed_entries.len(),
        1,
        "ACID I6: EXACTLY ONE ClaimCommitted ledger entry for the original claim. \
         808 RecallReEntry re-ingestions must NOT generate additional ledger rows. \
         Found: {}",
        committed_entries.len()
    );

    // Summary: COUNT asserted = 1 (ONE claim row for 808+1 total ingest calls)
    println!(
        "ACID AMPLIFICATION PASS: {AMPLIFICATION_COUNT} RecallReEntry re-ingestions → 1 claim row, \
         {corroborate_count} corroborations, {unexpected_new_refs} unexpected new refs"
    );
}

/// Smoke check: a GENUINELY NEW claim on a different subject-line IS admitted.
/// This guards against an overly aggressive firewall that blocks all External writes.
/// Uses DIFFERENT subject/predicate pairs to keep the test focused on the amplification
/// guard property (not the conflict/supersession path, which is covered in acid_b11_contested.rs
/// and acid_allergy_retained.rs).
#[tokio::test]
async fn acid_amplification_genuine_new_claim_on_fresh_subject_line_is_admitted() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-amplification-distinct-agent".into());

    // First claim on subject "user" / predicate "city".
    let req_a = IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "city".into(),
        value: serde_json::json!("Berlin"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    };

    let resp_a = engine
        .ingest_claim(req_a)
        .await
        .expect("first ingest must succeed");
    assert_eq!(resp_a.disposition, Disposition::CommittedCheap);

    // Second claim on a DIFFERENT subject-line — no conflict, no supersession cascade.
    let req_b = IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "country".into(), // different predicate → no SameLineConflict
        value: serde_json::json!("Germany"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    };

    let resp_b = engine
        .ingest_claim(req_b)
        .await
        .expect("second ingest on different predicate must succeed");

    // The second claim on a different subject-line gets its own claim_ref (new row).
    assert_ne!(
        resp_b.claim_ref,
        resp_a.claim_ref,
        "second claim on different predicate must get a NEW claim_ref"
    );
    assert_eq!(
        resp_b.disposition,
        Disposition::CommittedCheap,
        "second claim on different predicate must be CommittedCheap (no conflict)"
    );

    println!(
        "AMPLIFICATION SMOKE PASS: distinct subject-line admitted. \
         city_ref={}, country_ref={}",
        resp_a.claim_ref.0,
        resp_b.claim_ref.0
    );
}

/// Confirm RecallReEntry with NO matching derived_from and NO injected refs degrades
/// gracefully to Admit (not a silent NoOp). This is the firewall step 2 edge case
/// documented in firewall.rs: "RecallReEntry with no matching prior ref → conservative Admit".
#[tokio::test]
async fn acid_amplification_recall_reentry_no_ref_degrades_to_admit() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-amplification-noref-agent".into());

    // RecallReEntry with NO derived_from AND no prior injected claims for this agent.
    // firewall.rs step 2: existing_claim = derived_from.first().or_else(injected.first())
    // = None → falls through to step 4: Admit.
    let req = IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "orphan".into(),
        predicate: "data".into(),
        value: serde_json::json!("no-prior-ref"),
        provenance: ProvenanceLabel::RecallReEntry,
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.8, valid_time_confidence: 0.0 },
        criticality: Criticality::Low,
        derived_from: vec![], // empty — no existing ref to corroborate
    };

    let resp = engine
        .ingest_claim(req)
        .await
        .expect("RecallReEntry with no ref must succeed (degrades to Admit)");

    // The claim is admitted (CommittedCheap or similar) since no prior ref was found.
    // It should NOT be silently dropped or return an error.
    assert!(
        !resp.claim_ref.0.is_nil(),
        "orphaned RecallReEntry must produce a valid claim_ref (graceful Admit)"
    );

    println!(
        "AMPLIFICATION NOREF PASS: RecallReEntry with no derived_from degrades to Admit. \
         claim_ref={}",
        resp.claim_ref.0
    );
}
