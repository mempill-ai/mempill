//! End-to-end tests for the W7a public oracle constructors.
//!
//! Tests that `open_with_oracle` and `open_with_oracle_in_memory` produce a fully wired
//! `EngineHandle` that can run the complete oracle resolution loop through the new public
//! API surface (no direct `EngineHandle::new_with_pending_store` calls).
//!
//! These are the FIRST tests to exercise the oracle via the public constructor API.

use std::sync::Arc;

use mempill_core::ports::OraclePort;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_sqlite::{open_with_oracle, open_with_oracle_in_memory};
use mempill_types::{
    AgentId, AdjudicationResponse, AdjudicationVerdict, BeliefStatus, Cardinality,
    Confidence, Criticality, Disposition, ExternalKind, ProvenanceLabel,
};

// ── TestOracle ────────────────────────────────────────────────────────────────

/// Deterministic oracle that returns a caller-supplied UUID as the handle.
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ingest_req(agent: &AgentId, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "company".into(),
        predicate: "hq".into(),
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
        subject: "company".into(),
        predicate: "hq".into(),
        as_of_tx_time: None,
    }
}

// ── Test 1: open_with_oracle_in_memory — conflict + Affirm ───────────────────

/// Construct via `open_with_oracle_in_memory`, ingest two conflicting claims,
/// then submit Affirm. Verifies the complete W4–W5 loop through the new constructor.
///
/// Before submit: query_memory must surface Contested (both claims visible).
/// After Affirm: query_memory must surface the challenger (Resolved/TimingUncertain).
#[tokio::test]
async fn e2e_open_with_oracle_in_memory_affirm_resolution() {
    let handle_id = uuid::Uuid::new_v4();
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

    let engine = open_with_oracle_in_memory(oracle)
        .expect("open_with_oracle_in_memory must succeed");

    let agent = AgentId("w7a-inmem-affirm-agent".into());

    // Ingest incumbent.
    let resp_berlin = engine.ingest_claim(ingest_req(&agent, "Berlin")).await
        .expect("ingest Berlin must succeed");
    assert_eq!(resp_berlin.disposition, Disposition::CommittedCheap,
        "first ingest must be CommittedCheap");

    // Ingest challenger — must be QueuedForAdjudication (oracle present + conflict).
    let resp_paris = engine.ingest_claim(ingest_req(&agent, "Paris")).await
        .expect("ingest Paris must succeed");
    assert_eq!(resp_paris.disposition, Disposition::QueuedForAdjudication,
        "conflicting claim with oracle present must be QueuedForAdjudication");

    let challenger_ref = resp_paris.claim_ref.clone();

    // BEFORE submit: query_memory must surface Contested.
    let qr_before = engine.query_memory(query_req(&agent)).await
        .expect("query before submit must succeed");
    assert_eq!(qr_before.belief.status, BeliefStatus::Contested,
        "BEFORE submit: belief must be Contested; got {:?}", qr_before.belief.status);

    // Submit Affirm.
    let outcome = engine.submit_adjudication(
        handle_id,
        AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await.expect("Affirm submit must succeed");

    assert_eq!(outcome.claim_ref, challenger_ref);
    assert_eq!(outcome.disposition, Disposition::CommittedCheap,
        "challenger must be CommittedCheap after Affirm");

    // AFTER Affirm: query_memory must surface the challenger.
    let qr_after = engine.query_memory(query_req(&agent)).await
        .expect("query after Affirm must succeed");

    assert_ne!(qr_after.belief.status, BeliefStatus::Contested,
        "AFTER Affirm: must NOT be Contested; got {:?}", qr_after.belief.status);
    assert_ne!(qr_after.belief.status, BeliefStatus::NoBelief,
        "AFTER Affirm: must NOT be NoBelief; got {:?}", qr_after.belief.status);

    let primary_val = qr_after.belief.primary.as_ref()
        .map(|b| b.fact.value.clone())
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(primary_val, serde_json::json!("Paris"),
        "AFTER Affirm: challenger 'Paris' must be the surfaced belief; got {primary_val:?}");
}

// ── Test 2: open_with_oracle (file-backed) smoke test ────────────────────────

/// Smoke test for the file-backed `open_with_oracle` constructor.
/// Opens a temp file, ingests one claim, verifies the engine works end-to-end.
#[tokio::test]
async fn e2e_open_with_oracle_file_backed_smoke() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().join("smoke.db");
    let path_str = path.to_str().unwrap();

    let handle_id = uuid::Uuid::new_v4();
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

    let engine = open_with_oracle(path_str, oracle)
        .expect("open_with_oracle (file-backed) must succeed");

    let agent = AgentId("w7a-file-smoke-agent".into());

    // Ingest a single claim — no conflict, should be CommittedCheap.
    let resp = engine.ingest_claim(ingest_req(&agent, "Munich")).await
        .expect("ingest must succeed");
    assert_eq!(resp.disposition, Disposition::CommittedCheap);
    assert!(!resp.claim_ref.0.is_nil(), "claim_ref must be non-nil");

    // Query it back.
    let qr = engine.query_memory(query_req(&agent)).await
        .expect("query must succeed");
    assert!(
        matches!(qr.belief.status, BeliefStatus::Resolved | BeliefStatus::TimingUncertain),
        "single claim must surface as Resolved or TimingUncertain; got {:?}", qr.belief.status
    );
    let val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    assert_eq!(val, Some(serde_json::json!("Munich")));
}

// ── Test 3: DefaultEngine (no-oracle) still compiles and passes ───────────────

/// Regression guard: `open_default_in_memory` must still work unchanged.
#[tokio::test]
async fn e2e_open_default_in_memory_unchanged() {
    let engine = mempill_sqlite::open_default_in_memory()
        .expect("open_default_in_memory must succeed");

    let agent = AgentId("w7a-regression-agent".into());

    let resp = engine.ingest_claim(IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "user".into(),
        predicate: "lang".into(),
        value: serde_json::json!("Rust"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.99, valid_time_confidence: 0.0 },
        criticality: Criticality::Low,
        derived_from: vec![],
    }).await.expect("ingest must succeed");

    assert_eq!(resp.disposition, Disposition::CommittedCheap);
}
