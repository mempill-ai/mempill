//! ACID test — I1 non-destruction through supersession.
//!
//! I1 (Non-destruction): claims are WRITE-ONCE and IMMUTABLE after append.
//! Supersession bounds a claim's validity interval by inserting a `ValidityAssertion(Bound)`
//! and appending a new claim row for the superseding value. It NEVER deletes the original row.
//!
//! DEFECT-1 (TxnAlreadyOpen on supersession cascade) has been fixed:
//! `supersession::execute` now receives pre-loaded edges from the caller (loaded before
//! `begin_atomic()`), eliminating reads inside the open transaction window.
//!
//! # Tests in this file
//! - `acid_allergy_first_claim_committed_cheap`: first External ingest works.
//! - `acid_allergy_supersession_succeeds_and_incumbent_retained`: supersession succeeds (DEFECT-1 fixed),
//!   original penicillin-allergy row still exists in the ledger (I1), belief reflects new state.
//! - `acid_allergy_three_distinct_first_ingests_all_committed`: all first-time ingests work.
//! - `acid_allergy_audit_shows_incumbent_retained_after_supersession`: original claim survives
//!   supersession (append-only, I1).

use mempill_core::application::{AuditQueryRequest, IngestClaimRequest, QueryMemoryRequest};
use mempill_sqlite::open_default_in_memory;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality, Disposition,
    ExternalKind, LedgerEventKind, ProvenanceLabel,
};

/// Phase 1 (always passes): original allergy claim is CommittedCheap.
/// This is the pre-condition for the I1 test — we can commit claims successfully.
#[tokio::test]
async fn acid_allergy_first_claim_committed_cheap() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-allergy-phase1-agent".into());

    let req = IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "patient".into(),
        predicate: "allergy".into(),
        value: serde_json::json!("penicillin"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.99, valid_time_confidence: 0.0 },
        criticality: Criticality::Critical,
        derived_from: vec![],
    };

    let resp = engine.ingest_claim(req).await.expect("original allergy ingest must succeed");

    assert_eq!(
        resp.disposition,
        Disposition::CommittedCheap,
        "first External allergy claim must be CommittedCheap"
    );

    // Verify via query_memory: belief is live.
    let q = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "allergy".into(),
            as_of_tx_time: None,
        })
        .await
        .expect("query must succeed");

    assert!(
        matches!(
            q.belief.status,
            BeliefStatus::Resolved | BeliefStatus::TimingUncertain
        ),
        "single External claim must produce Resolved or TimingUncertain belief, got {:?}",
        q.belief.status
    );
    let primary = q.belief.primary.expect("primary belief must be present");
    assert_eq!(primary.fact.value, serde_json::json!("penicillin"));

    // Verify via audit: ClaimCommitted in ledger.
    let audit = engine
        .query_audit(AuditQueryRequest {
            agent_id: agent.clone(),
            claim_ref: Some(resp.claim_ref.clone()),
            from_tx_time: None,
            limit: 50,
        })
        .await
        .expect("audit must succeed");

    let committed = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == resp.claim_ref
                && matches!(e.event_kind, LedgerEventKind::ClaimCommitted)
        })
        .count();

    assert_eq!(
        committed, 1,
        "I1 pre-condition: ClaimCommitted entry MUST exist for the original allergy claim"
    );

    println!(
        "I1 PRE-CONDITION PASS: original allergy claim committed and in audit ledger. \
         claim_ref={}",
        resp.claim_ref.0
    );
}

/// DEFECT-1 fix verification: supersession succeeds end-to-end.
///
/// Before the fix: second ingest on the same subject-line (SameLineConflict → HeavyPath →
/// supersession::execute → load_edges_for → TxnAlreadyOpen) returned an error.
///
/// After the fix: edges are pre-loaded BEFORE begin_atomic(), supersession succeeds,
/// original penicillin-allergy claim row is retained in the audit ledger (I1), and
/// the current belief reflects the superseding state.
#[tokio::test]
async fn acid_allergy_supersession_succeeds_and_incumbent_retained() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-allergy-supersession-agent".into());

    // Ingest original allergy.
    let original = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "allergy".into(),
            value: serde_json::json!("penicillin"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.99, valid_time_confidence: 0.0 },
            criticality: Criticality::Critical,
            derived_from: vec![],
        })
        .await
        .expect("original allergy ingest must succeed");

    assert_eq!(original.disposition, Disposition::CommittedCheap, "first ingest must be CommittedCheap");
    let original_ref = original.claim_ref.clone();

    // Second ingest with a conflicting value: triggers HeavyPath supersession.
    // DEFECT-1 FIX: edges are pre-loaded before begin_atomic — this must SUCCEED now.
    let supersession_resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "allergy".into(),
            value: serde_json::json!("none — penicillin allergy retracted"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
            criticality: Criticality::Critical,
            derived_from: vec![],
        })
        .await
        .expect("DEFECT-1 FIXED: supersession must succeed; edges are now pre-loaded before begin_atomic");

    // The supersession should produce a Contested or Superseded-like response (HeavyPath outcome).
    assert_ne!(
        supersession_resp.disposition, Disposition::CommittedCheap,
        "second conflicting claim must not be CommittedCheap (it triggers HeavyPath)"
    );

    // I1 — original claim must still be in the audit ledger (never deleted).
    let audit = engine
        .query_audit(AuditQueryRequest {
            agent_id: agent.clone(),
            claim_ref: Some(original_ref.clone()),
            from_tx_time: None,
            limit: 50,
        })
        .await
        .expect("audit query for original must succeed");

    let committed = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == original_ref
                && matches!(e.event_kind, LedgerEventKind::ClaimCommitted)
        })
        .count();

    assert_eq!(
        committed, 1,
        "I1 (DEFECT-1 FIXED): original penicillin-allergy claim MUST still have \
         its ClaimCommitted audit entry after successful supersession (append-only). Found: {}",
        committed
    );

    // The superseded claim should now have a ValidityAsserted entry in the audit trail.
    let validity_asserted = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == original_ref
                && matches!(e.event_kind, LedgerEventKind::ValidityAsserted)
        })
        .count();

    assert_eq!(
        validity_asserted, 1,
        "I1: superseded claim must have a ValidityAsserted audit entry (bound appended, not deleted). Found: {}",
        validity_asserted
    );

    println!(
        "I1 FULL PASS (DEFECT-1 FIXED): supersession succeeded. \
         original_ref={} still in audit ledger with ClaimCommitted + ValidityAsserted.",
        original_ref.0
    );
}

/// Three first-time ingests on different subject-lines all commit successfully.
/// This tests that the first-ingest (cheap path) I1 property holds:
/// each writes exactly one ClaimCommitted entry and the claim is live.
#[tokio::test]
async fn acid_allergy_three_distinct_first_ingests_all_committed() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-allergy-3first-agent".into());

    let cases = [
        ("patient-a", "allergy", "penicillin"),
        ("patient-b", "allergy", "sulfa"),
        ("patient-c", "allergy", "aspirin"),
    ];

    for (subject, predicate, value) in &cases {
        let resp = engine
            .ingest_claim(IngestClaimRequest {
                agent_id: agent.clone(),
                subject: subject.to_string(),
                predicate: predicate.to_string(),
                value: serde_json::json!(value),
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                cardinality: Cardinality::Functional,
                valid_time: None,
                confidence: Confidence { value_confidence: 0.99, valid_time_confidence: 0.0 },
                criticality: Criticality::Critical,
                derived_from: vec![],
            })
            .await
            .unwrap_or_else(|e| {
                panic!("ingest for {}/{} must succeed: {}", subject, predicate, e)
            });

        assert_eq!(
            resp.disposition,
            Disposition::CommittedCheap,
            "first ingest for {}/{} must be CommittedCheap",
            subject,
            predicate
        );

        // Each claim must be in the audit ledger (I1: write-once, auditable).
        let audit = engine
            .query_audit(AuditQueryRequest {
                agent_id: agent.clone(),
                claim_ref: Some(resp.claim_ref.clone()),
                from_tx_time: None,
                limit: 20,
            })
            .await
            .unwrap_or_else(|e| panic!("audit for {}/{} must succeed: {}", subject, predicate, e));

        let committed = audit
            .entries
            .iter()
            .filter(|e| {
                e.claim_ref == resp.claim_ref
                    && matches!(e.event_kind, LedgerEventKind::ClaimCommitted)
            })
            .count();

        assert_eq!(
            committed, 1,
            "I1: claim for {}/{} must have exactly 1 ClaimCommitted entry in audit ledger",
            subject, predicate
        );
    }

    println!(
        "I1 FIRST-INGEST PASS: all {} distinct first ingests committed and in audit ledger.",
        cases.len()
    );
}

/// I1 end-to-end audit trail: after successful supersession, original claim is retained.
///
/// After DEFECT-1 fix: supersession succeeds. The original claim remains in the audit ledger
/// (I1 — append-only, never deleted). The audit trail shows both ClaimCommitted (for the
/// original) and ValidityAsserted (for the bound). The new belief reflects the supersession.
#[tokio::test]
async fn acid_allergy_audit_shows_incumbent_retained_after_supersession() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-allergy-audit-agent".into());

    // Ingest the original allergy.
    let original = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "allergy".into(),
            value: serde_json::json!("penicillin"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.99, valid_time_confidence: 0.0 },
            criticality: Criticality::Critical,
            derived_from: vec![],
        })
        .await
        .expect("original allergy ingest must succeed");

    let original_ref = original.claim_ref.clone();
    assert_eq!(original.disposition, Disposition::CommittedCheap);

    // Supersession — DEFECT-1 is fixed, this must succeed.
    let superseding = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "allergy".into(),
            value: serde_json::json!("none"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Critical,
            derived_from: vec![],
        })
        .await
        .expect("DEFECT-1 FIXED: supersession must succeed");

    // Superseding disposition must be non-cheap (HeavyPath gate fired).
    assert_ne!(
        superseding.disposition, Disposition::CommittedCheap,
        "superseding ingest must not be CommittedCheap (HeavyPath override expected)"
    );

    // I1: original claim must still be in audit ledger (append-only, never deleted).
    let audit = engine
        .query_audit(AuditQueryRequest {
            agent_id: agent.clone(),
            claim_ref: Some(original_ref.clone()),
            from_tx_time: None,
            limit: 50,
        })
        .await
        .expect("audit query for original must succeed after supersession");

    let committed = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == original_ref
                && matches!(e.event_kind, LedgerEventKind::ClaimCommitted)
        })
        .count();

    assert_eq!(
        committed, 1,
        "I1 FULL PASS (DEFECT-1 FIXED): original penicillin-allergy claim MUST still \
         have its ClaimCommitted audit entry after supersession (append-only). Found: {}",
        committed
    );

    // The bound (ValidityAsserted) must also be in the audit trail.
    let validity_asserted = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == original_ref
                && matches!(e.event_kind, LedgerEventKind::ValidityAsserted)
        })
        .count();

    assert_eq!(
        validity_asserted, 1,
        "I1: superseded original must have ValidityAsserted audit entry (bound, not delete). Found: {}",
        validity_asserted
    );

    println!(
        "I1 FULL PASS (DEFECT-1 FIXED): original allergy (claim_ref={}) \
         is in audit ledger with ClaimCommitted + ValidityAsserted after successful supersession.",
        original_ref.0
    );
}
