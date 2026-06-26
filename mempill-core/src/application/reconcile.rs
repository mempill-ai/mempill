//! ReconcileUseCase — contradiction detection pass over a set of subject-lines.
//!
//! Orchestrates the Reconciler, AdjudicationGate, and optionally Supersession for each claim
//! in the requested subject-lines within a single atomic transaction.

use std::sync::Arc;

use chrono::Utc;
use mempill_types::{ClaimEdge, LedgerEntry, LedgerEventKind, TransactionTime};

use crate::{
    application::ingest_claim::build_latest_disposition_map,
    config::EngineConfig,
    engine::{
        gate,
        gate::Route,
        reconciler::{self, ReconcilerInput},
        supersession::{self, SupersessionRequest},
        truth_engine,
    },
    error::MemError,
    ports::{OraclePort, PersistencePort},
};

use super::dto::{ReconcileRequest, ReconcileResponse};

/// Use-case: run a reconciliation pass over the given subject-lines.
pub struct ReconcileUseCase<P, O>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    oracle: Option<Arc<O>>,
    config: EngineConfig,
}

impl<P, O> ReconcileUseCase<P, O>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, oracle: Option<Arc<O>>, config: EngineConfig) -> Self {
        Self { persistence, oracle, config }
    }

    /// Reconcile all specified subject-lines. Empty `subject_lines` = no-op (not an error).
    pub fn execute(&self, req: ReconcileRequest) -> Result<ReconcileResponse, MemError> {
        if req.subject_lines.is_empty() {
            return Ok(ReconcileResponse { outcomes: vec![], oracle_escalations: 0 });
        }

        let now = Utc::now();
        let tx_time = TransactionTime(now);
        let oracle_present = self.oracle.is_some();
        let mut outcomes = Vec::new();
        let mut oracle_escalations = 0u32;

        // ── Collect ALL reads BEFORE begin_atomic ────────────────────────────────
        // All subject-line claims, validity assertions, ledger, and edges for any
        // supersession candidate must be loaded HERE — outside the transaction window.

        // Load ledger for disposition filtering (excludes non-live dispositions from fold).
        let all_ledger = self.persistence
            .load_ledger(&req.agent_id, None, 10_000)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let latest_disposition = build_latest_disposition_map(&all_ledger);

        // Per subject-line: load claims, fold, compute decisions, pre-load edges.
        struct SubjectLineData {
            fold: truth_engine::FoldResult,
            // For each live claim: (decision, preloaded_edges_for_supersession)
            per_claim: Vec<(
                mempill_types::ClaimRef,
                crate::engine::gate::GateDecision,
                Vec<ClaimEdge>,
            )>,
        }

        let mut subject_line_data: Vec<SubjectLineData> = Vec::new();

        for (subject, predicate) in &req.subject_lines {
            let claims = self.persistence
                .load_subject_line(&req.agent_id, subject, predicate)
                .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

            let fold = truth_engine::fold(
                claims.clone(),
                |cref| {
                    self.persistence
                        .load_validity_assertions_for(&req.agent_id, cref)
                        .unwrap_or_default()
                },
                tx_time.0,
                &self.config,
                &latest_disposition,
            );

            let n_live = fold.live_claims.len();
            let incumbent = fold.live_claims.first().map(|cs| truth_engine::claim_to_belief(cs));

            let mut per_claim = Vec::new();
            for cs in &fold.live_claims {
                let candidate = &cs.claim;
                let proposal = reconciler::reconcile(
                    ReconcilerInput {
                        candidate,
                        incumbent: incumbent.as_ref(),
                        superseded_claim_refs: &[],
                        measured_confidence: candidate.confidence().value_confidence,
                        cardinality_proposal: candidate.cardinality().clone(),
                        oracle_present,
                        succession_threshold: self.config.valid_time_confidence_threshold,
                        n_gt_1_live_incumbents: n_live > 1,
                    },
                    &self.config,
                );
                let decision = gate::adjudicate(&proposal, &self.config);

                // Pre-load edges for supersession if this will be a HeavyPath (reads must precede begin_atomic).
                let preloaded_edges = if matches!(decision.route, Route::HeavyPath) {
                    if let Some(ref inc) = incumbent {
                        if inc.claim_ref != *candidate.claim_ref() {
                            self.persistence
                                .load_edges_for(&req.agent_id, &inc.claim_ref)
                                .map_err(|e| MemError::Persistence { source: Box::new(e) })?
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };

                per_claim.push((candidate.claim_ref().clone(), decision, preloaded_edges));
            }

            subject_line_data.push(SubjectLineData { fold, per_claim });
        }

        // ── Now open the transaction — writes only ─────────────────────────────
        let mut txn = self.persistence
            .begin_atomic(&req.agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let result = (|| {
            for (sld_idx, sld) in subject_line_data.iter().enumerate() {
                let incumbent = sld.fold.live_claims.first()
                    .map(|cs| truth_engine::claim_to_belief(cs));
                let _ = sld_idx; // suppress unused warning

                for (claim_ref, decision, preloaded_edges) in &sld.per_claim {
                    // Append ledger entry for the reconciliation outcome.
                    let entry = LedgerEntry {
                        entry_id: uuid::Uuid::new_v4(),
                        agent_id: req.agent_id.clone(),
                        claim_ref: claim_ref.clone(),
                        event_kind: LedgerEventKind::AdjudicationResolved,
                        disposition: decision.disposition.clone(),
                        rationale: Some(decision.rationale.clone()),
                        recorded_at: tx_time.clone(),
                    };
                    self.persistence
                        .append_ledger_entry(&mut txn, &entry)
                        .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                    // C4 supersession if heavy path — uses preloaded_edges (loaded before begin_atomic).
                    if matches!(decision.route, Route::HeavyPath) {
                        if let Some(ref inc) = incumbent {
                            if inc.claim_ref != *claim_ref {
                                let supr = SupersessionRequest {
                                    agent_id: req.agent_id.clone(),
                                    superseded_ref: inc.claim_ref.clone(),
                                    overturning_ref: claim_ref.clone(),
                                    bound_at: tx_time.0,
                                    recorded_at: tx_time.clone(),
                                };
                                supersession::execute(
                                    &*self.persistence,
                                    &mut txn,
                                    &supr,
                                    preloaded_edges,
                                )
                                .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                            }
                            oracle_escalations += 1;
                        }
                    }

                    outcomes.push((claim_ref.clone(), decision.disposition.clone()));
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.persistence
                    .commit(txn)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                Ok(ReconcileResponse { outcomes, oracle_escalations })
            }
            Err(e) => {
                let _ = self.persistence.rollback(txn);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoOpOracle;
    use crate::ports::persistence::Txn;
    use mempill_types::{
        AgentId, Claim, ClaimEdge, ClaimRef, LedgerEntry, TransactionTime, ValidityAssertion,
    };

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock")]
    struct MockErr;

    #[derive(Default)]
    struct MockStore;

    impl PersistencePort for MockStore {
        type Transaction = MockTxn;
        type Error = MockErr;
        fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(aid.clone())) }
        fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> { Ok(c.claim_ref().clone()) }
        fn append_validity_assertion(&self, _t: &mut MockTxn, _a: &ValidityAssertion) -> Result<(), MockErr> { Ok(()) }
        fn append_ledger_entry(&self, _t: &mut MockTxn, _e: &LedgerEntry) -> Result<(), MockErr> { Ok(()) }
        fn append_claim_edge(&self, _t: &mut MockTxn, _e: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
        fn commit(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn load_subject_line(&self, _a: &AgentId, _s: &str, _p: &str) -> Result<Vec<Claim>, MockErr> { Ok(vec![]) }
        fn load_claim(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }
        fn load_ledger(&self, _a: &AgentId, _f: Option<&TransactionTime>, _l: usize) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_edges_for(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _a: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
    }

    #[test]
    fn empty_subject_lines_returns_empty_outcomes() {
        let store = Arc::new(MockStore);
        let uc = ReconcileUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpOracle>>,
            EngineConfig::default(),
        );
        let resp = uc.execute(ReconcileRequest {
            agent_id: AgentId("a".into()),
            subject_lines: vec![],
        }).unwrap();
        assert!(resp.outcomes.is_empty());
        assert_eq!(resp.oracle_escalations, 0);
    }

    #[test]
    fn reconcile_no_claims_returns_empty_outcomes() {
        let store = Arc::new(MockStore);
        let uc = ReconcileUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpOracle>>,
            EngineConfig::default(),
        );
        let resp = uc.execute(ReconcileRequest {
            agent_id: AgentId("a".into()),
            subject_lines: vec![("user".into(), "city".into())],
        }).unwrap();
        // No claims on the subject-line → no outcomes.
        assert!(resp.outcomes.is_empty());
    }
}
