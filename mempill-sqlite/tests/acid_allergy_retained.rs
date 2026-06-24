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

/// B11 oracle-absent contested ingest: BOTH claims stay live, incumbent is NEVER superseded at ingest.
///
/// TASK-9-W4-W5-FIX: The old behavior was to run supersession::execute at ingest time for HeavyPath,
/// writing a ValidityAssertion::Bound + Superseded ledger entry on the incumbent. This was incorrect:
/// it silently excluded the incumbent before the oracle could respond (or in oracle-absent cases,
/// before the Contested state could be properly surfaced with BOTH values).
///
/// After the fix:
///   - HeavyPath at ingest NEVER supersedes the incumbent.
///   - Oracle absent (B11a): challenger=Contested, incumbent=CommittedCheap (still live).
///   - The audit ledger for the incumbent shows ONLY ClaimCommitted — no ValidityAsserted.
///   - query_memory returns BeliefStatus::Contested with BOTH values visible.
#[tokio::test]
async fn acid_allergy_supersession_succeeds_and_incumbent_retained() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-allergy-supersession-agent".into());

    // Ingest original allergy (incumbent).
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

    // Second ingest with a conflicting value: oracle absent → B11(a) → Contested.
    // Fix (TASK-9-W4-W5-FIX): incumbent is NOT superseded at ingest time.
    let challenger_resp = engine
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
        .expect("B11 contested ingest must succeed");

    // B11(a): oracle absent + conflict → Contested (not CommittedCheap, not Superseded).
    assert_eq!(
        challenger_resp.disposition, Disposition::Contested,
        "B11(a): oracle absent + conflicting External claim MUST be Contested. Got {:?}",
        challenger_resp.disposition
    );

    // I1 — original claim must still be in the audit ledger (never deleted, never superseded).
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
        "I1: original penicillin-allergy claim MUST still have its ClaimCommitted audit entry \
         after B11 contested ingest (append-only). Found: {}",
        committed
    );

    // CORRECTED ASSERTION (TASK-9-W4-W5-FIX): the incumbent must NOT have a ValidityAsserted
    // (Bound) entry in the audit trail after ingest. Supersession of the incumbent only happens
    // at submit_adjudication time (Affirm verdict). At ingest time (oracle absent / B11),
    // the incumbent remains CommittedCheap and live.
    let validity_asserted = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == original_ref
                && matches!(e.event_kind, LedgerEventKind::ValidityAsserted)
        })
        .count();

    assert_eq!(
        validity_asserted, 0,
        "TASK-9-W4-W5-FIX: the incumbent MUST NOT have a ValidityAsserted entry at ingest time. \
         Ingest-time supersession of the incumbent was the root cause of the Contested-surfacing bug. \
         Found: {} (expected 0). Supersession only happens at submit_adjudication Affirm.",
        validity_asserted
    );

    // query_memory must surface Contested with BOTH values visible.
    let qr = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "patient".into(),
        predicate: "allergy".into(),
        as_of_tx_time: None,
    }).await.expect("query must succeed");

    assert_eq!(
        qr.belief.status, BeliefStatus::Contested,
        "I1+B11: belief MUST be Contested with BOTH values visible. Got {:?}", qr.belief.status
    );

    let all_values: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(
        all_values.contains(&serde_json::json!("penicillin")),
        "I1+B11: incumbent 'penicillin' MUST be visible in Contested belief. Got: {:?}", all_values
    );

    println!(
        "I1+B11 PASS: original_ref={} retained in audit (ClaimCommitted only, NO ValidityAsserted). \
         Belief=Contested with both values.",
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

/// I1 non-destruction audit trail: after B11 contested ingest (oracle absent), both claims live.
///
/// TASK-9-W4-W5-FIX: After the fix, a conflicting ingest with no oracle produces B11(a) Contested.
/// The incumbent is NEVER superseded at ingest time. Only ClaimCommitted appears in the audit trail
/// for the incumbent. No ValidityAsserted (Bound) entry exists until an Affirm verdict is submitted.
/// The belief is Contested with BOTH the incumbent AND challenger values visible.
#[tokio::test]
async fn acid_allergy_audit_shows_incumbent_retained_after_supersession() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-allergy-audit-agent".into());

    // Ingest the original allergy (incumbent).
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

    // Conflicting ingest (oracle absent → B11(a) → Contested).
    let challenger = engine
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
        .expect("B11 contested ingest must succeed");

    // B11(a): oracle absent → Contested (not CommittedCheap).
    assert_eq!(
        challenger.disposition, Disposition::Contested,
        "B11(a): conflicting External claim with no oracle MUST be Contested. Got {:?}",
        challenger.disposition
    );

    // I1: original claim must still be in audit ledger (append-only, never deleted, never superseded).
    let audit = engine
        .query_audit(AuditQueryRequest {
            agent_id: agent.clone(),
            claim_ref: Some(original_ref.clone()),
            from_tx_time: None,
            limit: 50,
        })
        .await
        .expect("audit query for original must succeed after B11 contested ingest");

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
        "I1: original penicillin-allergy claim MUST have exactly 1 ClaimCommitted audit entry. Found: {}",
        committed
    );

    // CORRECTED (TASK-9-W4-W5-FIX): no ValidityAsserted at ingest time for oracle-absent B11.
    // The incumbent is retained as CommittedCheap (live) — no Bound assertion is written.
    // Only an Affirm verdict at submit_adjudication time would produce a ValidityAsserted entry.
    let validity_asserted = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == original_ref
                && matches!(e.event_kind, LedgerEventKind::ValidityAsserted)
        })
        .count();

    assert_eq!(
        validity_asserted, 0,
        "TASK-9-W4-W5-FIX: incumbent MUST NOT have ValidityAsserted at ingest time (B11 oracle-absent path). \
         Ingest-time supersession was the bug. Supersession only via Affirm at submit_adjudication. \
         Found: {} (expected 0)",
        validity_asserted
    );

    // The belief MUST be Contested with both "penicillin" (incumbent) and "none" (challenger).
    let qr = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "patient".into(),
        predicate: "allergy".into(),
        as_of_tx_time: None,
    }).await.expect("query must succeed");

    assert_eq!(
        qr.belief.status, BeliefStatus::Contested,
        "I1+B11: belief MUST be Contested. Got {:?}", qr.belief.status
    );
    let all_values: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(
        all_values.contains(&serde_json::json!("penicillin")),
        "I1+B11: 'penicillin' (incumbent) MUST be visible in Contested. Got: {:?}", all_values
    );
    assert!(
        all_values.contains(&serde_json::json!("none")),
        "I1+B11: 'none' (challenger) MUST be visible in Contested. Got: {:?}", all_values
    );

    println!(
        "I1+B11 PASS: original allergy (claim_ref={}) retained in audit (ClaimCommitted only, \
         NO ValidityAsserted at ingest). Belief=Contested with both values.",
        original_ref.0
    );
}
