//! Generic oracle-resolution conformance suite (TASK-9-W8).
//!
//! `run_oracle_conformance` exercises every observable oracle-resolution behavior
//! and panics on any deviation from the expected contract.
//!
//! Both `mempill-sqlite` and `mempill-postgres` activate `mempill-core/test-support`
//! in dev-dependencies and call the exported scenario functions to verify that the
//! SAME assertions pass on SQLite (in-memory + file-backed) and PG 16 + PG 18.
//!
//! # Design
//!
//! The harness is built from standalone async scenario functions.  Each function
//! receives either:
//! - A reference to an already-constructed `EngineHandle` (for most scenarios), OR
//! - A pair of factory closures (for the reopen / durability scenario which drops the
//!   first engine and opens a second one over the same durable backing store).
//!
//! Scenarios use a dedicated `AgentId` per-scenario so they are safe to run
//! sequentially against a shared store without cross-contamination.
//!
//! # Scenario catalogue (W8-CONFORMANCE)
//!
//! | Sub-test | Function |
//! |----------|----------|
//! | 1  | `scenario_affirm_challenger_wins` |
//! | 2  | `scenario_deny_incumbent_stands` |
//! | 3  | `scenario_unknown_stays_contested` |
//! | 4  | `scenario_queued_surfaces_contested` |
//! | 5  | `scenario_stale_handle_not_found` |
//! | 6  | `scenario_duplicate_submit_not_found` |
//! | 7  | `scenario_ttl_expiry_reverts_contested` |
//! | 8a | `scenario_sweep_reverts_expired` |
//! | 8b | `scenario_sweep_recovers_orphan` |
//! | 9  | `scenario_durable_store_survives_reopen` |
//! | 10 | `scenario_atomicity_no_torn_write` |
//! | 11 | `scenario_ledger_entry_expectations` |
//! | 12 | `scenario_b11_oracle_absent_contested` |

#[cfg(any(test, feature = "test-support"))]
use std::time::Duration;

#[cfg(any(test, feature = "test-support"))]
use mempill_types::{
    AdjudicationResponse, AdjudicationVerdict, AgentId, BeliefStatus, Cardinality, Confidence,
    Criticality, Disposition, ExternalKind, LedgerEventKind, ProvenanceLabel,
};

#[cfg(any(test, feature = "test-support"))]
use crate::{
    application::{AuditQueryRequest, IngestClaimRequest, QueryMemoryRequest},
    ports::OraclePort,
    EngineConfig, EngineHandle,
};

// ── Internal TestOracle ───────────────────────────────────────────────────────

/// Deterministic oracle that always returns the caller-supplied `fixed_uuid` as the
/// adjudication handle.  `handle_to_uuid` is the identity function on `uuid::Uuid`.
///
/// Both adapter test files use this type (via the re-exported `build_oracle_engine` helper)
/// so that both adapters exercise **identical oracle behavior**.
#[cfg(any(test, feature = "test-support"))]
pub struct TestOracle {
    pub fixed_uuid: uuid::Uuid,
}

#[cfg(any(test, feature = "test-support"))]
impl OraclePort for TestOracle {
    type Error = crate::noop::NoOpError;
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

// ── Common request builders ───────────────────────────────────────────────────

#[cfg(any(test, feature = "test-support"))]
pub fn ingest_req(agent: &AgentId, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "subject".into(),
        predicate: "predicate".into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn query_req(agent: &AgentId) -> QueryMemoryRequest {
    QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "subject".into(),
        predicate: "predicate".into(),
        as_of_tx_time: None,
    }
}

#[cfg(any(test, feature = "test-support"))]
fn adj_response(
    handle_id: uuid::Uuid,
    verdict: AdjudicationVerdict,
) -> AdjudicationResponse {
    AdjudicationResponse {
        handle_id,
        verdict,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    }
}

// ── Scenario 1: Affirm — challenger wins ─────────────────────────────────────

/// W8-sub-1: Affirm → challenger CommittedCheap, incumbent Superseded,
/// ledger entry has External provenance, query_memory surfaces challenger.
///
/// Callers pass `handle_id` matching the UUID used when building the engine's `TestOracle`.
#[cfg(any(test, feature = "test-support"))]
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_affirm_challenger_wins_with_handle<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-affirm-agent".into());

    let resp_inc = engine.ingest_claim(ingest_req(&agent, "incumbent-value")).await
        .expect("conformance[affirm]: ingest incumbent must succeed");
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap,
        "conformance[affirm]: incumbent must be CommittedCheap");

    let resp_ch = engine.ingest_claim(ingest_req(&agent, "challenger-value")).await
        .expect("conformance[affirm]: ingest challenger must succeed");
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication,
        "conformance[affirm]: challenger with oracle present must be QueuedForAdjudication");

    let challenger_ref = resp_ch.claim_ref.clone();
    let incumbent_ref = resp_inc.claim_ref.clone();

    // Submit Affirm.
    let outcome = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Affirm),
    ).await.expect("conformance[affirm]: Affirm submit must succeed");

    assert_eq!(outcome.disposition, Disposition::CommittedCheap,
        "conformance[affirm]: challenger must be CommittedCheap after Affirm");
    assert_eq!(outcome.claim_ref, challenger_ref,
        "conformance[affirm]: outcome.claim_ref must be challenger");

    // query_memory must surface challenger.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[affirm]: query must succeed");
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    assert_ne!(qr.belief.status, BeliefStatus::Contested,
        "conformance[affirm]: must NOT be Contested after Affirm");
    assert_ne!(qr.belief.status, BeliefStatus::NoBelief,
        "conformance[affirm]: must NOT be NoBelief after Affirm");
    assert_eq!(primary_val, Some(serde_json::json!("challenger-value")),
        "conformance[affirm]: challenger must be surfaced as primary belief");

    // Ledger must have AdjudicationResolved + External provenance for challenger.
    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.expect("conformance[affirm]: audit must succeed");

    let ch_entry = audit.entries.iter()
        .find(|e| e.claim_ref == challenger_ref && e.event_kind == LedgerEventKind::AdjudicationResolved)
        .expect("conformance[affirm]: AdjudicationResolved entry for challenger must exist");
    assert_eq!(ch_entry.disposition, Disposition::CommittedCheap,
        "conformance[affirm]: ledger entry disposition must be CommittedCheap");
    let rationale = ch_entry.rationale.as_ref().map(|r| r.to_string()).unwrap_or_default();
    assert!(rationale.contains("ExternalFirstHand"),
        "conformance[affirm]: Affirm rationale must contain ExternalFirstHand provenance");

    // Incumbent must have a Superseded entry (written during ingest heavy-path).
    let inc_entry = audit.entries.iter()
        .find(|e| e.claim_ref == incumbent_ref && e.disposition == Disposition::Superseded)
        .expect("conformance[affirm]: incumbent Superseded entry must exist");
    assert_eq!(inc_entry.disposition, Disposition::Superseded,
        "conformance[affirm]: incumbent must be Superseded");
}

// ── Scenario 2: Deny — incumbent stands ──────────────────────────────────────

#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_deny_incumbent_stands<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-deny-agent".into());

    let resp_inc = engine.ingest_claim(ingest_req(&agent, "incumbent-deny")).await
        .expect("conformance[deny]: ingest incumbent");
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap);

    let resp_ch = engine.ingest_claim(ingest_req(&agent, "challenger-deny")).await
        .expect("conformance[deny]: ingest challenger");
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication);

    let challenger_ref = resp_ch.claim_ref.clone();

    let outcome = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Deny),
    ).await.expect("conformance[deny]: Deny submit must succeed");

    assert_eq!(outcome.disposition, Disposition::Superseded,
        "conformance[deny]: challenger must be Superseded after Deny");
    assert_eq!(outcome.claim_ref, challenger_ref,
        "conformance[deny]: outcome.claim_ref must be challenger");

    // query_memory must surface incumbent.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[deny]: query must succeed");
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    assert_ne!(qr.belief.status, BeliefStatus::Contested,
        "conformance[deny]: must NOT be Contested after Deny");
    assert_ne!(qr.belief.status, BeliefStatus::NoBelief,
        "conformance[deny]: must NOT be NoBelief after Deny");
    assert_eq!(primary_val, Some(serde_json::json!("incumbent-deny")),
        "conformance[deny]: incumbent must be surfaced after Deny");
}

// ── Scenario 3: Unknown — stays Contested ────────────────────────────────────

#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_unknown_stays_contested<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-unknown-agent".into());

    let resp_inc = engine.ingest_claim(ingest_req(&agent, "incumbent-unknown")).await
        .expect("conformance[unknown]: ingest incumbent");
    let incumbent_ref = resp_inc.claim_ref.clone();
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap);

    let resp_ch = engine.ingest_claim(ingest_req(&agent, "challenger-unknown")).await
        .expect("conformance[unknown]: ingest challenger");
    let challenger_ref = resp_ch.claim_ref.clone();
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication);

    let outcome = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Unknown),
    ).await.expect("conformance[unknown]: Unknown submit must succeed");

    assert_eq!(outcome.disposition, Disposition::Contested,
        "conformance[unknown]: outcome must be Contested after Unknown");
    assert_eq!(outcome.claim_ref, challenger_ref);

    // query_memory must surface Contested[both].
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[unknown]: query must succeed");
    assert_eq!(qr.belief.status, BeliefStatus::Contested,
        "conformance[unknown]: must be Contested after Unknown");
    let all_vals: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(all_vals.contains(&serde_json::json!("incumbent-unknown")),
        "conformance[unknown]: incumbent must be visible in Contested");
    assert!(all_vals.contains(&serde_json::json!("challenger-unknown")),
        "conformance[unknown]: challenger must be visible in Contested");

    // Handle must be consumed — second submit must fail.
    let second = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Unknown),
    ).await;
    assert!(
        matches!(second, Err(crate::error::MemError::AdjudicationHandleNotFound { .. })),
        "conformance[unknown]: second submit on consumed handle must be AdjudicationHandleNotFound; got {:?}",
        second
    );

    // Audit: 2 AdjudicationResolved entries (one per claim).
    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.expect("conformance[unknown]: audit must succeed");
    let resolved: Vec<_> = audit.entries.iter()
        .filter(|e| e.event_kind == LedgerEventKind::AdjudicationResolved)
        .collect();
    assert_eq!(resolved.len(), 2,
        "conformance[unknown]: Unknown must produce 2 AdjudicationResolved entries");
    let has_inc = resolved.iter().any(|e| e.claim_ref == incumbent_ref && e.disposition == Disposition::Contested);
    let has_ch  = resolved.iter().any(|e| e.claim_ref == challenger_ref && e.disposition == Disposition::Contested);
    assert!(has_inc, "conformance[unknown]: incumbent AdjudicationResolved/Contested must exist");
    assert!(has_ch,  "conformance[unknown]: challenger AdjudicationResolved/Contested must exist");
}

// ── Scenario 4: Queued — BEFORE submit surfaces Contested ─────────────────────

#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_queued_surfaces_contested<P, O, V>(
    engine: &EngineHandle<P, O, V>,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-queued-agent".into());

    let resp_inc = engine.ingest_claim(ingest_req(&agent, "queued-incumbent")).await
        .expect("conformance[queued]: ingest incumbent");
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap);

    let resp_ch = engine.ingest_claim(ingest_req(&agent, "queued-challenger")).await
        .expect("conformance[queued]: ingest challenger");
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication,
        "conformance[queued]: challenger with oracle must be QueuedForAdjudication");

    // BEFORE submit: query_memory must surface Contested.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[queued]: query must succeed");
    assert_eq!(qr.belief.status, BeliefStatus::Contested,
        "conformance[queued]: BEFORE any submit, belief must be Contested (I7)");
    let all_vals: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(all_vals.contains(&serde_json::json!("queued-incumbent")),
        "conformance[queued]: incumbent must be visible in pre-submit Contested");
    assert!(all_vals.contains(&serde_json::json!("queued-challenger")),
        "conformance[queued]: challenger must be visible in pre-submit Contested");
}

// ── Scenario 5: Stale handle → AdjudicationHandleNotFound ────────────────────

#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_stale_handle_not_found<P, O, V>(
    engine: &EngineHandle<P, O, V>,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let random_handle = uuid::Uuid::new_v4();
    let result = engine.submit_adjudication(
        random_handle,
        adj_response(random_handle, AdjudicationVerdict::Affirm),
    ).await;
    assert!(
        matches!(result, Err(crate::error::MemError::AdjudicationHandleNotFound { .. })),
        "conformance[stale-handle]: random/unknown handle must return AdjudicationHandleNotFound; got {:?}",
        result
    );
}

// ── Scenario 6: Duplicate submit → AdjudicationHandleNotFound ─────────────────

#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_duplicate_submit_not_found<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-dup-agent".into());

    engine.ingest_claim(ingest_req(&agent, "dup-incumbent")).await
        .expect("conformance[dup]: ingest incumbent");
    engine.ingest_claim(ingest_req(&agent, "dup-challenger")).await
        .expect("conformance[dup]: ingest challenger");

    // First submit succeeds.
    engine.submit_adjudication(handle_id, adj_response(handle_id, AdjudicationVerdict::Affirm)).await
        .expect("conformance[dup]: first submit must succeed");

    // Second submit must fail.
    let second = engine.submit_adjudication(handle_id, adj_response(handle_id, AdjudicationVerdict::Affirm)).await;
    assert!(
        matches!(second, Err(crate::error::MemError::AdjudicationHandleNotFound { .. })),
        "conformance[dup]: duplicate submit must return AdjudicationHandleNotFound; got {:?}",
        second
    );
}

// ── Scenario 7: TTL expiry → AdjudicationHandleNotFound + Contested ──────────

/// TTL expiry via a 1-ns TTL so the row expires immediately.
/// Caller must supply an engine built with `EngineConfig { default_adjudication_ttl: Some(1ns), .. }`.
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_ttl_expiry_reverts_contested<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-ttl-agent".into());

    let resp_inc = engine.ingest_claim(ingest_req(&agent, "ttl-incumbent")).await
        .expect("conformance[ttl]: ingest incumbent");
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap);

    let resp_ch = engine.ingest_claim(ingest_req(&agent, "ttl-challenger")).await
        .expect("conformance[ttl]: ingest challenger");
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication);

    // Sleep a tiny bit to ensure the 1-ns TTL has elapsed.
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    // submit on the expired handle → AdjudicationHandleNotFound.
    let result = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Affirm),
    ).await;
    assert!(
        matches!(result, Err(crate::error::MemError::AdjudicationHandleNotFound { .. })),
        "conformance[ttl]: expired handle must return AdjudicationHandleNotFound; got {:?}",
        result
    );

    // query_memory must surface Contested[both] after lazy expiry.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[ttl]: query must succeed");
    assert_eq!(qr.belief.status, BeliefStatus::Contested,
        "conformance[ttl]: after TTL expiry, must be Contested");
    let all_vals: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(all_vals.contains(&serde_json::json!("ttl-incumbent")),
        "conformance[ttl]: incumbent must be visible in Contested after expiry");
    assert!(all_vals.contains(&serde_json::json!("ttl-challenger")),
        "conformance[ttl]: challenger must be visible in Contested after expiry");

    // Audit must contain a TTL/expiry-related ledger entry for the challenger.
    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.expect("conformance[ttl]: audit must succeed");
    // The engine writes an AdjudicationExpired or AdjudicationResolved entry on expiry.
    let has_expiry_entry = audit.entries.iter().any(|e| {
        e.claim_ref == resp_ch.claim_ref
            && (e.event_kind == LedgerEventKind::AdjudicationExpired
                || e.disposition == Disposition::Contested)
    });
    assert!(has_expiry_entry,
        "conformance[ttl]: ledger must have an expiry entry for the challenger; entries={:?}",
        audit.entries.iter().map(|e| (&e.claim_ref, &e.event_kind, &e.disposition)).collect::<Vec<_>>()
    );
}

// ── Scenario 8a: Sweep reverts expired ────────────────────────────────────────

/// Sweep test: an already-past TTL row is reverted to Contested by sweep.
/// Caller must supply an engine built with `default_adjudication_ttl: Some(1ns)`.
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_sweep_reverts_expired<P, O, V>(
    engine: &EngineHandle<P, O, V>,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-sweep-exp-agent".into());

    engine.ingest_claim(ingest_req(&agent, "sweep-exp-incumbent")).await
        .expect("conformance[sweep-exp]: ingest incumbent");
    let resp_ch = engine.ingest_claim(ingest_req(&agent, "sweep-exp-challenger")).await
        .expect("conformance[sweep-exp]: ingest challenger");
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication);

    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    let swept = engine.sweep_expired_adjudications().await
        .expect("conformance[sweep-exp]: sweep must succeed");
    assert!(swept >= 1,
        "conformance[sweep-exp]: sweep must revert at least 1 expired row; got {swept}");

    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[sweep-exp]: query must succeed");
    assert_eq!(qr.belief.status, BeliefStatus::Contested,
        "conformance[sweep-exp]: after sweep, must be Contested");
    let all_vals: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(all_vals.contains(&serde_json::json!("sweep-exp-incumbent")),
        "conformance[sweep-exp]: incumbent must be visible after sweep");
    assert!(all_vals.contains(&serde_json::json!("sweep-exp-challenger")),
        "conformance[sweep-exp]: challenger must be visible after sweep");
}

// ── Scenario 8b: Sweep recovers orphan ────────────────────────────────────────

/// Orphan recovery: a QueuedForAdjudication claim with no pending row is reverted by sweep.
///
/// The orphan is seeded directly via the persistence port, bypassing the engine ingest path.
/// The engine passed to this function must have an accessible persistence store.
/// Because `EngineHandle` does not expose the store, callers must seed the orphan externally
/// and then call this function.  The scenario verifies the post-sweep state.
///
/// This function takes two lambdas:
/// - `seed_orphan`: inserts the orphan claim + ledger entry directly, returns
///   `(incumbent_agent_id, challenger_value, incumbent_value)`.
/// - The engine reference (the EngineHandle built on the same store as seed_orphan touches).
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_sweep_recovers_orphan<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    agent_name: &str,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    // After the adapter test has seeded the orphan, we simply call sweep and verify.
    let agent = AgentId(agent_name.into());

    let swept = engine.sweep_expired_adjudications().await
        .expect("conformance[sweep-orphan]: sweep must succeed");
    assert!(swept >= 1,
        "conformance[sweep-orphan]: sweep must recover at least 1 orphaned claim; got {swept}");

    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[sweep-orphan]: query must succeed");
    assert_eq!(qr.belief.status, BeliefStatus::Contested,
        "conformance[sweep-orphan]: after orphan recovery, must be Contested");
    let all_vals: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(all_vals.contains(&serde_json::json!("orphan-incumbent")),
        "conformance[sweep-orphan]: incumbent must be visible; got {:?}", all_vals);
    assert!(all_vals.contains(&serde_json::json!("orphan-challenger")),
        "conformance[sweep-orphan]: orphaned challenger must be visible; got {:?}", all_vals);
}

// ── Scenario 9: Durable store survives reopen ─────────────────────────────────

/// After queuing a conflict on engine-1, drop it and open engine-2 on the SAME backing
/// store, then submit Affirm on the pre-restart handle.  This proves the pending row
/// (Amendment-1) survives engine restart.
///
/// Callers supply two engines built over the same durable backing store and the
/// handle_id used by the oracle.
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_durable_store_survives_reopen<P, O, V>(
    engine1: EngineHandle<P, O, V>,
    build_engine2: impl FnOnce() -> EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-reopen-agent".into());

    // Engine 1: ingest conflict.
    let resp_inc = engine1.ingest_claim(ingest_req(&agent, "reopen-incumbent")).await
        .expect("conformance[reopen]: ingest incumbent on engine-1");
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap);

    let resp_ch = engine1.ingest_claim(ingest_req(&agent, "reopen-challenger")).await
        .expect("conformance[reopen]: ingest challenger on engine-1");
    assert_eq!(resp_ch.disposition, Disposition::QueuedForAdjudication);

    let challenger_ref = resp_ch.claim_ref.clone();

    // Drop engine 1, simulating restart.
    drop(engine1);

    // Engine 2: open on same backing store.
    let engine2 = build_engine2();

    // Submit Affirm on the pre-restart handle — must resolve (proves pending row durability).
    let outcome = engine2.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Affirm),
    ).await.expect("conformance[reopen]: Affirm on pre-restart handle must succeed");
    assert_eq!(outcome.disposition, Disposition::CommittedCheap,
        "conformance[reopen]: challenger must be CommittedCheap after cross-restart Affirm");
    assert_eq!(outcome.claim_ref, challenger_ref,
        "conformance[reopen]: outcome.claim_ref must be challenger");

    // Query on engine 2 must surface challenger.
    let qr = engine2.query_memory(query_req(&agent)).await
        .expect("conformance[reopen]: query on engine-2 must succeed");
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    assert_eq!(primary_val, Some(serde_json::json!("reopen-challenger")),
        "conformance[reopen]: challenger must be surfaced after cross-restart Affirm");
}

// ── Scenario 10: Atomicity — no torn write ────────────────────────────────────

/// After a successful Affirm submit, the ledger + disposition + pending-row-resolved
/// are all consistent (no partial state).
///
/// Full mid-apply failure injection is not feasible without engine-level hooks; we
/// verify the observable post-success consistency guarantee instead.
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_atomicity_no_torn_write<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-atomicity-agent".into());

    engine.ingest_claim(ingest_req(&agent, "atom-incumbent")).await
        .expect("conformance[atomicity]: ingest incumbent");
    let resp_ch = engine.ingest_claim(ingest_req(&agent, "atom-challenger")).await
        .expect("conformance[atomicity]: ingest challenger");
    let challenger_ref = resp_ch.claim_ref.clone();

    let outcome = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Affirm),
    ).await.expect("conformance[atomicity]: Affirm submit must succeed");

    // Disposition check (ledger).
    assert_eq!(outcome.disposition, Disposition::CommittedCheap,
        "conformance[atomicity]: challenger disposition must be CommittedCheap");
    assert_eq!(outcome.claim_ref, challenger_ref);

    // Pending row must be consumed (handle gone).
    let second = engine.submit_adjudication(
        handle_id,
        adj_response(handle_id, AdjudicationVerdict::Affirm),
    ).await;
    assert!(
        matches!(second, Err(crate::error::MemError::AdjudicationHandleNotFound { .. })),
        "conformance[atomicity]: pending row must be consumed (not found on second submit)"
    );

    // query_memory consistent: challenger surfaced, not Contested.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[atomicity]: query must succeed");
    assert_ne!(qr.belief.status, BeliefStatus::Contested,
        "conformance[atomicity]: after Affirm, must NOT be Contested");
    assert_ne!(qr.belief.status, BeliefStatus::NoBelief,
        "conformance[atomicity]: after Affirm, must NOT be NoBelief");

    // Ledger consistent: AdjudicationResolved entry present.
    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.expect("conformance[atomicity]: audit must succeed");
    let resolved = audit.entries.iter()
        .find(|e| e.claim_ref == challenger_ref && e.event_kind == LedgerEventKind::AdjudicationResolved)
        .expect("conformance[atomicity]: AdjudicationResolved ledger entry must exist");
    assert_eq!(resolved.disposition, Disposition::CommittedCheap,
        "conformance[atomicity]: ledger entry must be CommittedCheap");
}

// ── Scenario 11: Ledger entry expectations consistent across adapters ──────────

/// Verify that the ledger entry kinds and dispositions for each verdict
/// are consistent (same invariants) across adapters.
/// This is an aggregated check — sub-assertions from scenarios 1, 2, 3 are reused
/// here as an explicit cross-check.
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_ledger_entry_expectations<P, O, V>(
    engine: &EngineHandle<P, O, V>,
    handle_id: uuid::Uuid,
    verdict: AdjudicationVerdict,
    expected_ch_disposition: Disposition,
    expected_ch_event_kind: LedgerEventKind,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let label = format!("{:?}", verdict);
    let agent = AgentId(format!("conformance-ledger-{label}-agent"));

    engine.ingest_claim(ingest_req(&agent, "ledger-incumbent")).await
        .expect("conformance[ledger]: ingest incumbent");
    let resp_ch = engine.ingest_claim(ingest_req(&agent, "ledger-challenger")).await
        .expect("conformance[ledger]: ingest challenger");
    let challenger_ref = resp_ch.claim_ref.clone();

    engine.submit_adjudication(handle_id, adj_response(handle_id, verdict)).await
        .expect("conformance[ledger]: submit must succeed");

    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.expect("conformance[ledger]: audit must succeed");

    // Find the resolution entry for the challenger.
    let ch_entry = audit.entries.iter()
        .find(|e| e.claim_ref == challenger_ref && e.event_kind == expected_ch_event_kind)
        .unwrap_or_else(|| panic!(
            "conformance[ledger/{label}]: expected {:?} event kind for challenger; entries={:?}",
            expected_ch_event_kind,
            audit.entries.iter().map(|e| (&e.event_kind, &e.disposition)).collect::<Vec<_>>()
        ));
    assert_eq!(ch_entry.disposition, expected_ch_disposition,
        "conformance[ledger/{label}]: challenger disposition must be {:?}", expected_ch_disposition);
}

// ── Scenario 12: B11 oracle-absent → Contested ────────────────────────────────

/// With no oracle, conflicting External claims must immediately surface as Contested.
/// Caller must pass a no-oracle engine (DefaultEngine / `open_default_in_memory` variant).
#[cfg(any(test, feature = "test-support"))]
pub async fn scenario_b11_oracle_absent_contested<P, O, V>(
    engine: &EngineHandle<P, O, V>,
) where
    P: crate::ports::PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
    O: OraclePort + Send + Sync + 'static,
    V: crate::ports::VectorPort + Send + Sync + 'static,
{
    let agent = AgentId("conformance-b11-agent".into());

    let resp_inc = engine.ingest_claim(ingest_req(&agent, "b11-incumbent")).await
        .expect("conformance[b11]: ingest incumbent");
    assert_eq!(resp_inc.disposition, Disposition::CommittedCheap);

    let resp_ch = engine.ingest_claim(ingest_req(&agent, "b11-challenger")).await
        .expect("conformance[b11]: ingest challenger");
    assert_eq!(resp_ch.disposition, Disposition::Contested,
        "conformance[b11]: oracle-absent External conflict MUST be Contested immediately");

    let qr = engine.query_memory(query_req(&agent)).await
        .expect("conformance[b11]: query must succeed");
    assert_eq!(qr.belief.status, BeliefStatus::Contested,
        "conformance[b11]: query_memory after oracle-absent conflict must be Contested");
    let all_vals: Vec<_> = qr.belief.primary.iter()
        .map(|b| b.fact.value.clone())
        .chain(qr.belief.alternatives.iter().map(|b| b.fact.value.clone()))
        .collect();
    assert!(all_vals.contains(&serde_json::json!("b11-incumbent")),
        "conformance[b11]: incumbent must be visible in Contested");
    assert!(all_vals.contains(&serde_json::json!("b11-challenger")),
        "conformance[b11]: challenger must be visible in Contested");
}

// ── Public entry-point helpers ─────────────────────────────────────────────────

/// Build a fresh `EngineConfig` with a 1-nanosecond adjudication TTL.
/// Used by adapter tests for TTL/sweep scenarios.
#[cfg(any(test, feature = "test-support"))]
pub fn tiny_ttl_config() -> EngineConfig {
    EngineConfig {
        default_adjudication_ttl: Some(Duration::from_nanos(1)),
        ..EngineConfig::default()
    }
}
