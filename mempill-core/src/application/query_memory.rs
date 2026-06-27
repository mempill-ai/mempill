#![allow(missing_docs)]
//! QueryMemoryUseCase — application layer read path.
//!
//! Read-only: no Txn opened, no writes. Delegates to TruthEngine (fold)
//! then Projection (project). `now` is injected by the EngineHandle boundary.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::{
    application::ingest_claim::build_latest_disposition_map,
    config::EngineConfig,
    engine::{projection, truth_engine},
    error::MemError,
    ports::{PersistencePort, VectorPort},
};

use super::dto::{QueryMemoryRequest, QueryMemoryResponse};

/// Use-case: query the canonical belief for a (subject, predicate) line.
/// Generic over persistence and vector ports.
/// Vector is optional: None = structural-only mode (v0.1 default).
pub struct QueryMemoryUseCase<P, V>
where
    P: PersistencePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    #[allow(dead_code)]
    vector: Option<Arc<V>>, // v0.1: unused; structural-only query
    config: EngineConfig,
}

impl<P, V> QueryMemoryUseCase<P, V>
where
    P: PersistencePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, vector: Option<Arc<V>>, config: EngineConfig) -> Self {
        Self { persistence, vector, config }
    }

    /// Read path: no Txn (read-only). TruthEngine fold → Projection → DTO.
    ///
    /// `now` is injected by the EngineHandle (DETERMINISM — no clock reads here).
    pub fn execute_with_time(
        &self,
        req: QueryMemoryRequest,
        now: DateTime<Utc>,
    ) -> Result<QueryMemoryResponse, MemError> {
        // Load all claims for the subject-line.
        let claims = self.persistence
            .load_subject_line(&req.agent_id, &req.subject, &req.predicate)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Determine the bi-temporal as-of point: use the request's as_of_tx_time if supplied,
        // otherwise use the injected `now`.
        let as_of = req.as_of_tx_time.unwrap_or(now);

        // Load ledger for the disposition-based liveness filter — scoped to exactly the
        // claims on this subject-line (no agent-wide cap; always complete regardless of
        // total agent ledger size — fixes the silent-wrong-belief-at-scale bug).
        let claim_refs: Vec<_> = claims.iter().map(|c| c.claim_ref().clone()).collect();
        let all_ledger = self.persistence
            .load_ledger_for_claims(&req.agent_id, &claim_refs)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let latest_disposition = build_latest_disposition_map(&all_ledger);

        // C2: canonical valid-time fold (with disposition filter).
        let fold = truth_engine::fold(
            claims.clone(),
            |cref| {
                self.persistence
                    .load_validity_assertions_for(&req.agent_id, cref)
                    .unwrap_or_default()
            },
            as_of,
            &self.config,
            &latest_disposition,
        );

        // Build ledger entries per claim (for A26 PendingReview detection).
        // Reuse the already-loaded all_ledger from above (no second load needed).
        let ledger_entries: Vec<_> = claims.iter().flat_map(|c| {
            all_ledger.iter()
                .filter(|e| &e.claim_ref == c.claim_ref())
                .cloned()
        }).collect();

        // Determine contested state from ledger (Contested disposition in live claims).
        let contested = fold.live_claims.iter().any(|cs| {
            cs.last_disposition
                .as_ref()
                .map(|d| *d == mempill_types::Disposition::Contested)
                .unwrap_or(false)
        });

        // C5: projection.
        let belief = projection::project(&fold, &ledger_entries, now, &self.config, contested);

        Ok(QueryMemoryResponse { belief })
    }

    /// Convenience wrapper that stamps now internally (for direct calls outside EngineHandle).
    pub fn execute(&self, req: QueryMemoryRequest) -> Result<QueryMemoryResponse, MemError> {
        self.execute_with_time(req, Utc::now())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoOpVector;
    use crate::ports::persistence::Txn;
    use chrono::TimeZone;
    use mempill_types::{
        AgentId, BeliefStatus, Cardinality, Claim, ClaimEdge, ClaimRef, Confidence, Criticality,
        ExternalAnchor, ExternalKind, Fact, LedgerEntry, ProvenanceLabel, TransactionTime,
        ValidTime, ValidityAssertion,
    };
    use std::sync::Mutex;

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock")]
    struct MockErr;

    #[derive(Default)]
    struct MockStore {
        claims: Mutex<Vec<Claim>>,
    }

    impl PersistencePort for MockStore {
        type Transaction = MockTxn;
        type Error = MockErr;
        fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(aid.clone())) }
        fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> {
            self.claims.lock().unwrap().push(c.clone());
            Ok(c.claim_ref().clone())
        }
        fn append_validity_assertion(&self, _t: &mut MockTxn, _a: &ValidityAssertion) -> Result<(), MockErr> { Ok(()) }
        fn append_ledger_entry(&self, _t: &mut MockTxn, _e: &LedgerEntry) -> Result<(), MockErr> { Ok(()) }
        fn append_claim_edge(&self, _t: &mut MockTxn, _e: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
        fn commit(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn load_subject_line(&self, _aid: &AgentId, subject: &str, predicate: &str) -> Result<Vec<Claim>, MockErr> {
            let claims = self.claims.lock().unwrap();
            Ok(claims.iter()
                .filter(|c| c.fact().subject == subject && c.fact().predicate == predicate)
                .cloned()
                .collect())
        }
        fn load_claim(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }
        fn load_ledger(&self, _aid: &AgentId, _from: Option<&mempill_types::TransactionTime>, _lim: usize) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_ledger_for_claims(&self, _aid: &AgentId, _refs: &[ClaimRef]) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_edges_for(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _aid: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
    }

    fn make_claim(subject: &str, predicate: &str, value: serde_json::Value, tx: DateTime<Utc>) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent".into()),
            Fact { subject: subject.into(), predicate: predicate.into(), value },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    #[test]
    fn query_no_claims_returns_no_belief() {
        let store = Arc::new(MockStore::default());
        let uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            EngineConfig::default(),
        );
        let now = Utc::now();
        let req = QueryMemoryRequest {
            agent_id: AgentId("agent".into()),
            subject: "user".into(),
            predicate: "city".into(),
            as_of_tx_time: None,
        };
        let resp = uc.execute_with_time(req, now).unwrap();
        assert_eq!(resp.belief.status, BeliefStatus::NoBelief);
    }

    #[test]
    fn query_with_one_claim_returns_resolved() {
        let store = Arc::new(MockStore::default());
        let tx = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let claim = make_claim("user", "city", serde_json::json!("Paris"), tx);
        store.claims.lock().unwrap().push(claim);

        let uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            EngineConfig::default(),
        );
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let req = QueryMemoryRequest {
            agent_id: AgentId("agent".into()),
            subject: "user".into(),
            predicate: "city".into(),
            as_of_tx_time: None,
        };
        let resp = uc.execute_with_time(req, now).unwrap();
        // Single live claim with unknown valid_time → TimingUncertain (valid_time is None).
        assert!(
            matches!(resp.belief.status, BeliefStatus::TimingUncertain | BeliefStatus::Resolved),
            "expected Resolved or TimingUncertain, got {:?}",
            resp.belief.status
        );
        assert!(resp.belief.primary.is_some(), "primary belief must be present");
    }

    #[test]
    fn query_now_injected_not_read_from_clock() {
        // Verify that the injected `now` flows into projection (currency decay) rather than
        // the system clock. Two queries with different injected 'now' values on the same
        // claim yield different CurrencyState in the result.
        let store = Arc::new(MockStore::default());
        // A claim from 200 days ago.
        let old_tx = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let claim = make_claim("user", "job", serde_json::json!("Engineer"), old_tx);
        store.claims.lock().unwrap().push(claim);

        let uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            EngineConfig::default(),
        );

        // Query with 'now' very close to the claim's tx_time → should be Fresh.
        let near_now = Utc.with_ymd_and_hms(2020, 1, 2, 0, 0, 0).unwrap(); // 1 day later
        let req = QueryMemoryRequest {
            agent_id: AgentId("agent".into()),
            subject: "user".into(),
            predicate: "job".into(),
            as_of_tx_time: None,
        };
        let resp_near = uc.execute_with_time(req.clone(), near_now).unwrap();
        assert!(resp_near.belief.primary.is_some());
        assert_eq!(
            resp_near.belief.primary.unwrap().currency_signal.state,
            mempill_types::CurrencyState::Fresh,
            "1 day after claim, currency must be Fresh (injected now, not system clock)"
        );

        // Query with 'now' 200 days after claim → should be Decayed (decayed_threshold_days=90).
        let far_now = Utc.with_ymd_and_hms(2020, 7, 20, 0, 0, 0).unwrap(); // ~200 days later
        let resp_far = uc.execute_with_time(req, far_now).unwrap();
        assert!(resp_far.belief.primary.is_some());
        assert_eq!(
            resp_far.belief.primary.unwrap().currency_signal.state,
            mempill_types::CurrencyState::Decayed,
            "200 days after claim, currency must be Decayed (injected now, not system clock)"
        );
    }
}
