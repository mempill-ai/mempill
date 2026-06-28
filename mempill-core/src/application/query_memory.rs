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
        //
        // D2 independence: `as_of` drives transaction-time visibility (which claims/assertions
        // are visible). `req.valid_at` is the independent valid-time axis — it narrows the
        // live set to the single claim whose valid-time window contains the instant, AFTER
        // the tx-time filter. When `req.valid_at` is `None`, backward-compatible behaviour
        // is preserved: the fold uses `as_of` for both axes.
        let fold = truth_engine::fold(
            claims.clone(),
            |cref| {
                self.persistence
                    .load_validity_assertions_for(&req.agent_id, cref)
                    .unwrap_or_default()
            },
            as_of,
            req.valid_at, // D2: independent valid-time axis; None = backward-compatible
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
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 , granularity: None},
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
        valid_at: None,
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
        valid_at: None,
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

    // ── DIAG-valid-at-supersession: probe test ───────────────────────────────────
    //
    // KNOWN GAP documented by DIAG-valid-at-supersession (2026-06-28):
    //
    // This test exercises the headline bi-temporal valid_at scenario through the PUBLIC
    // ingest → query API (IngestClaimUseCase + QueryMemoryUseCase), NOT via direct
    // persistence writes.
    //
    // Scenario:
    //   - Ingest alice as CEO [2020-01-01, 2022-01-01), confident (0.9), External.
    //   - Ingest bob  as CEO [2022-01-01, 2024-01-01), confident (0.9), External.
    //   - Windows are NON-OVERLAPPING → reconciler classifies as Succession → CommittedCheap.
    //   - alice's disposition remains CommittedCheap (not Superseded) because ingest-time
    //     supersession was deliberately removed (ingest_claim.rs line 362-379 comment).
    //   - Query valid_at = 2021-06-01 (in alice's window) → expect alice.
    //   - Query valid_at = 2023-06-01 (in bob's window)   → expect bob.
    //
    // If this test PASSES: the succession fold works end-to-end through the public API.
    // If this test FAILS:  the valid_at gap is confirmed. The #[ignore] attribute is set
    //                      so CI does not break. Remove #[ignore] once the fix is shipped.
    //
    // ROOT CAUSE (if failing):
    //   The MockStore used here returns empty for load_ledger_for_claims. The QueryMemory
    //   use-case calls load_ledger_for_claims to build the latest_disposition map used by
    //   the fold's disposition-based liveness filter. When the ledger is empty, both alice
    //   and bob have NO disposition entry → both treated as live (default = true). The fold
    //   should then detect a trusted succession and select via valid_at. If the test STILL
    //   fails with an empty ledger mock, it means there is a deeper fold logic issue.
    //
    //   The real scenario (with a proper store returning ledger entries) would show both
    //   claims with CommittedCheap → not excluded → both live → succession select fires.
    //
    // RECOMMENDED FIX (NOT implemented here):
    //   No code change needed in the core fold or reconciler. The issue, if present, is
    //   that the B7 gate temporal coherence check quarantines valid_time_start values that
    //   are in the past relative to tx_time. The test constructs claims with past valid_time
    //   starts but FUTURE tx_times (injected as "now"), which is correct. The gate allows
    //   this (B7 only fires when start > tx_time). Both claims should reach CommittedCheap.
    //   If queries return wrong values, the fix is to ensure load_ledger_for_claims returns
    //   real ledger entries in the store, or to audit whether both claim dispositions are
    //   correctly recorded as CommittedCheap.
    // NOTE: #[ignore] is set as a PROBE MARKER per task DIAG-valid-at-supersession so CI
    // does not run this test by default (it requires --include-ignored). It currently PASSES,
    // confirming the prior engineer's hypothesis was REFUTED: the gap does NOT exist for the
    // standard ingest path (ingest-time supersession was already removed). Remove the
    // #[ignore] when the diagnosis is complete and this test is promoted to a regression guard.
    #[ignore = "DIAG-valid-at-supersession: probe test (currently PASSES — gap refuted for standard path)"]
    #[test]
    fn diag_valid_at_supersession_public_api_succession_fold() {
        use crate::application::ingest_claim::IngestClaimUseCase;
        use crate::noop::NoOpOracle;
        use chrono::TimeZone;
        use mempill_types::BeliefStatus;

        // A shared mock store that tracks claims AND ledger entries for both use-cases.
        // This is required because QueryMemoryUseCase.load_ledger_for_claims must return
        // the real ledger entries committed by IngestClaimUseCase.
        #[derive(Default)]
        struct FullMockStore {
            claims: Mutex<Vec<Claim>>,
            ledger: Mutex<Vec<LedgerEntry>>,
            assertions: Mutex<Vec<ValidityAssertion>>,
        }

        impl PersistencePort for FullMockStore {
            type Transaction = MockTxn;
            type Error = MockErr;

            fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> {
                Ok(MockTxn(aid.clone()))
            }
            fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> {
                self.claims.lock().unwrap().push(c.clone());
                Ok(c.claim_ref().clone())
            }
            fn append_validity_assertion(&self, _t: &mut MockTxn, a: &ValidityAssertion) -> Result<(), MockErr> {
                self.assertions.lock().unwrap().push(a.clone());
                Ok(())
            }
            fn append_ledger_entry(&self, _t: &mut MockTxn, e: &LedgerEntry) -> Result<(), MockErr> {
                self.ledger.lock().unwrap().push(e.clone());
                Ok(())
            }
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
            fn load_claim(&self, _aid: &AgentId, r: &ClaimRef) -> Result<Option<Claim>, MockErr> {
                let claims = self.claims.lock().unwrap();
                Ok(claims.iter().find(|c| c.claim_ref() == r).cloned())
            }
            fn load_validity_assertions_for(&self, _aid: &AgentId, r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> {
                let assertions = self.assertions.lock().unwrap();
                Ok(assertions.iter().filter(|a| &a.target_claim == r).cloned().collect())
            }
            fn load_ledger(&self, _aid: &AgentId, _from: Option<&mempill_types::TransactionTime>, _lim: usize) -> Result<Vec<LedgerEntry>, MockErr> {
                Ok(self.ledger.lock().unwrap().clone())
            }
            fn load_ledger_for_claims(&self, _aid: &AgentId, refs: &[ClaimRef]) -> Result<Vec<LedgerEntry>, MockErr> {
                let ledger = self.ledger.lock().unwrap();
                Ok(ledger.iter().filter(|e| refs.contains(&e.claim_ref)).cloned().collect())
            }
            fn load_edges_for(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
            fn load_injected_claims(&self, _aid: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
            fn load_lineage(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        }

        let store = Arc::new(FullMockStore::default());

        // tx_time for both ingests must be >= valid_time_start to pass B7 gate check.
        // Use a tx_time well after both valid windows end (2025-01-01 > 2024-01-01).
        let tx_now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let agent = AgentId("diag-agent".into());

        let ingest_uc = IngestClaimUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpOracle>>,
            None,
            EngineConfig::default(),
        );

        // Ingest alice: CEO [2020-01-01, 2022-01-01), confident.
        let alice_req = crate::application::dto::IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("alice"),
            provenance: mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap()),
                end: Some(Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap()),
                valid_time_confidence: 0.9,
                granularity: None,
            }),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };
        let alice_resp = ingest_uc.execute_with_time(alice_req, tx_now).unwrap();

        // ASSERT: alice must be CommittedCheap (first write, no incumbent).
        assert_eq!(
            alice_resp.disposition,
            mempill_types::Disposition::CommittedCheap,
            "alice (first write) must be CommittedCheap"
        );

        // Ingest bob: CEO [2022-01-01, 2024-01-01), confident. Non-overlapping → Succession.
        let bob_req = crate::application::dto::IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("bob"),
            provenance: mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap()),
                end: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
                valid_time_confidence: 0.9,
                granularity: None,
            }),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };
        let bob_resp = ingest_uc.execute_with_time(bob_req, tx_now).unwrap();

        // ASSERT: bob must be CommittedCheap via the Succession route (NOT Contested).
        // If bob is Contested here, the reconciler did NOT detect the succession.
        assert_eq!(
            bob_resp.disposition,
            mempill_types::Disposition::CommittedCheap,
            "bob (successor, non-overlapping windows) must route to CommittedCheap via Succession gate (not Contested)"
        );

        // Verify alice's disposition in the ledger: it should still be CommittedCheap.
        // Ingest-time supersession was removed — alice must NOT be Superseded.
        {
            let ledger = store.ledger.lock().unwrap();
            let alice_dispositions: Vec<_> = ledger.iter()
                .filter(|e| e.claim_ref == alice_resp.claim_ref)
                .map(|e| e.disposition.clone())
                .collect();
            // alice should have exactly one ledger entry: ClaimCommitted/CommittedCheap.
            // If it has a second entry with Superseded, ingest-time supersession crept back in.
            let latest_alice = alice_dispositions.last().cloned();
            assert_eq!(
                latest_alice,
                Some(mempill_types::Disposition::CommittedCheap),
                "alice's latest ledger disposition must be CommittedCheap (not Superseded) — \
                 ingest-time supersession was removed; predecessor stays live until oracle affirms"
            );
        }

        // Now query with valid_at = 2021-06-01 (in alice's window [2020, 2022)).
        // Expected: alice is returned (Resolved, primary.value = "alice").
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            EngineConfig::default(),
        );

        let query_now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let q_alice = QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            as_of_tx_time: None,
            valid_at: Some(Utc.with_ymd_and_hms(2021, 6, 1, 0, 0, 0).unwrap()),
        };
        let resp_alice = query_uc.execute_with_time(q_alice, query_now).unwrap();

        // ASSERT: valid_at=2021-06-01 must return alice.
        // If this fails, the test documents the known gap:
        //   - Check resp_alice.belief.status and resp_alice.belief.primary for the actual value.
        let alice_primary = resp_alice.belief.primary.as_ref();
        assert_eq!(
            resp_alice.belief.status,
            BeliefStatus::Resolved,
            "DIAG-valid-at-supersession GAP: valid_at=2021-06-01 (alice's window) \
             returned {:?} instead of Resolved. Primary: {:?}",
            resp_alice.belief.status,
            alice_primary.map(|b| &b.fact.value)
        );
        assert_eq!(
            alice_primary.map(|b| b.fact.value.clone()),
            Some(serde_json::json!("alice")),
            "DIAG-valid-at-supersession GAP: valid_at=2021-06-01 must return alice, \
             got {:?}",
            alice_primary.map(|b| &b.fact.value)
        );

        // Query with valid_at = 2023-06-01 (in bob's window [2022, 2024)).
        // Expected: bob is returned (Resolved, primary.value = "bob").
        let q_bob = QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            as_of_tx_time: None,
            valid_at: Some(Utc.with_ymd_and_hms(2023, 6, 1, 0, 0, 0).unwrap()),
        };
        let resp_bob = query_uc.execute_with_time(q_bob, query_now).unwrap();

        let bob_primary = resp_bob.belief.primary.as_ref();
        assert_eq!(
            resp_bob.belief.status,
            BeliefStatus::Resolved,
            "DIAG-valid-at-supersession GAP: valid_at=2023-06-01 (bob's window) \
             returned {:?} instead of Resolved. Primary: {:?}",
            resp_bob.belief.status,
            bob_primary.map(|b| &b.fact.value)
        );
        assert_eq!(
            bob_primary.map(|b| b.fact.value.clone()),
            Some(serde_json::json!("bob")),
            "DIAG-valid-at-supersession GAP: valid_at=2023-06-01 must return bob, \
             got {:?}",
            bob_primary.map(|b| &b.fact.value)
        );
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
        valid_at: None,
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
