//! SubmitAdjudicationUseCase — atomic verdict apply (TASK-9 W4, ORACLE_DESIGN §C.4, I9).
//!
//! This is the RESOLUTION path: an oracle verdict arrives asynchronously and this use-case
//! applies the transition atomically. All reads happen BEFORE begin_atomic; all writes
//! happen inside a single begin_atomic/commit unit.
//!
//! # Disposition transitions per ORACLE_DESIGN C.4
//!
//! | Verdict | Challenger | Incumbent | Ledger entries |
//! |---------|-----------|-----------|----------------|
//! | Affirm  | → CommittedCheap (new ledger w/ External provenance) | → Superseded (ValidityAssertion Bound + ledger) | 2 |
//! | Deny    | → Superseded (ValidityAssertion Bound + ledger) | stays committed (no change) | 1 |
//! | Unknown | → Contested (ledger abstain entry) | → Contested (ledger abstain entry) | 2 (one per claim) |
//!
//! # Lock invariant (I9)
//!
//! This use-case is SYNC — it runs inside spawn_blocking. The EngineHandle (W5) acquires
//! store_write_lock then per-agent write lock BEFORE spawn_blocking, exactly mirroring ingest.
//! This use-case must NOT acquire locks itself.
//!
//! # Transaction discipline
//!
//! Steps 1 and 3 (reads) execute BEFORE begin_atomic. Steps 4–6 (writes) execute inside
//! one begin_atomic/commit unit. On any error inside the unit: rollback is called and Err
//! is returned (I9 — no partial writes).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use mempill_types::{
    AgentId, AdjudicationOutcome, AdjudicationVerdict, AssertionKind, Disposition,
    LedgerEntry, LedgerEventKind, TransactionTime, ValidityAssertion,
};

use crate::{
    engine_handle::ErasedPendingStore,
    error::MemError,
    ports::PersistencePort,
};

/// Sync use-case: apply an oracle verdict atomically.
///
/// Invoked via `spawn_blocking` by `EngineHandle::submit_adjudication`.
pub struct SubmitAdjudicationUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    pending_store: Arc<dyn ErasedPendingStore>,
}

impl<P> SubmitAdjudicationUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, pending_store: Arc<dyn ErasedPendingStore>) -> Self {
        Self { persistence, pending_store }
    }

    /// Execute the verdict-apply algorithm (W4 spec).
    ///
    /// `now` is engine-stamped at the async boundary and passed in (DETERMINISM).
    pub fn execute(
        &self,
        handle_id: uuid::Uuid,
        response: mempill_types::AdjudicationResponse,
        now: DateTime<Utc>,
    ) -> Result<AdjudicationOutcome, MemError> {
        let tx_time = TransactionTime(now);

        // ── Step 1: Look up the pending row in the DB (DB-authoritative, Amendment 1) ──
        let row = self.pending_store
            .get_pending_erased(handle_id)
            .map_err(|e| MemError::PendingStore { source: e })?
            .ok_or(MemError::AdjudicationHandleNotFound { handle_id })?;

        // ── Lazy expiry: if TTL has elapsed, revert challenger → Contested + ledger ──
        // Do NOT apply the verdict on an expired handle. Revert atomically then return
        // AdjudicationHandleNotFound so the caller knows the handle is no longer active.
        if let Some(expires_at) = row.expires_at {
            if expires_at <= now {
                // Revert: write Contested ledger entry for the challenger, mark row expired.
                let agent_id_exp: AgentId = row.agent_id.clone();
                let challenger_ref_exp = row.challenger_claim_ref.clone();
                let handle_id_exp = handle_id;

                // Load ledger to verify challenger is still QueuedForAdjudication.
                let ledger_check = self.persistence
                    .load_ledger(&agent_id_exp, None, 10_000)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                let challenger_disp = latest_disposition_from_ledger(&ledger_check, &challenger_ref_exp);

                if challenger_disp == Some(Disposition::QueuedForAdjudication) {
                    // Write Contested ledger entry inside a transaction.
                    let mut txn = self.persistence
                        .begin_atomic(&agent_id_exp)
                        .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                    let expired_entry = mempill_types::LedgerEntry {
                        entry_id: uuid::Uuid::new_v4(),
                        agent_id: agent_id_exp.clone(),
                        claim_ref: challenger_ref_exp.clone(),
                        event_kind: LedgerEventKind::AdjudicationExpired,
                        disposition: Disposition::Contested,
                        rationale: Some(serde_json::json!({
                            "event": "adjudication_ttl_expired_lazy",
                            "handle_id": handle_id_exp.to_string(),
                            "expired_at": expires_at.to_rfc3339(),
                            "incumbent_claim_ref": row.incumbent_claim_ref.0.to_string(),
                        })),
                        recorded_at: tx_time.clone(),
                    };

                    match self.persistence.append_ledger_entry(&mut txn, &expired_entry) {
                        Ok(()) => {
                            if let Err(e) = self.persistence.commit(txn) {
                                return Err(MemError::Persistence { source: Box::new(e) });
                            }
                            // Mark pending row expired outside txn (within write lock).
                            let _ = self.pending_store.mark_expired_erased(handle_id_exp);
                        }
                        Err(e) => {
                            let _ = self.persistence.rollback(txn);
                            return Err(MemError::Persistence { source: Box::new(e) });
                        }
                    }
                }

                return Err(MemError::AdjudicationHandleNotFound { handle_id });
            }
        }

        let agent_id: AgentId = row.agent_id.clone();
        let challenger_ref = row.challenger_claim_ref.clone();
        let incumbent_ref = row.incumbent_claim_ref.clone();

        // ── Step 3: State guard — load latest dispositions BEFORE begin_atomic ────
        // Check that both claims are still in QueuedForAdjudication (idempotency guard R6).
        // We read the ledger to get the latest disposition per claim.
        let ledger = self.persistence
            .load_ledger(&agent_id, None, 10_000)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let challenger_disp = latest_disposition_from_ledger(&ledger, &challenger_ref);

        // State guard: the challenger must still be QueuedForAdjudication.
        // The incumbent is NOT checked here — it was never moved to QueuedForAdjudication
        // (only the challenger is gated there). Checking only the challenger is the correct
        // idempotency guard: a duplicate submit finds the challenger already resolved
        // (CommittedCheap / Superseded / Contested) and returns AdjudicationHandleNotFound.
        if challenger_disp != Some(Disposition::QueuedForAdjudication) {
            return Err(MemError::AdjudicationHandleNotFound { handle_id });
        }

        // Pre-load edges for supersession BEFORE begin_atomic (DEFECT-1 fix pattern).
        // For Affirm: need to bound the incumbent → load its edges.
        // For Deny: need to bound the challenger → load its edges.
        let incumbent_edges = self.persistence
            .load_edges_for(&agent_id, &incumbent_ref)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let challenger_edges = self.persistence
            .load_edges_for(&agent_id, &challenger_ref)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // ── Step 2 / Step 4–6: begin_atomic + apply verdict + mark resolved + commit ──
        let mut txn = self.persistence
            .begin_atomic(&agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let result = self.apply_verdict_within_txn(
            &response.verdict,
            &response.evidence_provenance,
            &agent_id,
            &challenger_ref,
            &incumbent_ref,
            tx_time.clone(),
            &incumbent_edges,
            &challenger_edges,
            &mut txn,
        );

        match result {
            Ok(()) => {
                self.persistence
                    .commit(txn)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                // ── Step 5: Mark the pending row resolved (outside txn, within write lock) ──
                self.pending_store
                    .mark_resolved_erased(handle_id)
                    .map_err(|e| MemError::PendingStore { source: e })?;

                // ── Step 7: Return AdjudicationOutcome ───────────────────────────────
                let (outcome_claim_ref, final_disposition) = match &response.verdict {
                    AdjudicationVerdict::Affirm => {
                        (challenger_ref, Disposition::CommittedCheap)
                    }
                    AdjudicationVerdict::Deny => {
                        (challenger_ref, Disposition::Superseded)
                    }
                    AdjudicationVerdict::Unknown => {
                        // Return the challenger with its new Contested disposition.
                        (challenger_ref, Disposition::Contested)
                    }
                };

                Ok(AdjudicationOutcome {
                    handle_id,
                    disposition: final_disposition,
                    claim_ref: outcome_claim_ref,
                })
            }
            Err(e) => {
                let _ = self.persistence.rollback(txn);
                Err(e)
            }
        }
    }

    /// Apply the verdict inside the already-open transaction.
    ///
    /// Returns `Ok(())` on success; caller derives the outcome disposition from the verdict.
    #[allow(clippy::too_many_arguments)]
    fn apply_verdict_within_txn(
        &self,
        verdict: &AdjudicationVerdict,
        evidence_provenance: &mempill_types::ProvenanceLabel,
        agent_id: &AgentId,
        challenger_ref: &mempill_types::ClaimRef,
        incumbent_ref: &mempill_types::ClaimRef,
        tx_time: TransactionTime,
        incumbent_edges: &[mempill_types::ClaimEdge],
        challenger_edges: &[mempill_types::ClaimEdge],
        txn: &mut P::Transaction,
    ) -> Result<(), MemError> {
        match verdict {
            AdjudicationVerdict::Affirm => {
                // Affirm: challenger wins.
                // 1. Bound the incumbent (→ Superseded) + ledger entry.
                self.bound_claim(
                    agent_id,
                    incumbent_ref,
                    challenger_ref,
                    tx_time.clone(),
                    incumbent_edges,
                    txn,
                )?;
                // 2. Write ledger entry for challenger → CommittedCheap with External provenance.
                let affirm_entry = LedgerEntry {
                    entry_id: uuid::Uuid::new_v4(),
                    agent_id: agent_id.clone(),
                    claim_ref: challenger_ref.clone(),
                    event_kind: LedgerEventKind::AdjudicationResolved,
                    disposition: Disposition::CommittedCheap,
                    rationale: Some(serde_json::json!({
                        "event": "oracle_affirm",
                        "verdict": "Affirm",
                        "evidence_provenance": serde_json::to_value(evidence_provenance).ok(),
                    })),
                    recorded_at: tx_time.clone(),
                };
                self.persistence
                    .append_ledger_entry(txn, &affirm_entry)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                Ok(())
            }

            AdjudicationVerdict::Deny => {
                // Deny: incumbent stands.
                // 1. Bound the challenger (→ Superseded) + ledger entry.
                self.bound_claim(
                    agent_id,
                    challenger_ref,
                    incumbent_ref,
                    tx_time.clone(),
                    challenger_edges,
                    txn,
                )?;
                // Incumbent disposition is unchanged — no ledger entry needed for it.
                Ok(())
            }

            AdjudicationVerdict::Unknown => {
                // Unknown: abstain — no supersession.
                // Transition both claims from QueuedForAdjudication → Contested.
                // Write one abstain ledger entry per claim.
                let rationale = serde_json::json!({
                    "event": "oracle_abstain",
                    "verdict": "Unknown",
                });
                let challenger_entry = LedgerEntry {
                    entry_id: uuid::Uuid::new_v4(),
                    agent_id: agent_id.clone(),
                    claim_ref: challenger_ref.clone(),
                    event_kind: LedgerEventKind::AdjudicationResolved,
                    disposition: Disposition::Contested,
                    rationale: Some(rationale.clone()),
                    recorded_at: tx_time.clone(),
                };
                self.persistence
                    .append_ledger_entry(txn, &challenger_entry)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                let incumbent_entry = LedgerEntry {
                    entry_id: uuid::Uuid::new_v4(),
                    agent_id: agent_id.clone(),
                    claim_ref: incumbent_ref.clone(),
                    event_kind: LedgerEventKind::AdjudicationResolved,
                    disposition: Disposition::Contested,
                    rationale: Some(rationale),
                    recorded_at: tx_time.clone(),
                };
                self.persistence
                    .append_ledger_entry(txn, &incumbent_entry)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                Ok(())
            }
        }
    }

    /// Write a Bound ValidityAssertion + Superseded ledger entry for `target_ref`
    /// inside the open transaction (reuses the supersession.rs pattern).
    ///
    /// `overturning_ref` — the claim that caused the bounding (for rationale).
    /// `preloaded_edges` — DependsOn edges for `target_ref` (loaded before begin_atomic).
    fn bound_claim(
        &self,
        agent_id: &AgentId,
        target_ref: &mempill_types::ClaimRef,
        overturning_ref: &mempill_types::ClaimRef,
        tx_time: TransactionTime,
        preloaded_edges: &[mempill_types::ClaimEdge],
        txn: &mut P::Transaction,
    ) -> Result<(), MemError> {
        use mempill_types::{EdgeKind, ExternalKind, Confidence};

        // Step A: Bound ValidityAssertion (closes the claim's valid window).
        let assertion = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            target_claim: target_ref.clone(),
            kind: AssertionKind::Bound { bound_at: tx_time.0 },
            provenance: mempill_types::ProvenanceLabel::External(
                ExternalKind::ExternalFirstHand,
            ),
            confidence: Confidence {
                value_confidence: 1.0,
                valid_time_confidence: 1.0,
            },
            asserted_at: tx_time.clone(),
        };
        self.persistence
            .append_validity_assertion(txn, &assertion)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Step B: Superseded ledger entry for the bounded claim.
        let ledger_entry = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: target_ref.clone(),
            event_kind: LedgerEventKind::ValidityAsserted,
            disposition: Disposition::Superseded,
            rationale: Some(serde_json::json!({
                "event": "oracle_supersession",
                "overturning_claim": overturning_ref.0.to_string(),
                "bound_at": tx_time.0.to_rfc3339(),
            })),
            recorded_at: tx_time.clone(),
        };
        self.persistence
            .append_ledger_entry(txn, &ledger_entry)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Step C: DependsOn cascade — flag direct dependents of the bounded claim.
        // (Same A26 cascade pattern as supersession.rs — but we inline it here to
        // avoid making supersession::execute pub. Uses the same deduplication logic.)
        use std::collections::HashSet;
        let mut seen: HashSet<mempill_types::ClaimRef> = HashSet::new();
        for edge in preloaded_edges {
            if edge.kind == EdgeKind::DependsOn && edge.to_claim == *target_ref {
                if !seen.insert(edge.from_claim.clone()) {
                    continue;
                }
                let flag_entry = LedgerEntry {
                    entry_id: uuid::Uuid::new_v4(),
                    agent_id: agent_id.clone(),
                    claim_ref: edge.from_claim.clone(),
                    event_kind: LedgerEventKind::DependentFlaggedPendingReview,
                    disposition: Disposition::PendingReview,
                    rationale: Some(serde_json::json!({
                        "event": "depends_on_cascade",
                        "superseded_parent": target_ref.0.to_string(),
                        "overturning_claim": overturning_ref.0.to_string(),
                    })),
                    recorded_at: tx_time.clone(),
                };
                self.persistence
                    .append_ledger_entry(txn, &flag_entry)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
            }
        }
        Ok(())
    }
}

/// Extract the latest `Disposition` for a given `ClaimRef` from a ledger slice.
/// Returns `None` if the claim has no ledger entries.
fn latest_disposition_from_ledger(
    ledger: &[LedgerEntry],
    target: &mempill_types::ClaimRef,
) -> Option<Disposition> {
    ledger
        .iter()
        .filter(|e| &e.claim_ref == target)
        .max_by_key(|e| e.recorded_at.0)
        .map(|e| e.disposition.clone())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
    use crate::ports::{
        PendingAdjudicationPort, PendingAdjudicationRow, PersistencePort, Txn as TxnTrait,
    };
    use mempill_types::{
        AgentId, AdjudicationRequest, AdjudicationResponse, AdjudicationVerdict,
        Cardinality, Claim, ClaimEdge, ClaimRef, Confidence, Criticality, CurrencySignal,
        CurrencyState, Disposition, ExternalAnchor, ExternalKind, Fact, LedgerEntry,
        ProvenanceLabel, TransactionTime, ValidTime, ValidityAssertion,
    };
    use std::sync::Mutex;

    // ── Mock Txn ──────────────────────────────────────────────────────────────

    struct MockTxn(AgentId);
    impl TxnTrait for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    // ── Mock error ────────────────────────────────────────────────────────────

    #[derive(Debug, thiserror::Error)]
    #[error("mock error")]
    struct MockErr;

    // ── Mock persistence (in-memory, append-tracking) ────────────────────────

    #[derive(Default)]
    struct MockStore {
        claims: Mutex<Vec<Claim>>,
        ledger: Mutex<Vec<LedgerEntry>>,
        validity_assertions: Mutex<Vec<ValidityAssertion>>,
        /// When set, causes `append_ledger_entry` to fail on the Nth call (1-indexed).
        fail_on_ledger_write: Mutex<Option<usize>>,
        ledger_write_count: Mutex<usize>,
        rollback_called: Mutex<bool>,
    }

    impl PersistencePort for MockStore {
        type Transaction = MockTxn;
        type Error = MockErr;

        fn begin_atomic(&self, agent_id: &AgentId) -> Result<MockTxn, MockErr> {
            Ok(MockTxn(agent_id.clone()))
        }

        fn append_claim(&self, _: &mut MockTxn, claim: &Claim) -> Result<ClaimRef, MockErr> {
            self.claims.lock().unwrap().push(claim.clone());
            Ok(claim.claim_ref().clone())
        }

        fn append_validity_assertion(
            &self,
            _: &mut MockTxn,
            a: &ValidityAssertion,
        ) -> Result<(), MockErr> {
            self.validity_assertions.lock().unwrap().push(a.clone());
            Ok(())
        }

        fn append_ledger_entry(
            &self,
            _: &mut MockTxn,
            e: &LedgerEntry,
        ) -> Result<(), MockErr> {
            let mut count = self.ledger_write_count.lock().unwrap();
            *count += 1;
            let fail_on = *self.fail_on_ledger_write.lock().unwrap();
            if fail_on == Some(*count) {
                return Err(MockErr);
            }
            self.ledger.lock().unwrap().push(e.clone());
            Ok(())
        }

        fn append_claim_edge(&self, _: &mut MockTxn, _: &ClaimEdge) -> Result<(), MockErr> {
            Ok(())
        }

        fn commit(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }

        fn rollback(&self, _: MockTxn) -> Result<(), MockErr> {
            *self.rollback_called.lock().unwrap() = true;
            Ok(())
        }

        fn load_subject_line(&self, _: &AgentId, _: &str, _: &str) -> Result<Vec<Claim>, MockErr> {
            Ok(self.claims.lock().unwrap().clone())
        }

        fn load_claim(&self, _: &AgentId, r: &ClaimRef) -> Result<Option<Claim>, MockErr> {
            Ok(self.claims.lock().unwrap().iter().find(|c| c.claim_ref() == r).cloned())
        }

        fn load_validity_assertions_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> {
            Ok(vec![])
        }

        fn load_ledger(&self, _: &AgentId, _: Option<&TransactionTime>, _: usize) -> Result<Vec<LedgerEntry>, MockErr> {
            Ok(self.ledger.lock().unwrap().clone())
        }

        fn load_edges_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> {
            Ok(vec![])
        }

        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }

        fn load_lineage(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
    }

    // ── Mock PendingStore ─────────────────────────────────────────────────────

    #[derive(Default)]
    struct MockPendingStore {
        rows: Mutex<Vec<PendingAdjudicationRow>>,
    }

    impl MockPendingStore {
        fn seed(&self, row: PendingAdjudicationRow) {
            self.rows.lock().unwrap().push(row);
        }

        fn is_resolved(&self, handle_id: uuid::Uuid) -> bool {
            self.rows.lock().unwrap().iter()
                .any(|r| r.handle_id == handle_id && r.status == "resolved")
        }
    }

    impl PendingAdjudicationPort for MockPendingStore {
        type Error = MockErr;

        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), MockErr> {
            self.rows.lock().unwrap().push(row.clone());
            Ok(())
        }

        fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> {
            Ok(self.rows.lock().unwrap().iter().find(|r| r.handle_id == handle_id).cloned())
        }

        fn list_pending(&self, agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
            Ok(self.rows.lock().unwrap().iter()
                .filter(|r| agent_id.map_or(true, |a| r.agent_id == *a) && r.status == "pending")
                .cloned()
                .collect())
        }

        fn list_expired(&self, now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
            Ok(self.rows.lock().unwrap().iter()
                .filter(|r| r.status == "pending" && r.expires_at.map_or(false, |e| e <= now))
                .cloned()
                .collect())
        }

        fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), MockErr> {
            for r in self.rows.lock().unwrap().iter_mut() {
                if r.handle_id == handle_id {
                    r.status = "resolved".to_string();
                }
            }
            Ok(())
        }

        fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), MockErr> {
            for r in self.rows.lock().unwrap().iter_mut() {
                if r.handle_id == handle_id {
                    r.status = "expired".to_string();
                }
            }
            Ok(())
        }

        fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> {
            Ok(vec![])
        }
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_agent() -> AgentId { AgentId("test-agent".into()) }

    fn make_claim(agent: &AgentId) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "user".into(), predicate: "city".into(), value: serde_json::json!("Berlin") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn make_dummy_adj_request(agent: &AgentId) -> AdjudicationRequest {
        AdjudicationRequest {
            subject_line: mempill_types::SubjectLineRef {
                agent_id: agent.clone(),
                subject: "user".into(),
                predicate: "city".into(),
            },
            incumbent: mempill_types::Belief {
                claim_ref: ClaimRef::new_random(),
                fact: Fact { subject: "user".into(), predicate: "city".into(), value: serde_json::json!("Berlin") },
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                valid_time: ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
                transaction_time: TransactionTime(Utc::now()),
                confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
                currency_signal: CurrencySignal {
                    last_refreshed_at: TransactionTime(Utc::now()),
                    state: CurrencyState::Fresh,
                    corroboration_count: 0,
                },
                criticality: Criticality::Medium,
            },
            challenger: make_claim(agent),
            criticality: Criticality::Medium,
            reason: mempill_types::OverturnReason::ExternalContradiction,
        }
    }

    /// Build a pending row with both claims at QueuedForAdjudication in the ledger.
    fn setup_queued_scenario(
        store: &MockStore,
        pending: &MockPendingStore,
        handle_id: uuid::Uuid,
    ) -> (ClaimRef, ClaimRef) {
        let agent = make_agent();
        let challenger = make_claim(&agent);
        let incumbent = make_claim(&agent);
        let now = Utc::now();

        // The challenger is QueuedForAdjudication; the incumbent is CommittedCheap.
        // (Only the challenger is gated to QueuedForAdjudication by the engine.)
        store.ledger.lock().unwrap().push(LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: challenger.claim_ref().clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::QueuedForAdjudication,
            rationale: None,
            recorded_at: TransactionTime(now - chrono::Duration::seconds(5)),
        });
        store.ledger.lock().unwrap().push(LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: incumbent.claim_ref().clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap, // incumbent stays committed
            rationale: None,
            recorded_at: TransactionTime(now - chrono::Duration::seconds(10)),
        });

        // Seed the pending row.
        pending.seed(PendingAdjudicationRow {
            handle_id,
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "city".into(),
            challenger_claim_ref: challenger.claim_ref().clone(),
            incumbent_claim_ref: incumbent.claim_ref().clone(),
            request_payload: make_dummy_adj_request(&agent),
            queued_at: now - chrono::Duration::seconds(10),
            expires_at: None,
            status: "pending".to_string(),
        });

        (challenger.claim_ref().clone(), incumbent.claim_ref().clone())
    }

    fn build_use_case(
        store: Arc<MockStore>,
        pending: Arc<MockPendingStore>,
    ) -> SubmitAdjudicationUseCase<MockStore> {
        let erased: Arc<dyn ErasedPendingStore> =
            Arc::new(ErasedPendingStoreAdapter::new({
                struct Delegate(Arc<MockPendingStore>);
                impl PendingAdjudicationPort for Delegate {
                    type Error = MockErr;
                    fn insert_pending(&self, r: &PendingAdjudicationRow) -> Result<(), MockErr> { self.0.insert_pending(r) }
                    fn get_pending(&self, h: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> { self.0.get_pending(h) }
                    fn list_pending(&self, a: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> { self.0.list_pending(a) }
                    fn list_expired(&self, n: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> { self.0.list_expired(n) }
                    fn mark_resolved(&self, h: uuid::Uuid) -> Result<(), MockErr> { self.0.mark_resolved(h) }
                    fn mark_expired(&self, h: uuid::Uuid) -> Result<(), MockErr> { self.0.mark_expired(h) }
                    fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> { self.0.list_queued_orphan_claims() }
                }
                Delegate(Arc::clone(&pending))
            }));
        SubmitAdjudicationUseCase::new(store, erased)
    }

    // ── Test: unknown handle → AdjudicationHandleNotFound ────────────────────

    #[test]
    fn unknown_handle_returns_handle_not_found() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));

        let response = AdjudicationResponse {
            handle_id: uuid::Uuid::new_v4(),
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let result = uc.execute(response.handle_id, response, Utc::now());
        assert!(matches!(result, Err(MemError::AdjudicationHandleNotFound { .. })));
    }

    // ── Test: Affirm — challenger CommittedCheap, incumbent Superseded, 2 ledger entries ──

    #[test]
    fn affirm_challenger_committed_cheap_incumbent_superseded_two_ledger_entries() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        let (challenger_ref, incumbent_ref) =
            setup_queued_scenario(&store, &pending, handle_id);

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let outcome = uc.execute(handle_id, response, Utc::now()).unwrap();

        assert_eq!(outcome.handle_id, handle_id);
        assert_eq!(outcome.disposition, Disposition::CommittedCheap);
        assert_eq!(outcome.claim_ref, challenger_ref);

        let ledger = store.ledger.lock().unwrap();
        // Filter only the entries written by this use-case (AdjudicationResolved + ValidityAsserted).
        let resolution_entries: Vec<_> = ledger.iter()
            .filter(|e| e.event_kind == LedgerEventKind::AdjudicationResolved
                || e.event_kind == LedgerEventKind::ValidityAsserted)
            .collect();
        assert_eq!(resolution_entries.len(), 2, "Affirm must write exactly 2 ledger entries");

        // Challenger entry: AdjudicationResolved + CommittedCheap.
        let challenger_entry = resolution_entries.iter()
            .find(|e| e.claim_ref == challenger_ref && e.event_kind == LedgerEventKind::AdjudicationResolved)
            .expect("challenger AdjudicationResolved entry must exist");
        assert_eq!(challenger_entry.disposition, Disposition::CommittedCheap);

        // Incumbent entry: ValidityAsserted + Superseded.
        let incumbent_entry = resolution_entries.iter()
            .find(|e| e.claim_ref == incumbent_ref)
            .expect("incumbent ValidityAsserted entry must exist");
        assert_eq!(incumbent_entry.disposition, Disposition::Superseded);

        // Validity assertion (Bound) written for incumbent.
        let assertions = store.validity_assertions.lock().unwrap();
        assert_eq!(assertions.len(), 1, "one Bound assertion for incumbent");
        assert_eq!(assertions[0].target_claim, incumbent_ref);

        // Pending row marked resolved.
        assert!(pending.is_resolved(handle_id), "pending row must be resolved");
    }

    // ── Test: Affirm — External provenance stamped on challenger ledger entry ──

    #[test]
    fn affirm_challenger_entry_has_external_provenance_in_rationale() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        setup_queued_scenario(&store, &pending, handle_id);

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let evidence = ProvenanceLabel::External(ExternalKind::ExternalFirstHand);
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: evidence.clone(),
        };
        uc.execute(handle_id, response, Utc::now()).unwrap();

        let ledger = store.ledger.lock().unwrap();
        let affirm_entry = ledger.iter()
            .find(|e| e.event_kind == LedgerEventKind::AdjudicationResolved
                && e.disposition == Disposition::CommittedCheap)
            .expect("affirm ledger entry must exist");
        let rationale = affirm_entry.rationale.as_ref().expect("rationale must be present");
        let rationale_str = rationale.to_string();
        assert!(rationale_str.contains("Affirm"), "rationale must mention Affirm verdict");
        assert!(rationale_str.contains("ExternalFirstHand"), "rationale must include evidence provenance");
    }

    // ── Test: Deny — challenger Superseded, 1 ledger entry ───────────────────

    #[test]
    fn deny_challenger_superseded_one_ledger_entry() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        let (challenger_ref, incumbent_ref) =
            setup_queued_scenario(&store, &pending, handle_id);

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Deny,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let outcome = uc.execute(handle_id, response, Utc::now()).unwrap();

        assert_eq!(outcome.disposition, Disposition::Superseded);
        assert_eq!(outcome.claim_ref, challenger_ref);

        let ledger = store.ledger.lock().unwrap();
        let resolution_entries: Vec<_> = ledger.iter()
            .filter(|e| e.event_kind == LedgerEventKind::ValidityAsserted)
            .collect();
        assert_eq!(resolution_entries.len(), 1, "Deny must write exactly 1 ValidityAsserted entry");
        assert_eq!(resolution_entries[0].claim_ref, challenger_ref);
        assert_eq!(resolution_entries[0].disposition, Disposition::Superseded);

        // Validity assertion (Bound) written for challenger.
        let assertions = store.validity_assertions.lock().unwrap();
        assert_eq!(assertions.len(), 1, "one Bound assertion for challenger");
        assert_eq!(assertions[0].target_claim, challenger_ref);

        // Incumbent has NO new entry — it remains committed.
        let incumbent_resolution = ledger.iter()
            .filter(|e| e.claim_ref == incumbent_ref
                && (e.event_kind == LedgerEventKind::AdjudicationResolved
                    || e.event_kind == LedgerEventKind::ValidityAsserted))
            .count();
        assert_eq!(incumbent_resolution, 0, "Deny must not touch the incumbent");

        assert!(pending.is_resolved(handle_id));
    }

    // ── Test: Unknown — both Contested, 2 ledger entries, no Bound assertion ──

    #[test]
    fn unknown_both_contested_two_ledger_entries_no_bound_assertion() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        let (challenger_ref, incumbent_ref) =
            setup_queued_scenario(&store, &pending, handle_id);

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Unknown,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let outcome = uc.execute(handle_id, response, Utc::now()).unwrap();

        assert_eq!(outcome.disposition, Disposition::Contested);
        assert_eq!(outcome.claim_ref, challenger_ref);

        let ledger = store.ledger.lock().unwrap();
        let abstain_entries: Vec<_> = ledger.iter()
            .filter(|e| e.event_kind == LedgerEventKind::AdjudicationResolved)
            .collect();
        assert_eq!(abstain_entries.len(), 2, "Unknown must write 2 AdjudicationResolved entries (one per claim)");

        let ch_entry = abstain_entries.iter().find(|e| e.claim_ref == challenger_ref).unwrap();
        let inc_entry = abstain_entries.iter().find(|e| e.claim_ref == incumbent_ref).unwrap();
        assert_eq!(ch_entry.disposition, Disposition::Contested);
        assert_eq!(inc_entry.disposition, Disposition::Contested);

        // No Bound assertions for Unknown.
        let assertions = store.validity_assertions.lock().unwrap();
        assert_eq!(assertions.len(), 0, "Unknown must not write any Bound assertions");

        assert!(pending.is_resolved(handle_id));
    }

    // ── Test: duplicate submit → AdjudicationHandleNotFound ──────────────────

    #[test]
    fn duplicate_submit_returns_handle_not_found() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        setup_queued_scenario(&store, &pending, handle_id);

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let mk_response = || AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Deny,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };

        // First submit succeeds.
        uc.execute(handle_id, mk_response(), Utc::now()).unwrap();

        // After first submit the state guard finds claims are no longer QueuedForAdjudication,
        // so second submit returns AdjudicationHandleNotFound.
        let result = uc.execute(handle_id, mk_response(), Utc::now());
        assert!(
            matches!(result, Err(MemError::AdjudicationHandleNotFound { .. })),
            "duplicate submit must return AdjudicationHandleNotFound"
        );
    }

    // ── Test: stale (challenger no longer QueuedForAdjudication) → HandleNotFound ─
    //
    // The state guard checks only the challenger's disposition. If the challenger has
    // already been resolved (CommittedCheap / Superseded / Contested), it's stale.
    // The incumbent's disposition is NOT checked here — it was never QueuedForAdjudication.

    #[test]
    fn stale_challenger_not_queued_returns_handle_not_found() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        let agent = make_agent();
        let challenger = make_claim(&agent);
        let incumbent = make_claim(&agent);
        let now = Utc::now();

        // Seed challenger ledger with CommittedCheap (already resolved — stale).
        store.ledger.lock().unwrap().push(LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: challenger.claim_ref().clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap, // NOT queued — already resolved
            rationale: None,
            recorded_at: TransactionTime(now),
        });
        // Incumbent stays CommittedCheap (normal state before adjudication).
        store.ledger.lock().unwrap().push(LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: incumbent.claim_ref().clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(now),
        });

        pending.seed(PendingAdjudicationRow {
            handle_id,
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "city".into(),
            challenger_claim_ref: challenger.claim_ref().clone(),
            incumbent_claim_ref: incumbent.claim_ref().clone(),
            request_payload: make_dummy_adj_request(&agent),
            queued_at: now,
            expires_at: None,
            status: "pending".to_string(),
        });

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let result = uc.execute(handle_id, response, Utc::now());
        assert!(
            matches!(result, Err(MemError::AdjudicationHandleNotFound { .. })),
            "stale challenger (not QueuedForAdjudication) must return AdjudicationHandleNotFound"
        );
    }

    // ── Test: expired handle → AdjudicationHandleNotFound ────────────────────

    #[test]
    fn expired_handle_returns_handle_not_found() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        let agent = make_agent();
        let challenger = make_claim(&agent);
        let incumbent = make_claim(&agent);
        let past = Utc::now() - chrono::Duration::hours(2);

        // Seed with expires_at in the past.
        pending.seed(PendingAdjudicationRow {
            handle_id,
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "city".into(),
            challenger_claim_ref: challenger.claim_ref().clone(),
            incumbent_claim_ref: incumbent.claim_ref().clone(),
            request_payload: make_dummy_adj_request(&agent),
            queued_at: past - chrono::Duration::hours(1),
            expires_at: Some(past), // expired
            status: "pending".to_string(),
        });

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let result = uc.execute(handle_id, response, Utc::now());
        assert!(
            matches!(result, Err(MemError::AdjudicationHandleNotFound { .. })),
            "expired handle must return AdjudicationHandleNotFound"
        );
    }

    // ── Test: atomicity — failure mid-apply → no partial state ───────────────

    #[test]
    fn atomicity_failure_mid_apply_no_partial_state() {
        let store = Arc::new(MockStore::default());
        let pending = Arc::new(MockPendingStore::default());
        let handle_id = uuid::Uuid::new_v4();
        setup_queued_scenario(&store, &pending, handle_id);

        // Make the 2nd ledger write fail (after the Bound assertion but during the first ledger entry).
        *store.fail_on_ledger_write.lock().unwrap() = Some(1);

        let uc = build_use_case(Arc::clone(&store), Arc::clone(&pending));
        let response = AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        let result = uc.execute(handle_id, response, Utc::now());
        assert!(result.is_err(), "must propagate the injected failure");

        // After failure + rollback: only the seeded QueuedForAdjudication entries remain.
        let ledger = store.ledger.lock().unwrap();
        let resolution_entries: Vec<_> = ledger.iter()
            .filter(|e| e.event_kind == LedgerEventKind::AdjudicationResolved
                || e.event_kind == LedgerEventKind::ValidityAsserted)
            .collect();
        assert_eq!(
            resolution_entries.len(), 0,
            "no resolution ledger entries must remain after mid-apply failure"
        );
        assert!(*store.rollback_called.lock().unwrap(), "rollback must be called");
    }
}
