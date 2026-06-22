//! IngestClaimUseCase — application layer write path (§4a, A28, I9).
//!
//! Orchestrates C1 → C6 → C3 → C7 → (optional C4) with a single atomic Txn.
//! The use-case OWNS the transaction boundary (I9): begin_atomic → appends → commit.
//! On any error: rollback is called and Err(MemError) is returned.
//!
//! "now" is injected by the EngineHandle caller (DETERMINISM convention).
//! The lock is acquired at the EngineHandle boundary before spawn_blocking.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use mempill_types::{
    AgentId, Claim, ClaimEdge, Disposition,
    EdgeKind, ExternalAnchor, Fact, LedgerEntry, LedgerEventKind,
    TransactionTime,
};

use crate::{
    config::EngineConfig,
    engine::{
        firewall::{AmplificationGuard, FirewallVerdict},
        gate,
        gateway::{self, IngestInput},
        reconciler::{self, ReconcilerInput},
        supersession::{self, SupersessionRequest},
        truth_engine,
    },
    error::MemError,
    ports::{OraclePort, PersistencePort},
};

use super::dto::{IngestClaimRequest, IngestClaimResponse};

/// Use-case: ingest a new claim from any binding.
/// Generic over persistence and oracle ports; zero-cost dispatch; testable with mocks.
/// Oracle is optional: when None, oracle-absent → Contested path fires for heavy-path ops (B11).
pub struct IngestClaimUseCase<P, O>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    oracle: Option<Arc<O>>,
    config: EngineConfig,
}

impl<P, O> IngestClaimUseCase<P, O>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, oracle: Option<Arc<O>>, config: EngineConfig) -> Self {
        Self { persistence, oracle, config }
    }

    /// Synchronous execute — called from EngineHandle via spawn_blocking.
    ///
    /// `now` is engine-stamped by the EngineHandle boundary (DETERMINISM).
    pub fn execute_with_time(
        &self,
        req: IngestClaimRequest,
        now: DateTime<Utc>,
    ) -> Result<IngestClaimResponse, MemError> {
        // ── Step 1: C1 gateway — stamp the claim ─────────────────────────────────
        let tx_time = TransactionTime(now);
        let ingest_input = IngestInput {
            agent_id: req.agent_id.clone(),
            fact: Fact {
                subject: req.subject.clone(),
                predicate: req.predicate.clone(),
                value: req.value.clone(),
            },
            cardinality: req.cardinality.clone(),
            provenance: Some(req.provenance.clone()),
            external_anchor: ExternalAnchor {
                nearest_external_anchor: None,
                derivation_depth: 0,
            },
            valid_time: req.valid_time.clone(),
            confidence: req.confidence.clone(),
            criticality: req.criticality.clone(),
            derived_from: req.derived_from.clone(),
            metadata: None,
        };
        let stamped = gateway::stamp(ingest_input, tx_time.clone())
            .map_err(|e| e)?;
        let claim = stamped.claim;

        // ── Step 2: C6 firewall — amplification guard ────────────────────────────
        let guard = AmplificationGuard::new(Arc::new(self.config.clone()));
        let injected = self.persistence
            .load_injected_claims(&req.agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let fw_verdict = guard.check(&claim, &injected, 0);

        // CorroborateByIdentity: return the existing claim, no new row.
        if let FirewallVerdict::CorroborateByIdentity { existing_claim, .. } = fw_verdict {
            return Ok(IngestClaimResponse {
                claim_ref: existing_claim,
                disposition: Disposition::CommittedCheap,
                contested_with: vec![],
            });
        }
        // Quarantine: park, no row.
        if let FirewallVerdict::Quarantine { .. } = fw_verdict {
            return Ok(IngestClaimResponse {
                claim_ref: claim.claim_ref().clone(),
                disposition: Disposition::Quarantined,
                contested_with: vec![],
            });
        }

        // ── Step 3: C3 reconciler — load incumbent + classify conflict ────────────
        let incumbent_claims = self.persistence
            .load_subject_line(&req.agent_id, &req.subject, &req.predicate)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Fold the incumbent claims to get the live canonical belief.
        let as_of = tx_time.0;
        let fold_result = truth_engine::fold(
            incumbent_claims.clone(),
            |cref| {
                self.persistence
                    .load_validity_assertions_for(&req.agent_id, cref)
                    .unwrap_or_default()
            },
            as_of,
            &self.config,
        );
        let incumbent_belief = fold_result.live_claims.first().map(|cs| {
            truth_engine::claim_to_belief(cs)
        });

        let oracle_present = self.oracle.is_some();
        let proposal = reconciler::reconcile(
            ReconcilerInput {
                candidate: &claim,
                incumbent: incumbent_belief.as_ref(),
                superseded_claim_refs: &[],
                measured_confidence: req.confidence.value_confidence,
                cardinality_proposal: req.cardinality.clone(),
                oracle_present,
            },
            &self.config,
        );

        // ── Step 4: C7 gate — adjudicate ─────────────────────────────────────────
        let decision = gate::adjudicate(&proposal, &self.config);

        // ── Step 5: I9 atomic Txn — begin ────────────────────────────────────────
        let mut txn = self.persistence
            .begin_atomic(&req.agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let result = self.append_within_txn(
            &claim,
            &decision,
            &incumbent_belief,
            &req.agent_id,
            tx_time.clone(),
            &mut txn,
        );

        match result {
            Ok(response) => {
                self.persistence
                    .commit(txn)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                Ok(response)
            }
            Err(e) => {
                // Rollback on any error (I9 — no partial writes).
                let _ = self.persistence.rollback(txn);
                Err(e)
            }
        }
    }

    /// Convenience wrapper: stamps "now" from the caller (the EngineHandle stamps Utc::now()
    /// once at the async boundary and passes it in via execute_with_time).
    /// This shim keeps backward compat with spawn_blocking closures that capture self.
    pub fn execute(&self, req: IngestClaimRequest) -> Result<IngestClaimResponse, MemError> {
        // In tests or direct calls without a EngineHandle, stamp now here.
        self.execute_with_time(req, Utc::now())
    }

    /// All persistence writes inside the open Txn.
    fn append_within_txn(
        &self,
        claim: &Claim,
        decision: &gate::GateDecision,
        incumbent_belief: &Option<mempill_types::Belief>,
        agent_id: &AgentId,
        tx_time: TransactionTime,
        txn: &mut P::Transaction,
    ) -> Result<IngestClaimResponse, MemError> {
        use gate::Route;

        // Append the claim.
        let claim_ref = self.persistence
            .append_claim(txn, claim)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Append ledger entry for the committed claim.
        let ledger_entry = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: claim_ref.clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: decision.disposition.clone(),
            rationale: Some(decision.rationale.clone()),
            recorded_at: tx_time.clone(),
        };
        self.persistence
            .append_ledger_entry(txn, &ledger_entry)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Append DerivedFrom edges for lineage tracking.
        for parent_ref in claim.derived_from() {
            let edge = ClaimEdge {
                edge_id: uuid::Uuid::new_v4(),
                agent_id: agent_id.clone(),
                from_claim: claim_ref.clone(),
                to_claim: parent_ref.clone(),
                kind: EdgeKind::DerivedFrom,
                created_at: tx_time.clone(),
            };
            self.persistence
                .append_claim_edge(txn, &edge)
                .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        }

        // C4 supersession: if heavy-path overturn was decided, cascade.
        let mut contested_with = vec![];
        if matches!(decision.route, Route::HeavyPath) {
            if let Some(incumbent) = incumbent_belief {
                let supersession_req = SupersessionRequest {
                    agent_id: agent_id.clone(),
                    superseded_ref: incumbent.claim_ref.clone(),
                    overturning_ref: claim_ref.clone(),
                    bound_at: tx_time.0,
                    recorded_at: tx_time.clone(),
                };
                supersession::execute(&*self.persistence, txn, &supersession_req)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                contested_with.push(incumbent.claim_ref.clone());
            }
        }

        Ok(IngestClaimResponse {
            claim_ref,
            disposition: decision.disposition.clone(),
            contested_with,
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoOpOracle;
    use crate::ports::persistence::Txn;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimEdge, ClaimRef, Confidence, Criticality, ExternalKind,
        LedgerEntry, ProvenanceLabel, TransactionTime, ValidityAssertion,
    };
    use std::sync::Mutex;

    // ── Mock persistence (in-memory) ──────────────────────────────────────────

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock error")]
    struct MockErr;

    #[derive(Default)]
    struct MockStore {
        claims: Mutex<Vec<Claim>>,
        ledger: Mutex<Vec<LedgerEntry>>,
        should_fail_append: Mutex<bool>,
        // Track rollback calls.
        rollback_called: Mutex<bool>,
    }

    impl PersistencePort for MockStore {
        type Transaction = MockTxn;
        type Error = MockErr;

        fn begin_atomic(&self, agent_id: &AgentId) -> Result<MockTxn, MockErr> {
            Ok(MockTxn(agent_id.clone()))
        }

        fn append_claim(&self, _txn: &mut MockTxn, claim: &Claim) -> Result<ClaimRef, MockErr> {
            if *self.should_fail_append.lock().unwrap() {
                return Err(MockErr);
            }
            self.claims.lock().unwrap().push(claim.clone());
            Ok(claim.claim_ref().clone())
        }

        fn append_validity_assertion(
            &self,
            _txn: &mut MockTxn,
            _a: &ValidityAssertion,
        ) -> Result<(), MockErr> { Ok(()) }

        fn append_ledger_entry(
            &self,
            _txn: &mut MockTxn,
            entry: &LedgerEntry,
        ) -> Result<(), MockErr> {
            self.ledger.lock().unwrap().push(entry.clone());
            Ok(())
        }

        fn append_claim_edge(&self, _txn: &mut MockTxn, _e: &ClaimEdge) -> Result<(), MockErr> {
            Ok(())
        }

        fn commit(&self, _txn: MockTxn) -> Result<(), MockErr> { Ok(()) }

        fn rollback(&self, _txn: MockTxn) -> Result<(), MockErr> {
            *self.rollback_called.lock().unwrap() = true;
            Ok(())
        }

        fn load_subject_line(
            &self,
            _agent_id: &AgentId,
            _subject: &str,
            _predicate: &str,
        ) -> Result<Vec<Claim>, MockErr> { Ok(vec![]) }

        fn load_claim(&self, _agent_id: &AgentId, _ref: &ClaimRef) -> Result<Option<Claim>, MockErr> {
            Ok(None)
        }

        fn load_validity_assertions_for(
            &self,
            _agent_id: &AgentId,
            _ref: &ClaimRef,
        ) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }

        fn load_ledger(
            &self,
            _agent_id: &AgentId,
            _from: Option<&TransactionTime>,
            _limit: usize,
        ) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }

        fn load_edges_for(
            &self,
            _agent_id: &AgentId,
            _ref: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }

        fn load_injected_claims(&self, _agent_id: &AgentId) -> Result<Vec<ClaimRef>, MockErr> {
            Ok(vec![])
        }

        fn load_lineage(
            &self,
            _agent_id: &AgentId,
            _ref: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
    }

    fn base_request() -> IngestClaimRequest {
        IngestClaimRequest {
            agent_id: AgentId("test-agent".into()),
            subject: "user".into(),
            predicate: "city".into(),
            value: serde_json::json!("Paris"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        }
    }

    // ── Test: happy path commits and returns a claim_ref ─────────────────────

    #[test]
    fn ingest_external_claim_commits_and_returns_claim_ref() {
        let store = Arc::new(MockStore::default());
        let uc = IngestClaimUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpOracle>>,
            EngineConfig::default(),
        );
        let now = chrono::Utc::now();
        let resp = uc.execute_with_time(base_request(), now).unwrap();
        assert!(!resp.claim_ref.0.is_nil(), "claim_ref must be a valid UUID");
        assert_eq!(resp.disposition, Disposition::CommittedCheap);
        assert_eq!(store.claims.lock().unwrap().len(), 1, "one claim row must be appended");
    }

    // ── Test: I9 rollback on error ────────────────────────────────────────────

    #[test]
    fn i9_rollback_called_when_append_claim_fails() {
        let store = Arc::new(MockStore::default());
        // Force append_claim to fail.
        *store.should_fail_append.lock().unwrap() = true;

        let uc = IngestClaimUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpOracle>>,
            EngineConfig::default(),
        );
        let now = chrono::Utc::now();
        let result = uc.execute_with_time(base_request(), now);
        assert!(result.is_err(), "must propagate the persistence error");
        assert!(
            *store.rollback_called.lock().unwrap(),
            "rollback must be called when append fails (I9)"
        );
        // No claims or ledger entries should have persisted.
        assert_eq!(store.claims.lock().unwrap().len(), 0, "no claims must remain after rollback");
    }

    // ── Test: oracle_present wiring (A24, B11) ────────────────────────────────

    #[test]
    fn oracle_present_true_when_oracle_is_some() {
        let store = Arc::new(MockStore::default());
        let oracle = Some(Arc::new(NoOpOracle));
        let uc = IngestClaimUseCase::new(
            Arc::clone(&store),
            oracle,
            EngineConfig::default(),
        );
        let now = chrono::Utc::now();
        // With NoOpOracle, oracle_present = true; first External claim has NoConflict → CheapPath.
        let resp = uc.execute_with_time(base_request(), now).unwrap();
        assert_eq!(resp.disposition, Disposition::CommittedCheap);
    }

    #[test]
    fn oracle_absent_none_sets_oracle_present_false() {
        let store = Arc::new(MockStore::default());
        let uc = IngestClaimUseCase::<_, NoOpOracle>::new(
            Arc::clone(&store),
            None,
            EngineConfig::default(),
        );
        let now = chrono::Utc::now();
        // oracle is None → oracle_present = false. First External claim still cheap-paths (no conflict).
        let resp = uc.execute_with_time(base_request(), now).unwrap();
        assert_eq!(resp.disposition, Disposition::CommittedCheap);
    }
}
