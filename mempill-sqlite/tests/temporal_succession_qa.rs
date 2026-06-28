//! QA temporal succession test — TASK-9-W4-W5-FIX guard + TASK-11 valid-time-aware update.
//!
//! PURPOSE: verify that a clean temporal succession (non-overlapping trusted valid-time windows)
//! is NOT treated as a conflict, AND that genuine conflicts still surface as Contested.
//!
//! TASK-11 BEHAVIORAL CHANGE (test1 assertions updated):
//! With valid-time-aware conflict classification, two claims with confident NON-overlapping
//! valid-time windows are now classified as `ConflictType::Succession` → `CommittedCheap`
//! (not Contested). The read-time fold performs instant-selection: querying as-of NOW returns
//! the single claim whose window contains NOW. Querying as-of a past instant returns the
//! claim for that window.
//!
//! TEST 1: Legitimate temporal succession (non-conflicting valid-time windows, both confident)
//!   - TASK-11: Bob's ingest is now CommittedCheap (succession), NOT Contested.
//!   - Query as-of NOW → single belief Bob (Resolved, NOT Contested).
//!   - Query as-of Feb 2022 (in Alice's window) → single belief Alice (Resolved).
//!   - This is the CORRECT behavior: no silent incumbent-wins — instead, the engine
//!     recognizes the windows as authoritative and selects the appropriate claim.
//!
//! TEST 2: Genuine conflict (overlapping/identical valid-time, same subject/predicate)
//!   - Same scenario with no valid_time specified → Contested with both values.
//!   - Confirms genuine conflicts still surface correctly (I2 fallback unchanged).
//!
//! TEST 3: Acid-test scenario review — see verdict comments inline.

use chrono::Utc;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_sqlite::open_default_in_memory;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality,
    Disposition, ExternalKind, ProvenanceLabel, ValidTime,
};

// ── TEST 1: Temporal succession with non-overlapping valid-time windows (TASK-11 updated) ──
//
// Scenario: "acme" CEO was Alice valid [2020-01-01, 2024-03-01), then Bob valid
// [2024-03-01, ∞). Both ingested without an oracle. Both windows have confidence=0.9 ≥ 0.7.
//
// TASK-11 EXPECTED RESULT: The reconciler recognizes the non-overlapping trusted windows as
// a `ConflictType::Succession` → CommittedCheap (NOT Contested). The read-time fold performs
// instant-selection: NOW falls in Bob's window → single belief Bob (Resolved). A past instant
// in Feb 2022 falls in Alice's window → single belief Alice (Resolved).
//
// Pre-TASK-11 behavior (OLD): Bob was Contested because valid-time windows were ignored.
// Post-TASK-11 behavior (NEW): Bob is CommittedCheap; query selects by window.
#[tokio::test]
async fn test1_temporal_succession_non_overlapping_valid_time() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("qa-succession-agent".into());

    // Claim A: Alice was CEO, valid 2020-01-01 to 2024-03-01 (past window, closed end).
    let alice_start = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let alice_end = chrono::DateTime::parse_from_rfc3339("2024-03-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let resp_alice = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!("alice"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: Some(ValidTime {
            start: Some(alice_start),
            end: Some(alice_end),
            valid_time_confidence: 0.9,
            start_granularity: None, end_granularity: None,
        }),
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }).await.expect("alice ingest must succeed");

    println!(
        "[TEST1] alice ingest → disposition={:?}, claim_ref={}",
        resp_alice.disposition, resp_alice.claim_ref.0
    );

    // B7 check: valid_time.start (2020) < tx_time (now ~2026) and start < end → COHERENT.
    assert_eq!(
        resp_alice.disposition,
        Disposition::CommittedCheap,
        "TEST1: alice (past valid window, coherent) MUST be CommittedCheap"
    );

    // Claim B: Bob is CEO, valid from 2024-03-01 onwards (open-ended).
    // valid_time.start = 2024-03-01. tx_time = now (~2026). start < tx_time → COHERENT.
    let bob_start = chrono::DateTime::parse_from_rfc3339("2024-03-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let resp_bob = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!("bob"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: Some(ValidTime {
            start: Some(bob_start),
            end: None, // open-ended: Bob is current CEO
            valid_time_confidence: 0.9,
            start_granularity: None, end_granularity: None,
        }),
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }).await.expect("bob ingest must succeed");

    println!(
        "[TEST1] bob ingest → disposition={:?}, claim_ref={}",
        resp_bob.disposition, resp_bob.claim_ref.0
    );

    // TASK-11: Bob's windows are non-overlapping with Alice's + both confident → Succession.
    // Gate routes Succession → CheapPath / CommittedCheap (NOT Contested, NOT HeavyPath).
    assert_eq!(
        resp_bob.disposition,
        Disposition::CommittedCheap,
        "TEST1 (TASK-11): trusted non-overlapping succession MUST be CommittedCheap, not Contested"
    );

    // Query as-of NOW → NOW falls in Bob's window [2024-03-01, ∞) → single belief Bob.
    let qr_now = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None,
        valid_at: None,
    }).await.expect("query must succeed");

    println!(
        "[TEST1] query as-of NOW → status={:?}, primary={:?}, alternatives_count={}",
        qr_now.belief.status,
        qr_now.belief.primary.as_ref().map(|b| &b.fact.value),
        qr_now.belief.alternatives.len(),
    );

    // TASK-11 CRITICAL ASSERTION: fold instant-selection → single live claim Bob.
    assert_eq!(
        qr_now.belief.status, BeliefStatus::Resolved,
        "TEST1 (TASK-11): instant-selection at NOW MUST yield Resolved (Bob's window). Got {:?}",
        qr_now.belief.status
    );
    let primary_now = qr_now.belief.primary.as_ref()
        .expect("TEST1: primary belief must be present at NOW");
    assert_eq!(
        primary_now.fact.value,
        serde_json::json!("bob"),
        "TEST1 (TASK-11): primary belief at NOW MUST be Bob (his window [2024-03-01, ∞))"
    );
    assert!(
        qr_now.belief.alternatives.is_empty(),
        "TEST1 (TASK-11): no alternatives when single succession claim selected. Got: {:?}",
        qr_now.belief.alternatives
    );

    // Query with valid_at=2022-02-01 (in Alice's window [2020-01-01, 2024-03-01)).
    // Both claims were ingested "now" (tx_time ≈ current run time, well after 2022).
    // Using valid_at (independent valid-time axis) selects Alice without requiring tx-time travel.
    // Using as_of_tx_time=2022 would correctly return NoBelief because both claims
    // have tx_time > 2022 (the claim-level tx-time cutoff now filters them out).
    let alice_instant = chrono::DateTime::parse_from_rfc3339("2022-02-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let qr_alice = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None, // current tx-time view: both claims visible
        valid_at: Some(alice_instant), // D2 valid-time axis: select Alice's window
    }).await.expect("valid_at=2022 query must succeed");

    println!(
        "[TEST1] query valid_at=2022-02-01 (as_of=None) → status={:?}, primary={:?}",
        qr_alice.belief.status,
        qr_alice.belief.primary.as_ref().map(|b| &b.fact.value)
    );

    // With as_of=None (both claims visible) and valid_at=2022-02-01 (Alice's window),
    // the fold's instant-selection picks Alice: 2022-02-01 ∈ [2020-01-01, 2024-03-01).
    assert!(
        matches!(qr_alice.belief.status, BeliefStatus::Resolved | BeliefStatus::TimingUncertain),
        "TEST1 (TASK-11): valid_at=2022-02 in Alice's window must yield Resolved or TimingUncertain. Got {:?}",
        qr_alice.belief.status
    );
    assert_eq!(
        qr_alice.belief.primary.as_ref().map(|b| &b.fact.value),
        Some(&serde_json::json!("alice")),
        "TEST1: valid_at=2022-02 must select Alice (her window [2020, 2024))"
    );

    // Confirm correct claim-level tx-time cutoff: querying as_of=2022 with both claims
    // tx'd at ~NOW (>2022) correctly returns NoBelief (neither claim existed at 2022).
    let qr_tx_past = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: Some(alice_instant), // tx-time travel to 2022
        valid_at: None,
    }).await.expect("as_of_tx=2022 query must succeed");
    assert_eq!(
        qr_tx_past.belief.status,
        BeliefStatus::NoBelief,
        "TEST1: as_of_tx_time=2022 must return NoBelief — both claims were ingested ~now (tx_time > 2022)"
    );

    println!("[TEST1] OVERALL (TASK-11): Temporal succession with trusted non-overlapping windows \
              → CommittedCheap at ingest; Resolved (Bob) at NOW; valid_at selects Alice correctly. \
              No Contested state. PASS.");
}

// ── TEST 2: Genuine conflict (overlapping/identical validity, same subject/predicate) ─
//
// Scenario: two conflicting CEO claims with no valid_time (identical/unspecified validity).
// Expected: Contested with BOTH values (correct genuine conflict surfacing).
#[tokio::test]
async fn test2_genuine_conflict_overlapping_validity_contested() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("qa-conflict-agent".into());

    // Claim A: alice is CEO, no valid_time (unspecified/open-ended).
    let resp_alice = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!("alice"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }).await.expect("alice ingest must succeed");

    println!(
        "[TEST2] alice ingest → disposition={:?}, claim_ref={}",
        resp_alice.disposition, resp_alice.claim_ref.0
    );
    assert_eq!(
        resp_alice.disposition, Disposition::CommittedCheap,
        "TEST2: first External claim (alice) MUST be CommittedCheap"
    );

    // Claim B: bob is CEO, no valid_time — overlapping (identical) unspecified validity.
    let resp_bob = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!("bob"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }).await.expect("bob ingest must succeed");

    println!(
        "[TEST2] bob ingest → disposition={:?}, claim_ref={}",
        resp_bob.disposition, resp_bob.claim_ref.0
    );
    assert_eq!(
        resp_bob.disposition, Disposition::Contested,
        "TEST2: genuine conflict (no oracle, overlapping unspecified validity) MUST be Contested"
    );

    // query_memory → Contested with BOTH values.
    let qr = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None,
        valid_at: None,
    }).await.expect("query must succeed");

    let all_values: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();

    println!(
        "[TEST2] query → status={:?}, values={:?}",
        qr.belief.status, all_values
    );

    assert_eq!(
        qr.belief.status, BeliefStatus::Contested,
        "TEST2: genuine conflict MUST surface as Contested. Got {:?}", qr.belief.status
    );
    assert!(
        all_values.contains(&serde_json::json!("alice")),
        "TEST2: alice MUST be in Contested belief. Got: {all_values:?}"
    );
    assert!(
        all_values.contains(&serde_json::json!("bob")),
        "TEST2: bob MUST be in Contested belief. Got: {all_values:?}"
    );

    println!("[TEST2] PASS: genuine conflict → Contested with both values. \
              Fix did NOT regress conflict surfacing.");
}

// ── TEST 3: Sanity-check of the changed acid test scenarios ───────────────────
//
// This test does not run engine code — it is a documented review of the two changed
// acid tests to determine whether the new "Contested (both)" assertion is CORRECT.
//
// ACID_ALLERGY_RETAINED.RS — acid_allergy_supersession_succeeds_and_incumbent_retained
//   SCENARIO: patient/allergy = "penicillin" (incumbent), then "none — penicillin allergy
//   retracted" (challenger). No valid_time on either claim. No oracle.
//
//   VERDICT: GENUINE CONFLICT → CHANGE IS CORRECT.
//   Reasoning:
//   - Both claims have no valid_time (open-ended / unspecified validity).
//   - Same (subject, predicate) + different value → SameLineConflict.
//   - No oracle → B11(a) → Contested is the correct disposition.
//   - The OLD assertion ("ValidityAsserted on incumbent after ingest") was wrong:
//     it tested the pre-fix bug behavior where the incumbent was superseded at ingest.
//   - The NEW assertion ("Contested with both 'penicillin' and 'none' visible") is
//     correct: the engine cannot safely auto-accept "penicillin allergy retracted"
//     without oracle confirmation. A safety-critical allergy retraction MUST stay
//     Contested until an oracle (clinician) Affirms. The fix correctly surfaces both.
//
// ACID_ATOMIC_COMMIT.RS — i9_heavypath_supersession_commits_atomically
//   SCENARIO: user/email = "old@example.com" (claim A), then "new@example.com"
//   (claim B). No valid_time. No oracle.
//
//   VERDICT: GENUINE CONFLICT → CHANGE IS CORRECT.
//   Reasoning:
//   - Both have no valid_time (unspecified / open-ended).
//   - Same (subject, predicate) + different value → SameLineConflict.
//   - No oracle → B11(a) → Contested is correct.
//   - The ORIGINAL test comment called this "supersession" but that was the pre-fix
//     conceptual error: without oracle resolution, an email change is ambiguous
//     (typo vs. intentional update?). The engine must surface Contested.
//   - The test's original purpose was atomicity (I9): verifying no partial rows.
//     The I9 property is PRESERVED: either all rows commit or none. The disposition
//     changed from "Superseded + committed" to "Contested (both live)" which is
//     CORRECT for an oracle-absent SameLineConflict. The atomicity of the write
//     (ClaimCommitted, no ValidityAsserted at ingest) is still tested and passes.
//   - No regression: atomicity is verified; only the disposition semantics are corrected.
//
// BOTTOM LINE: Neither changed test was testing a legitimate temporal update.
// Both were testing genuinely conflicting claims (different values, no valid_time,
// no oracle). The new assertions ("Contested with both values") are semantically
// correct for oracle-absent SameLineConflicts.
#[tokio::test]
async fn test3_acid_scenario_review_verdict() {
    // This test re-runs both acid scenarios and documents findings empirically.

    // ── ACID ALLERGY SCENARIO ──────────────────────────────────────────────────
    {
        let engine = open_default_in_memory().expect("in-memory engine must open");
        let agent = AgentId("qa-review-allergy-agent".into());

        let resp1 = engine.ingest_claim(IngestClaimRequest {
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
        }).await.expect("allergy incumbent ingest must succeed");

        assert_eq!(resp1.disposition, Disposition::CommittedCheap);

        let resp2 = engine.ingest_claim(IngestClaimRequest {
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
        }).await.expect("allergy challenger ingest must succeed");

        println!(
            "[TEST3-ALLERGY] challenger disposition={:?}", resp2.disposition
        );
        assert_eq!(
            resp2.disposition, Disposition::Contested,
            "TEST3: allergy retraction with no oracle MUST be Contested (genuine conflict, no valid_time)"
        );

        let qr = engine.query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "patient".into(),
            predicate: "allergy".into(),
            as_of_tx_time: None,
        valid_at: None,
        }).await.expect("query must succeed");

        assert_eq!(qr.belief.status, BeliefStatus::Contested);
        let vals: Vec<_> = qr.belief.primary.iter()
            .map(|b| b.fact.value.clone())
            .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
            .collect();
        assert!(vals.contains(&serde_json::json!("penicillin")),
            "TEST3: penicillin MUST be visible in Contested. Got: {vals:?}");

        println!(
            "[TEST3-ALLERGY] VERDICT: genuine conflict (no valid_time, no oracle). \
             New assertion (Contested both) is CORRECT. Old assertion (ValidityAsserted \
             at ingest) was testing the pre-fix bug."
        );
    }

    // ── ACID ATOMIC COMMIT (HEAVYPATH EMAIL) SCENARIO ─────────────────────────
    {
        let engine = open_default_in_memory().expect("in-memory engine must open");
        let agent = AgentId("qa-review-atomic-agent".into());

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
        }).await.expect("email A ingest must succeed");

        assert_eq!(resp_a.disposition, Disposition::CommittedCheap);

        let resp_b = engine.ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "email".into(),
            value: serde_json::json!("new@example.com"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::High,
            derived_from: vec![],
        }).await.expect("email B ingest must succeed");

        println!(
            "[TEST3-EMAIL] challenger disposition={:?}", resp_b.disposition
        );
        assert_eq!(
            resp_b.disposition, Disposition::Contested,
            "TEST3: email update with no oracle MUST be Contested (no valid_time, genuine conflict)"
        );

        let qr = engine.query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "email".into(),
            as_of_tx_time: None,
        valid_at: None,
        }).await.expect("query must succeed");

        assert_eq!(qr.belief.status, BeliefStatus::Contested);
        let vals: Vec<_> = qr.belief.primary.iter()
            .map(|b| b.fact.value.clone())
            .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
            .collect();
        assert!(vals.contains(&serde_json::json!("old@example.com")),
            "TEST3: old email MUST be visible in Contested. Got: {vals:?}");
        assert!(vals.contains(&serde_json::json!("new@example.com")),
            "TEST3: new email MUST be visible in Contested. Got: {vals:?}");

        println!(
            "[TEST3-EMAIL] VERDICT: genuine conflict (no valid_time, no oracle). \
             New assertion (Contested both values visible) is CORRECT. \
             Old assertion (ValidityAsserted on incumbent) was testing the pre-fix bug \
             where ingest-time supersession silently dropped the incumbent."
        );
    }

    println!(
        "[TEST3] OVERALL VERDICT: both changed acid tests exercised GENUINE CONFLICTS. \
         New 'Contested (both)' assertions are CORRECT. \
         No regression introduced by the test changes."
    );
}
