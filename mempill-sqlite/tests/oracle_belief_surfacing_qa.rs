//! QA: Oracle resolution belief-surfacing tests (TASK-9-W4-W5-QA).
//!
//! These tests verify what a user ACTUALLY SEES via `query_memory` across the full
//! oracle resolution lifecycle. The engineer's existing `oracle_resolution_e2e.rs` tests
//! only assert ledger dispositions — they never call `query_memory`.
//!
//! This file answers the question: after each oracle verdict, does the SURFACED BELIEF
//! (the value returned to a user) match the mempill principle that Contested MUST be
//! surfaced rather than silently overwriting?
//!
//! The suspected bug (engineer-flagged): the heavy-path (HeavyPath) ALREADY RUNS
//! `supersession::execute` during INGEST when oracle IS present, writing a Bound
//! ValidityAssertion on the incumbent and marking it Superseded. After a Deny verdict,
//! `submit_adjudication` supersedes only the challenger, but the incumbent's Bound
//! assertion is never removed (append-only I1). So the fold sees an incumbent that is
//! bounded + Superseded → excluded from live_claims → `query_memory` returns NoBelief
//! or only the now-Superseded challenger, rather than the incumbent ("alice").
//!
//! # Scenarios covered
//!
//! 1. QUEUED   — before any submit: must surface Contested (both "alice" + "bob").
//! 2. AFFIRM   — challenger ("bob") wins: must surface "bob" as Resolved/TimingUncertain.
//! 3. DENY     — incumbent ("alice") stands: must surface "alice" (not empty, not "bob").
//! 4. UNKNOWN  — oracle abstains: must surface Contested (both still visible).
//! 5. B11-ABSENT — oracle absent regression: must surface Contested immediately.

use std::sync::Arc;

use mempill_core::{
    application::{IngestClaimRequest, QueryMemoryRequest},
    engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter},
    ports::OraclePort,
    EngineConfig, EngineHandle,
};
use mempill_sqlite::{
    connection::open_in_memory,
    store::SqlitePersistenceStore,
};
use mempill_types::{
    AgentId, AdjudicationResponse, AdjudicationVerdict, BeliefStatus, Cardinality,
    Confidence, Criticality, ExternalKind, ProvenanceLabel,
};

// ── TestOracle (same pattern as oracle_resolution_e2e.rs) ────────────────────

struct TestOracle {
    fixed_uuid: uuid::Uuid,
}

impl OraclePort for TestOracle {
    type Error = mempill_core::noop::NoOpError;
    type Handle = uuid::Uuid;

    fn request_adjudication(
        &self,
        _agent_id: &AgentId,
        _request: mempill_types::AdjudicationRequest,
    ) -> Result<Self::Handle, Self::Error> {
        Ok(self.fixed_uuid)
    }

    fn handle_to_uuid(handle: &Self::Handle) -> uuid::Uuid {
        *handle
    }
}

// ── Engine builder (same pattern as oracle_resolution_e2e.rs) ────────────────

fn build_oracle_engine(
    oracle_uuid: uuid::Uuid,
) -> EngineHandle<SqlitePersistenceStore, TestOracle, mempill_core::NoOpVector> {
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: oracle_uuid });

    EngineHandle::<_, _, mempill_core::NoOpVector>::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        EngineConfig::default(),
    )
}

fn ingest_req(agent: &AgentId, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }
}

fn query_req(agent: &AgentId) -> QueryMemoryRequest {
    QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None,
    }
}

// ── Scenario 1: QUEUED — before submit, query_memory must surface Contested ──

/// SCENARIO 1 (QUEUED — before submit).
///
/// After ingesting incumbent "alice" (CommittedCheap) and challenger "bob"
/// (QueuedForAdjudication), BEFORE any oracle verdict is submitted:
///
/// EXPECTED: BeliefStatus::Contested (both claims visible; NO silent pick).
/// SUSPECTED BUG: The heavy-path runs supersession::execute at ingest time, bounding
/// the incumbent with a ValidityAssertion::Bound and writing Superseded to the ledger.
/// The fold's is_non_live_disposition filter excludes Superseded claims. This means
/// the incumbent "alice" may NOT appear in live_claims even before any submit.
/// If only "bob" (QueuedForAdjudication, not excluded by is_non_live_disposition) is
/// live, the fold reports has_conflict=false and projection returns TimingUncertain/"bob",
/// violating the Contested-surfacing principle.
#[tokio::test]
async fn scenario_queued_before_submit_must_surface_contested() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("qa-queued-agent".into());

    // Ingest incumbent "alice".
    let resp_alice = engine.ingest_claim(ingest_req(&agent, "alice")).await
        .expect("ingest alice must succeed");
    assert_eq!(resp_alice.disposition, mempill_types::Disposition::CommittedCheap,
        "alice must be CommittedCheap (no conflict on first ingest)");

    // Ingest challenger "bob" — conflicts → QueuedForAdjudication.
    let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
        .expect("ingest bob must succeed");
    assert_eq!(resp_bob.disposition, mempill_types::Disposition::QueuedForAdjudication,
        "bob must be QueuedForAdjudication (oracle present + conflict)");

    // Query BEFORE any oracle verdict.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("query must succeed");

    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[QUEUED] status={:?} primary={:?} alternatives={:?}",
        status, primary_val, alt_vals
    );

    // MUST be Contested (I7, contested-surfacing principle).
    assert_eq!(
        *status, BeliefStatus::Contested,
        "QUEUED: query_memory BEFORE submit MUST return Contested. \
         Got {:?}. If this is TimingUncertain or Resolved with only 'bob', \
         the heavy-path supersession at ingest incorrectly bounded alice before \
         the oracle could respond — violating the contested-surfacing principle.",
        status
    );

    // Must NOT silently surface only "bob" (challenger) as the winner.
    let only_bob = primary_val == Some(serde_json::json!("bob")) && alt_vals.is_empty();
    assert!(!only_bob,
        "QUEUED: MUST NOT surface only 'bob' as the winner before oracle responds. \
         Both alice and bob must be visible as Contested.");

    // Must NOT return NoBelief (both claims exist in the store).
    assert_ne!(*status, BeliefStatus::NoBelief,
        "QUEUED: MUST NOT return NoBelief — two claims exist and neither has been resolved.");
}

// ── Scenario 2: AFFIRM — challenger "bob" wins ───────────────────────────────

/// SCENARIO 2 (AFFIRM).
///
/// After submit Affirm, the oracle confirms "bob" (challenger) wins.
/// EXPECTED: query_memory surfaces "bob" as the single current belief
/// (Resolved or TimingUncertain); NOT Contested, NOT "alice", NOT NoBelief.
#[tokio::test]
async fn scenario_affirm_surfaces_challenger_bob() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("qa-affirm-agent".into());

    engine.ingest_claim(ingest_req(&agent, "alice")).await.expect("ingest alice");
    engine.ingest_claim(ingest_req(&agent, "bob")).await.expect("ingest bob");

    let outcome = engine.submit_adjudication(
        handle_id,
        AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await.expect("Affirm submit must succeed");
    assert_eq!(outcome.disposition, mempill_types::Disposition::CommittedCheap,
        "Affirm outcome disposition must be CommittedCheap");

    let qr = engine.query_memory(query_req(&agent)).await
        .expect("query must succeed");

    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[AFFIRM] status={:?} primary={:?} alternatives={:?}",
        status, primary_val, alt_vals
    );

    // After Affirm: must NOT be Contested.
    assert_ne!(*status, BeliefStatus::Contested,
        "AFFIRM: after Affirm verdict, must NOT be Contested. Got {:?}.", status);
    assert_ne!(*status, BeliefStatus::NoBelief,
        "AFFIRM: after Affirm verdict, must NOT be NoBelief. Got {:?}.", status);

    // The surfaced primary must be "bob" (challenger wins).
    let surfaced_bob = primary_val == Some(serde_json::json!("bob"))
        || alt_vals.contains(&serde_json::json!("bob"));
    assert!(surfaced_bob,
        "AFFIRM: query_memory MUST surface 'bob' (challenger) as the belief after Affirm. \
         Got status={:?}, primary={:?}, alternatives={:?}",
        status, primary_val, alt_vals);

    // "alice" must NOT be visible (incumbent was superseded).
    let surfaced_alice = primary_val == Some(serde_json::json!("alice"))
        || alt_vals.contains(&serde_json::json!("alice"));
    assert!(!surfaced_alice,
        "AFFIRM: 'alice' (incumbent) must NOT be surfaced after Affirm. \
         Got status={:?}, primary={:?}, alternatives={:?}",
        status, primary_val, alt_vals);
}

// ── Scenario 3: DENY — incumbent "alice" stands (the suspected bug) ──────────

/// SCENARIO 3 (DENY) — the engineer's suspected bug.
///
/// After submit Deny, the oracle confirms the incumbent "alice" stands.
/// EXPECTED: query_memory surfaces "alice" as the single current belief.
///
/// THE SUSPECTED BUG: During ingest, the heavy-path runs supersession::execute which:
///   (a) writes a ValidityAssertion::Bound on alice
///   (b) writes a Superseded ledger entry for alice
///
/// After Deny, `submit_adjudication` bounds "bob" (challenger → Superseded) and leaves
/// "alice" alone (no new ledger entry, no reinstatement). But alice already has a Bound
/// assertion from ingest-time supersession. The fold checks is_claim_live (returns false
/// for bounded claims) AND is_non_live_disposition (Superseded → excluded). With both
/// filtering alice out and "bob" also Superseded (thus excluded), the fold returns
/// live_claims=[] → NoBelief.
///
/// CORRECT BEHAVIOR: Either:
///   (a) Deny should write a Reopen assertion for alice to reinstate it, OR
///   (b) The heavy-path should NOT run supersession during ingest when oracle IS present
///       (the incumbent should stay CommittedCheap until the oracle resolves).
#[tokio::test]
async fn scenario_deny_surfaces_incumbent_alice() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("qa-deny-agent".into());

    let resp_alice = engine.ingest_claim(ingest_req(&agent, "alice")).await
        .expect("ingest alice");
    assert_eq!(resp_alice.disposition, mempill_types::Disposition::CommittedCheap);

    let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
        .expect("ingest bob");
    assert_eq!(resp_bob.disposition, mempill_types::Disposition::QueuedForAdjudication);

    let outcome = engine.submit_adjudication(
        handle_id,
        AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Deny,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await.expect("Deny submit must succeed");
    assert_eq!(outcome.disposition, mempill_types::Disposition::Superseded,
        "Deny outcome: challenger must be Superseded");
    assert_eq!(outcome.claim_ref, resp_bob.claim_ref,
        "Deny outcome claim_ref must be challenger bob");

    let qr = engine.query_memory(query_req(&agent)).await
        .expect("query must succeed");

    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[DENY] status={:?} primary={:?} alternatives={:?}",
        status, primary_val, alt_vals
    );
    println!(
        "[DENY] alice_claim_ref={} bob_claim_ref={}",
        resp_alice.claim_ref.0, resp_bob.claim_ref.0
    );

    // After Deny: incumbent "alice" MUST be surfaced as the single belief.
    let surfaced_alice = primary_val == Some(serde_json::json!("alice"))
        || alt_vals.contains(&serde_json::json!("alice"));

    assert!(surfaced_alice,
        "DENY BUG: query_memory MUST surface 'alice' (incumbent stands after Deny). \
         Got status={:?}, primary={:?}, alternatives={:?}. \
         ROOT CAUSE: heavy-path supersession at ingest already bounded alice with \
         ValidityAssertion::Bound + Superseded ledger entry. After Deny, no reinstatement \
         (Reopen) is written for alice. The fold excludes bounded+Superseded claims. \
         Result: NoBelief instead of alice. Fix: either (a) write Reopen for alice in Deny \
         path, or (b) do NOT run supersession during ingest when oracle is present \
         (keep incumbent CommittedCheap until oracle resolves).",
        status, primary_val, alt_vals);

    // Must NOT be Contested (oracle resolved with Deny = clear winner).
    assert_ne!(*status, BeliefStatus::Contested,
        "DENY: after Deny verdict, must NOT remain Contested. Got {:?}.", status);

    // Must NOT be NoBelief.
    assert_ne!(*status, BeliefStatus::NoBelief,
        "DENY: after Deny verdict, MUST NOT return NoBelief. \
         'alice' (incumbent) was confirmed by oracle Deny but is not surfaced. \
         Got status={:?}, primary={:?}, alternatives={:?}",
        status, primary_val, alt_vals);

    // "bob" must NOT be surfaced (challenger was superseded by Deny).
    let surfaced_bob = primary_val == Some(serde_json::json!("bob"))
        || alt_vals.contains(&serde_json::json!("bob"));
    assert!(!surfaced_bob,
        "DENY: 'bob' (challenger) MUST NOT be surfaced after Deny. Got {:?}", status);
}

// ── Scenario 4: UNKNOWN — oracle abstains, must stay Contested ───────────────

/// SCENARIO 4 (UNKNOWN).
///
/// After submit Unknown, the oracle abstains. Neither claim wins.
/// EXPECTED: query_memory surfaces Contested (both "alice" + "bob" visible).
///
/// NOTE: This scenario also has the same ingest-time supersession issue as Deny.
/// The incumbent "alice" was bounded at ingest. After Unknown, submit writes Contested
/// ledger entries for BOTH claims. The fold must now include alice again. However,
/// alice still has a Bound assertion from ingest. The Contested ledger entry does not
/// remove the Bound assertion (append-only). So alice is still bounded → excluded.
/// Only bob (Contested disposition, not in is_non_live_disposition) appears as live.
/// has_conflict = false (only 1 live) → NOT Contested → violation.
#[tokio::test]
async fn scenario_unknown_surfaces_contested_both_visible() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("qa-unknown-agent".into());

    engine.ingest_claim(ingest_req(&agent, "alice")).await.expect("ingest alice");
    engine.ingest_claim(ingest_req(&agent, "bob")).await.expect("ingest bob");

    let outcome = engine.submit_adjudication(
        handle_id,
        AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Unknown,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await.expect("Unknown submit must succeed");
    assert_eq!(outcome.disposition, mempill_types::Disposition::Contested,
        "Unknown outcome disposition must be Contested");

    let qr = engine.query_memory(query_req(&agent)).await
        .expect("query must succeed");

    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[UNKNOWN] status={:?} primary={:?} alternatives={:?}",
        status, primary_val, alt_vals
    );

    // After Unknown: must be Contested.
    assert_eq!(
        *status, BeliefStatus::Contested,
        "UNKNOWN: after Unknown verdict (oracle abstains), query_memory MUST return Contested. \
         Got {:?}. If this is TimingUncertain/'bob' only, the ingest-time Bound assertion \
         on alice is excluding the incumbent from live_claims. The Unknown verdict writes \
         Contested to both, but the Bound assertion is never removed.",
        status
    );

    // Both values must be surfaced.
    let surfaced_alice = primary_val == Some(serde_json::json!("alice"))
        || alt_vals.contains(&serde_json::json!("alice"));
    let surfaced_bob = primary_val == Some(serde_json::json!("bob"))
        || alt_vals.contains(&serde_json::json!("bob"));

    assert!(surfaced_alice,
        "UNKNOWN: 'alice' (incumbent) MUST be visible in Contested projection. \
         Got primary={:?}, alternatives={:?}", primary_val, alt_vals);
    assert!(surfaced_bob,
        "UNKNOWN: 'bob' (challenger) MUST be visible in Contested projection. \
         Got primary={:?}, alternatives={:?}", primary_val, alt_vals);
}

// ── Scenario 5: B11-ABSENT — oracle absent regression ────────────────────────

/// SCENARIO 5 (B11-ABSENT regression).
///
/// Oracle absent: when no oracle is configured and a conflicting External claim arrives,
/// the gate fires B11(a) → Contested immediately (not QueuedForAdjudication).
/// `query_memory` MUST return Contested.
///
/// This scenario uses DefaultEngine (no oracle) to confirm the oracle-absent path
/// is not broken by any of the oracle-resolution changes.
#[tokio::test]
async fn scenario_b11_absent_oracle_surfaces_contested() {
    // Use DefaultEngine (no oracle, no pending store) — same as acid_b11_contested.rs.
    let engine = mempill_sqlite::open_default_in_memory()
        .expect("in-memory DefaultEngine must open");

    let agent = AgentId("qa-b11-absent-agent".into());

    let resp_alice = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!("alice"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }).await.expect("ingest alice");
    assert_eq!(resp_alice.disposition, mempill_types::Disposition::CommittedCheap);

    let resp_bob = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        value: serde_json::json!("bob"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.90, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }).await.expect("ingest bob");

    // B11(a): oracle absent → Contested immediately (never QueuedForAdjudication).
    assert_eq!(resp_bob.disposition, mempill_types::Disposition::Contested,
        "B11-ABSENT: oracle absent + External contradiction MUST be Contested immediately. \
         Got {:?}.", resp_bob.disposition);

    let qr = engine.query_memory(QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None,
    }).await.expect("query must succeed");

    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[B11-ABSENT] status={:?} primary={:?} alternatives={:?}",
        status, primary_val, alt_vals
    );

    assert_eq!(
        *status, BeliefStatus::Contested,
        "B11-ABSENT: query_memory after oracle-absent contradiction MUST return Contested. \
         Got {:?}.", status
    );

    let surfaced_alice = primary_val == Some(serde_json::json!("alice"))
        || alt_vals.contains(&serde_json::json!("alice"));
    let surfaced_bob = primary_val == Some(serde_json::json!("bob"))
        || alt_vals.contains(&serde_json::json!("bob"));

    assert!(surfaced_alice,
        "B11-ABSENT: 'alice' MUST be visible in Contested projection. \
         Got primary={:?}, alternatives={:?}", primary_val, alt_vals);
    assert!(surfaced_bob,
        "B11-ABSENT: 'bob' MUST be visible in Contested projection. \
         Got primary={:?}, alternatives={:?}", primary_val, alt_vals);
}
