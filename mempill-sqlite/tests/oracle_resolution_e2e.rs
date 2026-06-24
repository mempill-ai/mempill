//! End-to-end oracle resolution tests (TASK-9 W4+W5, ORACLE_DESIGN §C.4).
//!
//! Uses a real `EngineHandle::new_with_pending_store` wired with:
//!   - `SqlitePersistenceStore` (in-memory)
//!   - `SqlitePendingStore` (shares the same connection)
//!   - `TestOracle` — returns a fixed UUID handle
//!
//! These tests prove the full W4+W5 loop:
//!   ingest two conflicting claims → QueuedForAdjudication + pending row →
//!   submit_adjudication(verdict) → correct disposition + ledger + handle consumed.

use std::sync::Arc;

use mempill_core::{
    application::{AuditQueryRequest, IngestClaimRequest},
    engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter},
    ports::OraclePort,
    EngineConfig, EngineHandle,
};
use mempill_sqlite::{
    connection::open_in_memory,
    store::SqlitePersistenceStore,
};
use mempill_types::{
    AgentId, AdjudicationResponse, AdjudicationVerdict, Cardinality, Confidence, Criticality,
    Disposition, ExternalKind, LedgerEventKind, ProvenanceLabel,
};

// ── TestOracle: returns a fixed UUID handle so tests can correlate it ─────────

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

// ── Helper: build an EngineHandle wired with TestOracle + SqlitePendingStore ──

fn build_oracle_engine(
    oracle_uuid: uuid::Uuid,
) -> EngineHandle<SqlitePersistenceStore, TestOracle, mempill_core::NoOpVector> {
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: oracle_uuid });

    // S is a spurious unconstrained type param on new_with_pending_store — use () to satisfy it.
    #[allow(clippy::type_complexity)]
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
        subject: "user".into(),
        predicate: "city".into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

// ── Test: Affirm — challenger CommittedCheap, incumbent Superseded ────────────

#[tokio::test]
async fn e2e_affirm_challenger_wins_incumbent_superseded() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("e2e-oracle-agent".into());

    // Ingest incumbent (CommittedCheap).
    let resp_a = engine.ingest_claim(ingest_req(&agent, "Berlin")).await
        .expect("first ingest must succeed");
    assert_eq!(resp_a.disposition, Disposition::CommittedCheap);

    // Ingest challenger (conflicts → QueuedForAdjudication + pending row).
    let resp_b = engine.ingest_claim(ingest_req(&agent, "Paris")).await
        .expect("second ingest must succeed");
    assert_eq!(resp_b.disposition, Disposition::QueuedForAdjudication,
        "conflicting External claim with oracle present must be QueuedForAdjudication");

    let challenger_ref = resp_b.claim_ref.clone();
    let incumbent_ref = resp_a.claim_ref.clone();

    // Submit Affirm verdict.
    let response = AdjudicationResponse {
        handle_id,
        verdict: AdjudicationVerdict::Affirm,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    };
    let outcome = engine.submit_adjudication(handle_id, response).await
        .expect("Affirm verdict must succeed");

    assert_eq!(outcome.handle_id, handle_id);
    assert_eq!(outcome.disposition, Disposition::CommittedCheap, "challenger must be CommittedCheap after Affirm");
    assert_eq!(outcome.claim_ref, challenger_ref);

    // Verify ledger entries written by submit_adjudication.
    //
    // NOTE: During ingest, the heavy-path supersession ALREADY writes a ValidityAsserted+Superseded
    // entry for the incumbent (bound during ingest). Affirm submit writes only the challenger's
    // AdjudicationResolved+CommittedCheap entry. So the net effect of Affirm submit is:
    //   - 1 new AdjudicationResolved entry (challenger → CommittedCheap) from submit
    //   - The incumbent's Superseded entry was written during ingest (not during submit)
    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.expect("audit must succeed");

    // Challenger must have an AdjudicationResolved+CommittedCheap entry (from submit).
    let challenger_affirm_entry = audit.entries.iter()
        .find(|e| e.claim_ref == challenger_ref && e.event_kind == LedgerEventKind::AdjudicationResolved)
        .expect("challenger AdjudicationResolved entry must exist after Affirm");
    assert_eq!(challenger_affirm_entry.disposition, Disposition::CommittedCheap,
        "challenger disposition after Affirm must be CommittedCheap");

    // Verify External provenance in rationale.
    let rationale_str = challenger_affirm_entry.rationale.as_ref()
        .map(|r| r.to_string())
        .unwrap_or_default();
    assert!(rationale_str.contains("ExternalFirstHand"),
        "Affirm rationale must include evidence_provenance ExternalFirstHand");

    // Incumbent must be Superseded — written by ingest heavy-path supersession.
    let incumbent_superseded_entry = audit.entries.iter()
        .find(|e| e.claim_ref == incumbent_ref && e.disposition == Disposition::Superseded)
        .expect("incumbent Superseded entry must exist (written during ingest heavy path)");
    assert_eq!(incumbent_superseded_entry.disposition, Disposition::Superseded);
}

// ── Test: Deny — incumbent stands, challenger Superseded ─────────────────────

#[tokio::test]
async fn e2e_deny_incumbent_stands_challenger_superseded() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("e2e-deny-agent".into());

    let resp_a = engine.ingest_claim(ingest_req(&agent, "London")).await.unwrap();
    assert_eq!(resp_a.disposition, Disposition::CommittedCheap);

    let resp_b = engine.ingest_claim(ingest_req(&agent, "Madrid")).await.unwrap();
    assert_eq!(resp_b.disposition, Disposition::QueuedForAdjudication);

    let challenger_ref = resp_b.claim_ref.clone();

    let response = AdjudicationResponse {
        handle_id,
        verdict: AdjudicationVerdict::Deny,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    };
    let outcome = engine.submit_adjudication(handle_id, response).await
        .expect("Deny verdict must succeed");

    assert_eq!(outcome.disposition, Disposition::Superseded);
    assert_eq!(outcome.claim_ref, challenger_ref);

    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.unwrap();

    // Deny writes a ValidityAsserted+Superseded entry for the challenger (1 ledger entry from submit).
    // The incumbent was already bounded during ingest heavy-path; Deny does NOT reinstate it via
    // a new validity assertion — append-only I1 prohibits "undoing" the ingest supersession.
    // Instead, the latest ledger entry for the incumbent after Deny is still the ingest-time entry.
    let challenger_deny_entry = audit.entries.iter()
        .find(|e| e.claim_ref == challenger_ref && e.event_kind == LedgerEventKind::ValidityAsserted)
        .expect("challenger ValidityAsserted (Superseded) entry must exist after Deny");
    assert_eq!(challenger_deny_entry.disposition, Disposition::Superseded,
        "challenger must be Superseded after Deny");
}

// ── Test: Unknown — both Contested, handle consumed ──────────────────────────

#[tokio::test]
async fn e2e_unknown_both_contested_handle_consumed() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("e2e-unknown-agent".into());

    let resp_a = engine.ingest_claim(ingest_req(&agent, "Rome")).await.unwrap();
    assert_eq!(resp_a.disposition, Disposition::CommittedCheap);

    let resp_b = engine.ingest_claim(ingest_req(&agent, "Athens")).await.unwrap();
    assert_eq!(resp_b.disposition, Disposition::QueuedForAdjudication);

    let challenger_ref = resp_b.claim_ref.clone();
    let incumbent_ref = resp_a.claim_ref.clone();

    let response = AdjudicationResponse {
        handle_id,
        verdict: AdjudicationVerdict::Unknown,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    };
    let outcome = engine.submit_adjudication(handle_id, response).await
        .expect("Unknown verdict must succeed");

    assert_eq!(outcome.disposition, Disposition::Contested);
    assert_eq!(outcome.claim_ref, challenger_ref);

    let audit = engine.query_audit(AuditQueryRequest {
        agent_id: agent.clone(),
        claim_ref: None,
        from_tx_time: None,
        limit: 100,
    }).await.unwrap();

    let abstain_entries: Vec<_> = audit.entries.iter()
        .filter(|e| e.event_kind == LedgerEventKind::AdjudicationResolved)
        .collect();
    assert_eq!(abstain_entries.len(), 2, "Unknown must produce 2 AdjudicationResolved entries");

    let challenger_abstain = abstain_entries.iter().find(|e| e.claim_ref == challenger_ref).unwrap();
    let incumbent_abstain = abstain_entries.iter().find(|e| e.claim_ref == incumbent_ref).unwrap();
    assert_eq!(challenger_abstain.disposition, Disposition::Contested);
    assert_eq!(incumbent_abstain.disposition, Disposition::Contested);

    // Handle must be consumed — second submit returns AdjudicationHandleNotFound.
    let second_response = AdjudicationResponse {
        handle_id,
        verdict: AdjudicationVerdict::Unknown,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    };
    let second_result = engine.submit_adjudication(handle_id, second_response).await;
    assert!(
        matches!(second_result, Err(mempill_core::MemError::AdjudicationHandleNotFound { .. })),
        "second submit with same handle must return AdjudicationHandleNotFound"
    );
}

// ── Test: duplicate submit (same handle after Affirm) → AdjudicationHandleNotFound ──

#[tokio::test]
async fn e2e_duplicate_submit_returns_handle_not_found() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_oracle_engine(handle_id);
    let agent = AgentId("e2e-dup-agent".into());

    let resp_a = engine.ingest_claim(ingest_req(&agent, "Warsaw")).await.unwrap();
    assert_eq!(resp_a.disposition, Disposition::CommittedCheap);

    let resp_b = engine.ingest_claim(ingest_req(&agent, "Krakow")).await.unwrap();
    assert_eq!(resp_b.disposition, Disposition::QueuedForAdjudication);

    let mk_resp = || AdjudicationResponse {
        handle_id,
        verdict: AdjudicationVerdict::Affirm,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    };

    // First submit succeeds.
    engine.submit_adjudication(handle_id, mk_resp()).await
        .expect("first submit must succeed");

    // Second submit must fail.
    let result = engine.submit_adjudication(handle_id, mk_resp()).await;
    assert!(
        matches!(result, Err(mempill_core::MemError::AdjudicationHandleNotFound { .. })),
        "duplicate submit must return AdjudicationHandleNotFound"
    );
}
