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
        // Pass the same as_of cutoff to the ledger load so that post-T supersession
        // entries are excluded, preserving correct bi-temporal tx-time travel.
        let all_ledger = self.persistence
            .load_ledger_for_claims(&req.agent_id, &claim_refs, Some(as_of))
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
        fn load_ledger_for_claims(&self, _aid: &AgentId, _refs: &[ClaimRef], _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
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

    // ── Regression: valid_at point-in-time selection works end-to-end via public ingest API ──
    //
    // Bi-temporal property guarded: querying valid_at = T returns the claim whose valid-time
    // window contains T, even when multiple non-overlapping claims exist on the same subject-line.
    //
    // WHY this matters: the succession fold (truth_engine Step 4) selects the single claim
    // whose half-open [start, end) window covers the query instant. This test proves the full
    // chain — public IngestClaimUseCase → QueryMemoryUseCase — correctly surfaces alice for
    // valid_at=2021 and bob for valid_at=2023, without ingest-time supersession collapsing
    // alice's claim before the query even runs.
    //
    // Scenario: alice=CEO [2020,2022), bob=CEO [2022,2024) — non-overlapping, Succession route.
    // alice stays CommittedCheap (ingest-time supersession was removed per ingest_claim.rs L362).
    #[test]
    fn valid_at_succession_two_windows_selects_correct_ceo_via_public_api() {
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
            fn load_ledger_for_claims(&self, _aid: &AgentId, refs: &[ClaimRef], _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<LedgerEntry>, MockErr> {
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

        // valid_at=2021-06-01 is in alice's window [2020, 2022) → must return alice.
        let alice_primary = resp_alice.belief.primary.as_ref();
        assert_eq!(
            resp_alice.belief.status,
            BeliefStatus::Resolved,
            "valid_at=2021-06-01 (alice's window [2020,2022)) must be Resolved, got {:?}; primary={:?}",
            resp_alice.belief.status,
            alice_primary.map(|b| &b.fact.value)
        );
        assert_eq!(
            alice_primary.map(|b| b.fact.value.clone()),
            Some(serde_json::json!("alice")),
            "valid_at=2021-06-01 must return alice, got {:?}",
            alice_primary.map(|b| &b.fact.value)
        );

        // valid_at=2023-06-01 is in bob's window [2022, 2024) → must return bob.
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
            "valid_at=2023-06-01 (bob's window [2022,2024)) must be Resolved, got {:?}; primary={:?}",
            resp_bob.belief.status,
            bob_primary.map(|b| &b.fact.value)
        );
        assert_eq!(
            bob_primary.map(|b| b.fact.value.clone()),
            Some(serde_json::json!("bob")),
            "valid_at=2023-06-01 must return bob, got {:?}",
            bob_primary.map(|b| &b.fact.value)
        );
    }

    // ── Regression: oracle adjudication → Affirm → correct belief surfaced via public API ──
    //
    // Bi-temporal property guarded: after a conflict is routed to the oracle and resolved
    // with an Affirm verdict, the winner (bob) is surfaced as the canonical belief and
    // the loser (alice) is Superseded. This tests the full oracle composition path:
    // IngestClaimUseCase (oracle present) → QueuedForAdjudication pending row →
    // SubmitAdjudicationUseCase(Affirm) → QueryMemoryUseCase.
    //
    // NOTE — bi-temporal tx-time rewind limitation (not tested here):
    // Querying with as_of_tx_time set to a point BEFORE the Affirm was issued would
    // ideally return alice (the tx-time axis rewinds past the supersession). However,
    // QueryMemoryUseCase.load_ledger_for_claims does NOT accept an as_of_tx_time
    // parameter and returns ALL ledger entries regardless of recorded_at. As a result,
    // build_latest_disposition_map sees alice's Superseded entry (written at affirm_time)
    // even when querying before that time. The tx-time axis IS correctly applied to
    // ValidityAssertion::Bound (truth_engine.rs ~L123), but the disposition-based
    // liveness filter is not tx-time filtered. This means alice is incorrectly excluded
    // from the live set even at pre-affirm as_of_tx_time. See BLOCKER in task output.
    #[test]
    fn oracle_affirm_surfaces_winner_and_excludes_loser_via_public_api() {
        use crate::application::ingest_claim::IngestClaimUseCase;
        use crate::application::submit_adjudication::SubmitAdjudicationUseCase;
        use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
        use crate::ports::{PendingAdjudicationPort, PendingAdjudicationRow};
        use chrono::TimeZone;
        use mempill_types::{BeliefStatus, Disposition};

        // ── Oracle that returns a deterministic handle UUID ───────────────────
        struct TestOracle {
            fixed_uuid: uuid::Uuid,
        }
        impl crate::ports::OraclePort for TestOracle {
            type Error = crate::noop::NoOpError;
            type Handle = uuid::Uuid;
            fn request_adjudication(
                &self,
                _aid: &AgentId,
                _req: mempill_types::AdjudicationRequest,
            ) -> Result<uuid::Uuid, crate::noop::NoOpError> {
                Ok(self.fixed_uuid)
            }
            fn handle_to_uuid(h: &uuid::Uuid) -> uuid::Uuid { *h }
        }

        // ── Mock pending-adjudication store ───────────────────────────────────
        #[derive(Default)]
        struct MockPending {
            rows: Mutex<Vec<PendingAdjudicationRow>>,
        }
        impl PendingAdjudicationPort for MockPending {
            type Error = MockErr;
            fn insert_pending(&self, r: &PendingAdjudicationRow) -> Result<(), MockErr> {
                self.rows.lock().unwrap().push(r.clone()); Ok(())
            }
            fn get_pending(&self, id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> {
                Ok(self.rows.lock().unwrap().iter().find(|r| r.handle_id == id).cloned())
            }
            fn list_pending(&self, _: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
                Ok(self.rows.lock().unwrap().clone())
            }
            fn list_expired(&self, _: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
                Ok(vec![])
            }
            fn mark_resolved(&self, id: uuid::Uuid) -> Result<(), MockErr> {
                for r in self.rows.lock().unwrap().iter_mut() {
                    if r.handle_id == id { r.status = "resolved".to_string(); }
                }
                Ok(())
            }
            fn mark_expired(&self, _: uuid::Uuid) -> Result<(), MockErr> { Ok(()) }
            fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> {
                Ok(vec![])
            }
        }

        // ── Shared mock persistence that tracks all written state ─────────────
        // FullMockStore2 tracks claims, ledger, and assertions for all three use-cases.
        #[derive(Default)]
        struct FullMockStore2 {
            claims: Mutex<Vec<Claim>>,
            ledger: Mutex<Vec<LedgerEntry>>,
            assertions: Mutex<Vec<ValidityAssertion>>,
        }
        impl PersistencePort for FullMockStore2 {
            type Transaction = MockTxn;
            type Error = MockErr;
            fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(aid.clone())) }
            fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> {
                self.claims.lock().unwrap().push(c.clone());
                Ok(c.claim_ref().clone())
            }
            fn append_validity_assertion(&self, _t: &mut MockTxn, a: &ValidityAssertion) -> Result<(), MockErr> {
                self.assertions.lock().unwrap().push(a.clone()); Ok(())
            }
            fn append_ledger_entry(&self, _t: &mut MockTxn, e: &LedgerEntry) -> Result<(), MockErr> {
                self.ledger.lock().unwrap().push(e.clone()); Ok(())
            }
            fn append_claim_edge(&self, _t: &mut MockTxn, _e: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
            fn commit(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
            fn rollback(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
            fn load_subject_line(&self, _aid: &AgentId, subject: &str, predicate: &str) -> Result<Vec<Claim>, MockErr> {
                Ok(self.claims.lock().unwrap().iter()
                    .filter(|c| c.fact().subject == subject && c.fact().predicate == predicate)
                    .cloned().collect())
            }
            fn load_claim(&self, _aid: &AgentId, r: &ClaimRef) -> Result<Option<Claim>, MockErr> {
                Ok(self.claims.lock().unwrap().iter().find(|c| c.claim_ref() == r).cloned())
            }
            fn load_validity_assertions_for(&self, _aid: &AgentId, r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> {
                Ok(self.assertions.lock().unwrap().iter().filter(|a| &a.target_claim == r).cloned().collect())
            }
            fn load_ledger(&self, _aid: &AgentId, _from: Option<&mempill_types::TransactionTime>, _lim: usize) -> Result<Vec<LedgerEntry>, MockErr> {
                Ok(self.ledger.lock().unwrap().clone())
            }
            fn load_ledger_for_claims(&self, _aid: &AgentId, refs: &[ClaimRef], as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<LedgerEntry>, MockErr> {
                Ok(self.ledger.lock().unwrap().iter()
                    .filter(|e| refs.contains(&e.claim_ref))
                    .filter(|e| as_of.is_none_or(|t| e.recorded_at.0 <= t))
                    .cloned()
                    .collect())
            }
            fn load_edges_for(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
            fn load_injected_claims(&self, _aid: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
            fn load_lineage(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        }

        let store = Arc::new(FullMockStore2::default());
        let pending = Arc::new(MockPending::default());
        let handle_uuid = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: handle_uuid });

        // Erase pending store type for IngestClaimUseCase + SubmitAdjudicationUseCase.
        let erased_pending: Arc<dyn ErasedPendingStore> = {
            struct Delegate(Arc<MockPending>);
            impl PendingAdjudicationPort for Delegate {
                type Error = MockErr;
                fn insert_pending(&self, r: &PendingAdjudicationRow) -> Result<(), MockErr> { self.0.insert_pending(r) }
                fn get_pending(&self, id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> { self.0.get_pending(id) }
                fn list_pending(&self, a: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> { self.0.list_pending(a) }
                fn list_expired(&self, n: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> { self.0.list_expired(n) }
                fn mark_resolved(&self, id: uuid::Uuid) -> Result<(), MockErr> { self.0.mark_resolved(id) }
                fn mark_expired(&self, id: uuid::Uuid) -> Result<(), MockErr> { self.0.mark_expired(id) }
                fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> { self.0.list_queued_orphan_claims() }
            }
            Arc::new(ErasedPendingStoreAdapter::new(Delegate(Arc::clone(&pending))))
        };

        let agent = AgentId("oracle-vt-agent".into());
        let ingest_now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        let ingest_uc = IngestClaimUseCase::new(
            Arc::clone(&store),
            Some(Arc::clone(&oracle)),
            Some(Arc::clone(&erased_pending)),
            EngineConfig::default(),
        );

        // ── Step 1: Ingest alice — first claim, no conflict, CommittedCheap ──
        let alice_req = crate::application::dto::IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("alice"),
            provenance: mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None, // no valid_time → conflict on second write
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };
        let alice_resp = ingest_uc.execute_with_time(alice_req, ingest_now).unwrap();
        assert_eq!(alice_resp.disposition, Disposition::CommittedCheap,
            "alice (first write, no conflict) must be CommittedCheap");
        let alice_ref = alice_resp.claim_ref.clone();

        // ── Step 2: Ingest bob — conflicts with alice, oracle routes to QueuedForAdjudication ──
        let bob_req = crate::application::dto::IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("bob"),
            provenance: mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };
        let bob_resp = ingest_uc.execute_with_time(bob_req, ingest_now).unwrap();
        assert_eq!(bob_resp.disposition, Disposition::QueuedForAdjudication,
            "bob (conflict, oracle present) must be QueuedForAdjudication");
        let bob_ref = bob_resp.claim_ref.clone();

        // ── Step 3: Submit Affirm → alice Superseded, bob CommittedCheap ─────
        // The affirm_time is distinct from ingest_now so tx-time axis is separable.
        let affirm_now = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let submit_uc = SubmitAdjudicationUseCase::new(
            Arc::clone(&store),
            Arc::clone(&erased_pending),
        );
        let adj_response = mempill_types::AdjudicationResponse {
            handle_id: handle_uuid,
            verdict: mempill_types::AdjudicationVerdict::Affirm,
            evidence_provenance: mempill_types::ProvenanceLabel::External(
                mempill_types::ExternalKind::ExternalFirstHand,
            ),
        };
        let outcome = submit_uc.execute(handle_uuid, adj_response, affirm_now).unwrap();
        assert_eq!(outcome.disposition, Disposition::CommittedCheap,
            "Affirm outcome must be CommittedCheap for the winner (bob)");
        assert_eq!(outcome.claim_ref, bob_ref,
            "Affirm outcome claim_ref must be bob (the challenger/winner)");

        // Verify alice's ledger shows Superseded after Affirm.
        {
            let ledger = store.ledger.lock().unwrap();
            let alice_latest = ledger.iter()
                .filter(|e| e.claim_ref == alice_ref)
                .max_by_key(|e| e.recorded_at.0)
                .map(|e| e.disposition.clone());
            assert_eq!(alice_latest, Some(Disposition::Superseded),
                "alice's latest ledger entry must be Superseded after Affirm");
        }

        // ── Step 4: Query as_of=None (current view) → bob is the canonical belief ──
        //
        // With alice Superseded and bob CommittedCheap, bob is the single live claim.
        // The correct bi-temporal answer for as_of=now is bob (Resolved).
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<crate::noop::NoOpVector>>,
            EngineConfig::default(),
        );
        let query_now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let q_now = crate::application::dto::QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            as_of_tx_time: None,
            valid_at: None,
        };
        let resp_now = query_uc.execute_with_time(q_now, query_now).unwrap();
        let now_primary = resp_now.belief.primary.as_ref()
            .map(|b| b.fact.value.clone());
        // Bob has no valid_time → TimingUncertain (single live claim whose valid_time.is_unknown()).
        // The claim IS surfaced as the primary belief — TimingUncertain means "we have a belief
        // but don't know the exact valid-time window", which is correct for a no-valid_time claim.
        assert!(
            matches!(resp_now.belief.status, BeliefStatus::TimingUncertain | BeliefStatus::Resolved),
            "as_of=now after Affirm must surface bob (TimingUncertain or Resolved); got {:?}",
            resp_now.belief.status
        );
        assert_eq!(
            now_primary,
            Some(serde_json::json!("bob")),
            "as_of=now after Affirm must return bob (CommittedCheap winner); got {now_primary:?}"
        );

        // ── Step 5: Query as_of=before_affirm — bi-temporal tx-time rewind ───
        //
        // Correct bi-temporal behavior (D2 independence rule):
        //   At as_of_tx_time before affirm_now, alice's Superseded ledger entry (recorded_at=affirm_now)
        //   and any Bound ValidityAssertion from the Affirm are INVISIBLE (recorded_at > as_of).
        //   Alice is live (CommittedCheap as of that tx-time) and bob is QueuedForAdjudication.
        //   The fold sees two non-superseded claims → Contested.
        //
        // Implementation: load_ledger_for_claims now accepts as_of_tx_time and filters
        //   recorded_at <= T, so the disposition map correctly excludes the post-affirm
        //   Superseded entry for alice.
        let before_affirm = Utc.with_ymd_and_hms(2025, 3, 1, 0, 0, 0).unwrap(); // between ingest_now and affirm_now
        let q_before_affirm = crate::application::dto::QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "acme".into(),
            predicate: "ceo".into(),
            as_of_tx_time: Some(before_affirm),
            valid_at: None,
        };
        let resp_before = query_uc.execute_with_time(q_before_affirm, query_now).unwrap();
        // At as_of=before_affirm both alice (CommittedCheap) and bob (QueuedForAdjudication) are
        // visible and neither is superseded → Contested is the correct bi-temporal answer.
        assert_eq!(
            resp_before.belief.status,
            BeliefStatus::Contested,
            "as_of=before_affirm: bi-temporal tx-time rewind must return Contested \
             (both alice and bob live before affirm). Got {:?}",
            resp_before.belief.status
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
