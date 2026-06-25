//! IngestClaimUseCase — application layer write path.
//!
//! Orchestrates Gateway → AmplificationGuard → Reconciler → AdjudicationGate → (optional Supersession)
//! within a single atomic transaction: begin_atomic → appends → commit.
//! On any error: rollback is called and Err(MemError) is returned.
//!
//! "now" is injected by the EngineHandle caller (DETERMINISM convention).
//! The lock is acquired at the EngineHandle boundary before spawn_blocking.
//!
//! # Pending-adjudication persistence (W3, Amendment 1)
//!
//! When the gate routes to QueuedForAdjudication and an oracle IS present, the use-case
//! persists a `pending_adjudications` row AFTER the main claim txn commits (not inside it).
//! This keeps the claim commit and the oracle queue decoupled:
//!   - The claim row lands with disposition=QueuedForAdjudication.
//!   - The pending row is inserted via the type-erased ErasedPendingStore.
//!   - If the pending insert fails, an Err(MemError::PendingStore) is returned.
//!
//! The per-agent write lock (held by EngineHandle) remains held across both the main txn
//! commit and the pending insert, so there is no window for a race between them.

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
        truth_engine,
    },
    engine_handle::ErasedPendingStore,
    error::MemError,
    ports::{OraclePort, PendingAdjudicationRow, PersistencePort},
};

use super::dto::{IngestClaimRequest, IngestClaimResponse};

/// Use-case: ingest a new claim from any binding.
/// Generic over persistence and oracle ports; zero-cost dispatch; testable with mocks.
/// Oracle is optional: when None, the oracle-absent branch fires for heavy-path contradiction resolution.
/// The pending store is type-erased via `ErasedPendingStore` to keep the use-case free of a
/// third type parameter while still being injectable in tests.
pub struct IngestClaimUseCase<P, O>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    oracle: Option<Arc<O>>,
    /// Type-erased pending-adjudication store (W3). `None` when no oracle queue is configured.
    pending_store: Option<Arc<dyn ErasedPendingStore>>,
    config: EngineConfig,
}

impl<P, O> IngestClaimUseCase<P, O>
where
    P: PersistencePort + Send + Sync + 'static,
    O: OraclePort + Send + Sync + 'static,
{
    pub fn new(
        persistence: Arc<P>,
        oracle: Option<Arc<O>>,
        pending_store: Option<Arc<dyn ErasedPendingStore>>,
        config: EngineConfig,
    ) -> Self {
        Self { persistence, oracle, pending_store, config }
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

        // Load ledger for disposition-based filtering (DEFECT-2 fix).
        let ledger_for_fold = self.persistence
            .load_ledger(&req.agent_id, None, 10_000)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Build LATEST disposition per claim from the ledger (for DEFECT-2 fold filter).
        let latest_disposition = build_latest_disposition_map(&ledger_for_fold);

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
            &latest_disposition,
        );
        let n_live_incumbents = fold_result.live_claims.len();
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
                succession_threshold: self.config.valid_time_confidence_threshold,
                n_gt_1_live_incumbents: n_live_incumbents > 1,
            },
            &self.config,
        );

        // ── Step 4: C7 gate — adjudicate ─────────────────────────────────────────
        let decision = gate::adjudicate(&proposal, &self.config);

        // ── Step 4.5: Edge pre-load (DEFECT-1 fix — historical note) ────────────────
        // Previously, DependsOn edges were pre-loaded here for the HeavyPath supersession call
        // inside the transaction. Since ingest-time supersession on HeavyPath was the root cause
        // of the Contested-surfacing bug (TASK-9-W4-W5-FIX), that supersession call is removed.
        // No edges need to be pre-loaded at ingest time; all supersession now happens only at
        // submit_adjudication (Affirm verdict). An empty vec is kept so the append_within_txn
        // signature is unchanged (forward-compatible if a future deterministic-supersede path
        // is added to the gate).
        let preloaded_edges: Vec<ClaimEdge> = vec![];

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
            &preloaded_edges,
            &mut txn,
        );

        let response = match result {
            Ok(response) => {
                self.persistence
                    .commit(txn)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                response
            }
            Err(e) => {
                // Rollback on any error (I9 — no partial writes).
                let _ = self.persistence.rollback(txn);
                return Err(e);
            }
        };

        // ── Step 6: Persist pending-adjudication row (W3, Amendment 1) ──────────
        // Called AFTER the main claim txn commits. The per-agent write lock (held by
        // EngineHandle) ensures no race between commit and this insert.
        //
        // Conditions for a pending row:
        //   - disposition = QueuedForAdjudication (oracle IS present; gate routed B11b)
        //   - oracle port is configured (oracle.is_some())
        //   - pending store is configured (pending_store.is_some())
        //   - an incumbent belief exists (must have a claim_ref to reference)
        if response.disposition == Disposition::QueuedForAdjudication {
            if let (Some(oracle), Some(pending_store), Some(ref incumbent)) =
                (&self.oracle, &self.pending_store, &incumbent_belief)
            {
                // Build the AdjudicationRequest for oracle context.
                let adj_request = mempill_types::AdjudicationRequest {
                    subject_line: mempill_types::SubjectLineRef {
                        agent_id: req.agent_id.clone(),
                        subject: req.subject.clone(),
                        predicate: req.predicate.clone(),
                    },
                    incumbent: mempill_types::Belief {
                        claim_ref: incumbent.claim_ref.clone(),
                        fact: incumbent.fact.clone(),
                        provenance: incumbent.provenance.clone(),
                        valid_time: incumbent.valid_time.clone(),
                        transaction_time: incumbent.transaction_time.clone(),
                        confidence: incumbent.confidence.clone(),
                        currency_signal: incumbent.currency_signal.clone(),
                        criticality: incumbent.criticality.clone(),
                    },
                    challenger: claim.clone(),
                    criticality: req.criticality.clone(),
                    reason: mempill_types::OverturnReason::ExternalContradiction,
                };

                // Submit to oracle — non-blocking, returns a handle immediately.
                match oracle.request_adjudication(&req.agent_id, adj_request.clone()) {
                    Ok(handle) => {
                        let handle_id = O::handle_to_uuid(&handle);
                        // Compute expires_at from the configured TTL (W6).
                        // Per-request TTL override is deferred to a future wave; config
                        // default is the v1 mechanism (default_adjudication_ttl field).
                        let expires_at = self.config.default_adjudication_ttl
                            .map(|ttl| now + chrono::Duration::from_std(ttl)
                                .unwrap_or(chrono::Duration::seconds(0)));
                        let pending_row = PendingAdjudicationRow {
                            handle_id,
                            agent_id: req.agent_id.clone(),
                            subject: req.subject.clone(),
                            predicate: req.predicate.clone(),
                            challenger_claim_ref: response.claim_ref.clone(),
                            incumbent_claim_ref: incumbent.claim_ref.clone(),
                            request_payload: adj_request,
                            queued_at: now,
                            expires_at,
                            status: "pending".to_string(),
                        };
                        // NOTE: post-commit orphan window — the claim txn has already committed
                        // above (disposition=QueuedForAdjudication). If the process crashes between
                        // that commit and this insert_pending_erased call, a QueuedForAdjudication
                        // claim will exist with NO corresponding pending_adjudications row. Such
                        // orphans cannot be resolved by W4. The W6 sweep MUST detect them by
                        // scanning QueuedForAdjudication claims lacking a pending row and reverting
                        // them to Contested. This is a tracked risk deferred to W6.
                        pending_store.insert_pending_erased(&pending_row)
                            .map_err(|e| MemError::PendingStore { source: e })?;
                    }
                    Err(e) => {
                        return Err(MemError::OracleError { reason: e.to_string() });
                    }
                }
            }
        }

        Ok(response)
    }

    /// Convenience wrapper: stamps "now" from the caller (the EngineHandle stamps Utc::now()
    /// once at the async boundary and passes it in via execute_with_time).
    /// This shim keeps backward compat with spawn_blocking closures that capture self.
    pub fn execute(&self, req: IngestClaimRequest) -> Result<IngestClaimResponse, MemError> {
        // In tests or direct calls without a EngineHandle, stamp now here.
        self.execute_with_time(req, Utc::now())
    }

    /// All persistence writes inside the open Txn.
    ///
    /// `preloaded_edges` is kept for signature stability but is currently always empty —
    /// ingest-time supersession was removed (TASK-9-W4-W5-FIX) so no edges are needed here.
    /// If a future deterministic-supersede path is added to the gate, this parameter carries it.
    fn append_within_txn(
        &self,
        claim: &Claim,
        decision: &gate::GateDecision,
        incumbent_belief: &Option<mempill_types::Belief>,
        agent_id: &AgentId,
        tx_time: TransactionTime,
        preloaded_edges: &[ClaimEdge],
        txn: &mut P::Transaction,
    ) -> Result<IngestClaimResponse, MemError> {
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

        // C4 supersession: at ingest time, the incumbent must NEVER be superseded on the
        // HeavyPath. HeavyPath always produces Contested or QueuedForAdjudication — both
        // of which require the incumbent to remain live (either for immediate Contested
        // surfacing, or for oracle resolution later). Supersession of the incumbent only
        // happens at submit_adjudication time via an Affirm verdict (bound_claim there).
        //
        // Historically, this block ran supersession::execute unconditionally for HeavyPath,
        // which wrote a ValidityAssertion::Bound + Superseded ledger entry on the incumbent
        // at ingest time. This caused:
        //   - QUEUED: incumbent excluded from live_claims → only challenger visible
        //   - DENY: incumbent excluded → NoBelief (both bounded/Superseded)
        //   - UNKNOWN: incumbent excluded → only challenger visible
        //   - B11 (oracle absent / Contested): incumbent excluded → only challenger visible
        // The fix: do NOT call supersession::execute at ingest time for HeavyPath.
        // The incumbent remains CommittedCheap (live) until the oracle resolves.
        //
        // preloaded_edges retain their DEFECT-1 fix value: loaded before begin_atomic to
        // avoid reads inside the open txn. They are unused here now but kept in scope for
        // the pattern integrity if a future deterministic-supersede path is added.
        let _ = preloaded_edges; // DEFECT-1 preload preserved; HeavyPath never supersedes at ingest

        // Populate contested_with when the disposition signals a conflict (Contested or
        // QueuedForAdjudication). The incumbent's claim_ref is passed in from the caller
        // so that the Python (and any other) binding can surface BOTH conflicting refs.
        // An empty vec is correct for all non-conflict dispositions (CheapPath, etc.).
        let contested_with = if matches!(
            decision.disposition,
            Disposition::Contested | Disposition::QueuedForAdjudication
        ) {
            incumbent_belief
                .as_ref()
                .map(|b| vec![b.claim_ref.clone()])
                .unwrap_or_default()
        } else {
            vec![]
        };

        Ok(IngestClaimResponse {
            claim_ref,
            disposition: decision.disposition.clone(),
            contested_with,
        })
    }
}

/// Build a map of ClaimRef → latest Disposition from a ledger slice.
/// "Latest" = the entry with the maximum `recorded_at`. Used by fold to exclude
/// non-live dispositions (DEFECT-2 fix).
pub(crate) fn build_latest_disposition_map(
    ledger: &[mempill_types::LedgerEntry],
) -> std::collections::HashMap<mempill_types::ClaimRef, Disposition> {
    let mut map: std::collections::HashMap<mempill_types::ClaimRef, (mempill_types::TransactionTime, Disposition)> =
        std::collections::HashMap::new();
    for entry in ledger {
        let existing = map.get(&entry.claim_ref);
        let should_insert = existing
            .map(|(t, _)| entry.recorded_at.0 > t.0)
            .unwrap_or(true);
        if should_insert {
            map.insert(entry.claim_ref.clone(), (entry.recorded_at.clone(), entry.disposition.clone()));
        }
    }
    map.into_iter().map(|(k, (_, d))| (k, d)).collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoOpOracle;
    use crate::ports::persistence::Txn;
    use crate::ports::pending_adjudication::PendingAdjudicationRow;
    use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
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

    // ── Mock pending store ───────────────────────────────────────────────────

    use crate::ports::pending_adjudication::PendingAdjudicationPort;

    #[derive(Default)]
    struct MockPendingStore {
        rows: Mutex<Vec<PendingAdjudicationRow>>,
    }

    impl PendingAdjudicationPort for MockPendingStore {
        type Error = MockErr;

        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), MockErr> {
            self.rows.lock().unwrap().push(row.clone());
            Ok(())
        }

        fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> {
            let rows = self.rows.lock().unwrap();
            Ok(rows.iter().find(|r| r.handle_id == handle_id).cloned())
        }

        fn list_pending(&self, agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
            let rows = self.rows.lock().unwrap();
            Ok(rows.iter()
                .filter(|r| agent_id.map_or(true, |a| r.agent_id == *a) && r.status == "pending")
                .cloned()
                .collect())
        }

        fn list_expired(&self, now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
            let rows = self.rows.lock().unwrap();
            Ok(rows.iter()
                .filter(|r| {
                    r.status == "pending"
                        && r.expires_at.map_or(false, |exp| exp <= now)
                })
                .cloned()
                .collect())
        }

        fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), MockErr> {
            let mut rows = self.rows.lock().unwrap();
            for row in rows.iter_mut() {
                if row.handle_id == handle_id {
                    row.status = "resolved".to_string();
                }
            }
            Ok(())
        }

        fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), MockErr> {
            let mut rows = self.rows.lock().unwrap();
            for row in rows.iter_mut() {
                if row.handle_id == handle_id {
                    row.status = "expired".to_string();
                }
            }
            Ok(())
        }

        fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> {
            Ok(vec![])
        }
    }

    // ── TestOracle with deterministic UUID handle ─────────────────────────────

    struct TestOracle {
        fixed_uuid: uuid::Uuid,
    }

    impl crate::ports::OraclePort for TestOracle {
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

    // ── MockStore that tracks incumbent for conflict tests ────────────────────

    #[derive(Default)]
    struct MockStoreWithIncumbent {
        claims: Mutex<Vec<Claim>>,
        ledger: Mutex<Vec<LedgerEntry>>,
    }

    impl PersistencePort for MockStoreWithIncumbent {
        type Transaction = MockTxn;
        type Error = MockErr;

        fn begin_atomic(&self, agent_id: &AgentId) -> Result<MockTxn, MockErr> {
            Ok(MockTxn(agent_id.clone()))
        }

        fn append_claim(&self, _txn: &mut MockTxn, claim: &Claim) -> Result<ClaimRef, MockErr> {
            self.claims.lock().unwrap().push(claim.clone());
            Ok(claim.claim_ref().clone())
        }

        fn append_validity_assertion(&self, _: &mut MockTxn, _: &ValidityAssertion) -> Result<(), MockErr> { Ok(()) }

        fn append_ledger_entry(&self, _txn: &mut MockTxn, entry: &LedgerEntry) -> Result<(), MockErr> {
            self.ledger.lock().unwrap().push(entry.clone());
            Ok(())
        }

        fn append_claim_edge(&self, _: &mut MockTxn, _: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
        fn commit(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }

        fn load_subject_line(&self, _agent_id: &AgentId, _subject: &str, _predicate: &str) -> Result<Vec<Claim>, MockErr> {
            // Return stored claims so gate sees the incumbent.
            Ok(self.claims.lock().unwrap().clone())
        }

        fn load_claim(&self, _: &AgentId, _: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }

        fn load_ledger(&self, _: &AgentId, _: Option<&TransactionTime>, _: usize) -> Result<Vec<LedgerEntry>, MockErr> {
            Ok(self.ledger.lock().unwrap().clone())
        }

        fn load_edges_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
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
            None,
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
            None,
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
            None,
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
            None,
            EngineConfig::default(),
        );
        let now = chrono::Utc::now();
        // oracle is None → oracle_present = false. First External claim still cheap-paths (no conflict).
        let resp = uc.execute_with_time(base_request(), now).unwrap();
        assert_eq!(resp.disposition, Disposition::CommittedCheap);
    }

    // ── Test: W3 — QueuedForAdjudication persists exactly one pending row ─────

    /// When oracle IS present and a conflicting claim produces QueuedForAdjudication,
    /// exactly one pending_adjudications row must be inserted with the correct fields.
    #[test]
    fn w3_queued_for_adjudication_persists_one_pending_row() {
        use mempill_types::{ExternalAnchor, Fact, ValidTime};

        let agent = AgentId("test-agent".into());
        let fixed_uuid = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid });
        let raw_pending = Arc::new(MockPendingStore::default());
        let pending_store: Arc<dyn ErasedPendingStore> =
            Arc::new(ErasedPendingStoreAdapter::new(MockPendingStore::default()));

        // We need a shared reference to inspect rows, so build the store separately.
        let shared_pending = Arc::new(MockPendingStore::default());
        let erased_pending: Arc<dyn ErasedPendingStore> =
            Arc::new(ErasedPendingStoreAdapter::new({
                // Build a wrapper that delegates to shared_pending.
                struct SharedWrapper(Arc<MockPendingStore>);
                impl PendingAdjudicationPort for SharedWrapper {
                    type Error = MockErr;
                    fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), MockErr> {
                        self.0.insert_pending(row)
                    }
                    fn get_pending(&self, id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> {
                        self.0.get_pending(id)
                    }
                    fn list_pending(&self, a: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
                        self.0.list_pending(a)
                    }
                    fn list_expired(&self, now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> {
                        self.0.list_expired(now)
                    }
                    fn mark_resolved(&self, id: uuid::Uuid) -> Result<(), MockErr> {
                        self.0.mark_resolved(id)
                    }
                    fn mark_expired(&self, id: uuid::Uuid) -> Result<(), MockErr> {
                        self.0.mark_expired(id)
                    }
                    fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> {
                        self.0.list_queued_orphan_claims()
                    }
                }
                SharedWrapper(Arc::clone(&shared_pending))
            }));

        let _ = pending_store; // suppress unused warning
        let _ = raw_pending;

        // Seed an incumbent claim.
        let store = Arc::new(MockStoreWithIncumbent::default());
        let incumbent_claim = Claim::new(
            ClaimRef(uuid::Uuid::new_v4()),
            agent.clone(),
            Fact {
                subject: "user".into(),
                predicate: "city".into(),
                value: serde_json::json!("Berlin"),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(chrono::Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            vec![],
            None,
            None,
        );
        store.claims.lock().unwrap().push(incumbent_claim.clone());
        store.ledger.lock().unwrap().push(LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: incumbent_claim.claim_ref().clone(),
            event_kind: mempill_types::LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(chrono::Utc::now() - chrono::Duration::seconds(10)),
        });

        let uc = IngestClaimUseCase::new(
            Arc::clone(&store),
            Some(Arc::clone(&oracle)),
            Some(erased_pending),
            EngineConfig::default(),
        );

        let req = IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "city".into(),
            value: serde_json::json!("Paris"), // conflicts with "Berlin"
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };

        let now = chrono::Utc::now();
        let resp = uc.execute_with_time(req, now).unwrap();
        assert_eq!(resp.disposition, Disposition::QueuedForAdjudication,
            "conflicting External claim with oracle present must be QueuedForAdjudication");

        let rows = shared_pending.rows.lock().unwrap();
        assert_eq!(rows.len(), 1, "exactly one pending_adjudications row must be inserted");
        let row = &rows[0];
        assert_eq!(row.handle_id, fixed_uuid, "handle_id must match oracle's handle");
        assert_eq!(row.agent_id, agent);
        assert_eq!(row.subject, "user");
        assert_eq!(row.predicate, "city");
        assert_eq!(row.challenger_claim_ref, resp.claim_ref,
            "challenger_claim_ref must be the newly committed claim");
        assert_eq!(row.incumbent_claim_ref, incumbent_claim.claim_ref().clone(),
            "incumbent_claim_ref must be the pre-existing incumbent");
        assert_eq!(row.status, "pending");
        assert!(row.expires_at.is_none(), "expires_at must be NULL for W3");
    }

    // ── Test: B11a — oracle absent → Contested, NO pending row ───────────────

    /// B11a invariant: when oracle is absent, Contested fires immediately.
    /// No pending_adjudications row must be written.
    #[test]
    fn b11a_oracle_absent_contested_no_pending_row() {
        use mempill_types::{ExternalAnchor, Fact, ValidTime};

        let agent = AgentId("b11a-agent".into());
        let shared_pending = Arc::new(MockPendingStore::default());
        let erased_pending: Arc<dyn ErasedPendingStore> = {
            struct SharedWrapper(Arc<MockPendingStore>);
            impl PendingAdjudicationPort for SharedWrapper {
                type Error = MockErr;
                fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), MockErr> { self.0.insert_pending(row) }
                fn get_pending(&self, id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, MockErr> { self.0.get_pending(id) }
                fn list_pending(&self, a: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, MockErr> { self.0.list_pending(a) }
                fn list_expired(&self, now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, MockErr> { self.0.list_expired(now) }
                fn mark_resolved(&self, id: uuid::Uuid) -> Result<(), MockErr> { self.0.mark_resolved(id) }
                fn mark_expired(&self, id: uuid::Uuid) -> Result<(), MockErr> { self.0.mark_expired(id) }
                fn list_queued_orphan_claims(&self) -> Result<Vec<crate::ports::pending_adjudication::OrphanedQueuedClaim>, MockErr> { self.0.list_queued_orphan_claims() }
            }
            Arc::new(ErasedPendingStoreAdapter::new(SharedWrapper(Arc::clone(&shared_pending))))
        };

        let store = Arc::new(MockStoreWithIncumbent::default());
        let incumbent_claim = Claim::new(
            ClaimRef(uuid::Uuid::new_v4()),
            agent.clone(),
            Fact {
                subject: "user".into(),
                predicate: "city".into(),
                value: serde_json::json!("Berlin"),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(chrono::Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            vec![],
            None,
            None,
        );
        store.claims.lock().unwrap().push(incumbent_claim.clone());
        store.ledger.lock().unwrap().push(LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: incumbent_claim.claim_ref().clone(),
            event_kind: mempill_types::LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(chrono::Utc::now() - chrono::Duration::seconds(10)),
        });

        // NO oracle — oracle_present = false.
        let uc = IngestClaimUseCase::<_, NoOpOracle>::new(
            Arc::clone(&store),
            None,           // oracle absent
            Some(erased_pending),
            EngineConfig::default(),
        );

        let req = IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "user".into(),
            predicate: "city".into(),
            value: serde_json::json!("Paris"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };

        let now = chrono::Utc::now();
        let resp = uc.execute_with_time(req, now).unwrap();
        assert_eq!(resp.disposition, Disposition::Contested,
            "B11a: oracle absent + fresh external contradiction MUST be Contested");

        let rows = shared_pending.rows.lock().unwrap();
        assert_eq!(rows.len(), 0, "B11a: no pending row when Contested");
    }
}
