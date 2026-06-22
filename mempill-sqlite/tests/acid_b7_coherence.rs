//! ACID test — B7 temporal coherence (A25): incoherent valid-time window → Quarantined.
//!
//! B7 rule (A25): when `valid_time_confidence >= config.valid_time_confidence_threshold` (0.7),
//! the claim MUST be quarantined if EITHER:
//!   1. valid_time_start > valid_time_end (physically impossible window)
//!   2. valid_time_start > tx_time (a fact dated as valid AFTER it was learned)
//!
//! Decision path:
//!   C1 gateway stamps tx_time = Utc::now()
//!   valid_time.start = tx_time + 1 year (future start → incoherent)
//!   valid_time_confidence = 0.9 (above 0.7 threshold → coherence check runs)
//!   C7 gate adjudicate() step 2: is_temporally_incoherent() = true
//!     → GateDecision { route: Quarantine, disposition: Quarantined }
//!   The claim IS written to the DB (via append_within_txn) with disposition=Quarantined.
//!
//! DEFECT-2 (Quarantined claims surfacing as live) has been fixed:
//! `truth_engine::fold` now accepts a latest-disposition map (built from the ledger) and
//! excludes any claim whose latest disposition is Quarantined, Superseded, Invalidated, or
//! Rejected from the live set — even if no ValidityAssertion::Bound was appended.
//!
//! # What passes (B7 gate + fold logic):
//!   - `ingest_claim` returns `Disposition::Quarantined` for incoherent claims — CORRECT.
//!   - `query_memory` returns `NoBelief` for a Quarantined-only subject-line — CORRECT (DEFECT-2 FIXED).
//!   - Coherent claims are admitted as CommittedCheap — CORRECT.
//!   - Confidence threshold boundary (0.7) is enforced — CORRECT.
//!   - Quarantined claim IS in the audit ledger (parked, not destroyed) — CORRECT.

use chrono::{Duration, Utc};
use mempill_core::application::{AuditQueryRequest, IngestClaimRequest, QueryMemoryRequest};
use mempill_sqlite::open_default_in_memory;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality, Disposition,
    ExternalKind, LedgerEventKind, ProvenanceLabel, ValidTime,
};

/// B7 acid test PART 1: ingest response carries Disposition::Quarantined.
/// The gate correctly identifies the incoherent window at the ingest boundary.
#[tokio::test]
async fn acid_b7_incoherent_future_start_ingest_returns_quarantined() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-b7-ingest-agent".into());

    let future_start = Utc::now() + Duration::days(365);
    let future_end = future_start + Duration::days(30);

    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "event".into(),
            predicate: "scheduled_at".into(),
            value: serde_json::json!("conference-2027"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(future_start),
                end: Some(future_end),
                valid_time_confidence: 0.9, // above 0.7 threshold → B7 check runs
            }),
            confidence: Confidence {
                value_confidence: 0.95,
                valid_time_confidence: 0.9,
            },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("ingest must not hard-fail; claim is parked");

    // B7 gate fires at step 2 of adjudicate(): disposition must be Quarantined.
    assert_eq!(
        resp.disposition,
        Disposition::Quarantined,
        "B7 ACID gate boundary: valid_time_start AFTER tx_time with confidence 0.9 \
         MUST produce Disposition::Quarantined at ingest. Got {:?}",
        resp.disposition
    );

    println!(
        "B7 INGEST BOUNDARY PASS: incoherent claim returns Quarantined. claim_ref={}",
        resp.claim_ref.0
    );
}

/// B7 acid test PART 2: quarantined claim IS in the audit ledger (parked, not destroyed).
#[tokio::test]
async fn acid_b7_quarantined_claim_in_audit_ledger() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-b7-audit-agent".into());

    let future_start = Utc::now() + Duration::days(365);

    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "event".into(),
            predicate: "scheduled_at".into(),
            value: serde_json::json!("conference-2027"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(future_start),
                end: None,
                valid_time_confidence: 0.9,
            }),
            confidence: Confidence {
                value_confidence: 0.95,
                valid_time_confidence: 0.9,
            },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("ingest must succeed (parked)");

    assert_eq!(resp.disposition, Disposition::Quarantined);
    let quarantined_ref = resp.claim_ref.clone();

    // Quarantined claim MUST appear in the audit ledger (parked, auditable, not destroyed).
    let audit = engine
        .query_audit(AuditQueryRequest {
            agent_id: agent.clone(),
            claim_ref: Some(quarantined_ref.clone()),
            from_tx_time: None,
            limit: 100,
        })
        .await
        .expect("audit query must succeed");

    let committed_entries: Vec<_> = audit
        .entries
        .iter()
        .filter(|e| {
            e.claim_ref == quarantined_ref
                && matches!(e.event_kind, LedgerEventKind::ClaimCommitted)
        })
        .collect();

    assert_eq!(
        committed_entries.len(),
        1,
        "B7 ACID: Quarantined claim MUST appear in audit ledger as ClaimCommitted \
         (parked, not destroyed). Found {} entries.",
        committed_entries.len()
    );

    let ledger_entry = &committed_entries[0];
    assert_eq!(
        ledger_entry.disposition,
        Disposition::Quarantined,
        "B7 ACID: ClaimCommitted ledger entry MUST carry Disposition::Quarantined"
    );

    println!(
        "B7 AUDIT PASS: quarantined claim is in audit ledger. claim_ref={}, \
         ledger_disposition={:?}",
        quarantined_ref.0,
        ledger_entry.disposition
    );
}

/// B7 acid test PART 3 — DEFECT-2 fix verification.
///
/// query_memory on a subject-line with ONLY a Quarantined claim must return NoBelief.
/// DEFECT-2 fix: truth_engine::fold now filters claims whose latest ledger disposition
/// is Quarantined (or Superseded, Invalidated, Rejected) from the live set, even without
/// a ValidityAssertion::Bound.
#[tokio::test]
async fn acid_b7_quarantined_claim_excluded_from_live_fold() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-b7-defect2-agent".into());

    let future_start = Utc::now() + Duration::days(365);

    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "device".into(),
            predicate: "warranty_start".into(),
            value: serde_json::json!("future-device"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(future_start),
                end: None,
                valid_time_confidence: 0.9,
            }),
            confidence: Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.9,
            },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("ingest must not hard-fail");

    assert_eq!(
        resp.disposition,
        Disposition::Quarantined,
        "ingest response must be Quarantined (gate works)"
    );

    let q = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "device".into(),
            predicate: "warranty_start".into(),
            as_of_tx_time: None,
        })
        .await
        .expect("query must succeed");

    // DEFECT-2 FIXED: fold now inspects the latest ledger disposition and excludes
    // Quarantined claims from the live set. NoBelief is the correct response.
    assert_eq!(
        q.belief.status,
        BeliefStatus::NoBelief,
        "DEFECT-2 FIXED: query_memory must return NoBelief for a subject-line \
         with only a Quarantined claim. Got {:?}. \
         The fold correctly excludes Quarantined dispositions via the ledger map.",
        q.belief.status
    );

    println!(
        "B7 DEFECT-2 FIX PASS: Quarantined claim correctly excluded from live fold. \
         query_memory returns NoBelief as expected."
    );
}

/// B7 coherent counterpart: a past valid_time_start is ADMITTED as CommittedCheap.
/// This guard test ensures the quarantine gate is not overly aggressive.
#[tokio::test]
async fn acid_b7_coherent_past_start_is_admitted_as_live_belief() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-b7-coherent-agent".into());

    let past_start = Utc::now() - Duration::days(365);
    let past_end = Utc::now() - Duration::days(1);

    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "treatment_period".into(),
            value: serde_json::json!("chemotherapy-cycle-1"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(past_start),
                end: Some(past_end),
                valid_time_confidence: 0.9,
            }),
            confidence: Confidence {
                value_confidence: 0.95,
                valid_time_confidence: 0.9,
            },
            criticality: Criticality::High,
            derived_from: vec![],
        })
        .await
        .expect("coherent ingest must succeed");

    assert_eq!(
        resp.disposition,
        Disposition::CommittedCheap,
        "B7 ACID: coherent past valid_time window MUST be CommittedCheap, not Quarantined. \
         Got {:?}",
        resp.disposition
    );

    let q = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "treatment_period".into(),
            as_of_tx_time: None,
        })
        .await
        .expect("query must succeed");

    assert_ne!(
        q.belief.status,
        BeliefStatus::NoBelief,
        "B7 ACID: coherent External claim MUST produce a live belief (not NoBelief)"
    );

    println!(
        "ACID B7 COHERENT PASS: coherent past-start claim admitted. \
         claim_ref={}, disposition={:?}, query_status={:?}",
        resp.claim_ref.0,
        resp.disposition,
        q.belief.status
    );
}

/// Boundary test: valid_time_confidence just below 0.7 → B7 check skipped → admitted.
#[tokio::test]
async fn acid_b7_low_confidence_bypasses_coherence_check_and_is_admitted() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-b7-low-conf-agent".into());

    let future_start = Utc::now() + Duration::days(365);

    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "vacation_plan".into(),
            value: serde_json::json!("tokyo-trip"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(future_start),
                end: None,
                // 0.6 < 0.7 threshold → B7 coherence check is SKIPPED.
                valid_time_confidence: 0.6,
            }),
            confidence: Confidence {
                value_confidence: 0.8,
                valid_time_confidence: 0.6,
            },
            criticality: Criticality::Low,
            derived_from: vec![],
        })
        .await
        .expect("low-confidence ingest must succeed");

    assert_eq!(
        resp.disposition,
        Disposition::CommittedCheap,
        "B7 boundary: valid_time_confidence=0.6 < 0.7 → B7 check SKIPPED. \
         Future start MUST NOT quarantine. Got {:?}",
        resp.disposition
    );

    println!(
        "B7 LOW-CONF BOUNDARY PASS: confidence=0.6 bypasses B7. \
         disposition={:?}",
        resp.disposition
    );
}

/// Boundary test: valid_time_confidence = 0.7 exactly → B7 check RUNS → future start quarantines.
#[tokio::test]
async fn acid_b7_confidence_exactly_at_threshold_triggers_quarantine() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("acid-b7-threshold-agent".into());

    let future_start = Utc::now() + Duration::days(365);

    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "device".into(),
            predicate: "warranty_expires".into(),
            value: serde_json::json!("2027-model"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(future_start),
                end: None,
                // Exactly 0.7: the check is `< threshold`, so 0.7 is NOT below → check RUNS.
                valid_time_confidence: 0.7,
            }),
            confidence: Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.7,
            },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("threshold ingest must succeed (parked, not hard-failed)");

    assert_eq!(
        resp.disposition,
        Disposition::Quarantined,
        "B7 threshold: valid_time_confidence=0.7 → B7 check RUNS → future start quarantines. \
         Got {:?}",
        resp.disposition
    );

    println!(
        "B7 THRESHOLD PASS: confidence=0.7 triggers B7 check, future start quarantined."
    );
}
