#![allow(missing_docs)]
//! ReconcileUseCase — contradiction detection pass over a set of subject-lines.
//!
//! Orchestrates the Reconciler and AdjudicationGate for each live claim in the requested
//! subject-lines within a single atomic transaction (ledger appends only).
//!
//! ## Supersession is NEVER written here (A6 / I7)
//!
//! `gate::adjudicate` never emits a resolved single-winner disposition on `Route::HeavyPath` —
//! every `SameLineConflict`/`CrossLineConflict` routes to `Contested` or `QueuedForAdjudication`,
//! both of which require the incumbent to remain live pending resolution. `reconcile()` therefore
//! never calls `supersession::execute`; the only path that may bind/supersede a claim is
//! `submit_adjudication`'s `Affirm` verdict (mirrors `ingest_claim.rs`'s already-documented
//! contract — "incumbent must NEVER be superseded on HeavyPath"). A prior version of this
//! use-case called `supersession::execute` unconditionally on any `HeavyPath` outcome, which
//! silently resolved `Contested`/`QueuedForAdjudication` claims with no oracle verdict —
//! removed as a correctness fix, not a behavior users should have relied on.
//!
//! Repeated `reconcile()` calls on unchanged state are idempotent (I6): the only side effect is
//! appending `AdjudicationResolved` ledger entries whose disposition is a pure function of
//! `(candidate, unnarrowed_other_live_claims, config, oracle_present)`.

use std::sync::Arc;

use chrono::Utc;
use mempill_types::{LedgerEntry, LedgerEventKind, TransactionTime};

use crate::{
    application::ingest_claim::build_latest_disposition_map,
    config::EngineConfig,
    engine::{
        gate,
        gate::Route,
        reconciler::{self, ReconcilerInput},
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
        // All subject-line claims, validity assertions, and ledger entries must be loaded
        // HERE — outside the transaction window. No edges are pre-loaded: reconcile() never
        // supersedes (see module docs), so no `load_edges_for` read is needed.

        // Per subject-line: (claim_ref, GateDecision) per live claim, decided outside the txn.
        struct SubjectLineData {
            per_claim: Vec<(mempill_types::ClaimRef, crate::engine::gate::GateDecision)>,
        }

        // Load claims for every requested subject-line FIRST, so the ledger load below can
        // be scoped to exactly the claims being reconciled (no agent-wide cap) — mirrors the
        // read path (query_memory/query_history) and avoids the silent-wrong-belief bug where
        // a disposition-changing entry outside a capped agent-wide scan caused a superseded
        // claim to be misclassified as live.
        let mut subject_line_claims: Vec<Vec<mempill_types::Claim>> = Vec::new();
        let mut all_claim_refs: Vec<mempill_types::ClaimRef> = Vec::new();
        for (subject, predicate) in &req.subject_lines {
            // WRITE path: pass None so reconcile sees the full current state.
            // A tx-time cutoff here would break conflict detection (post-cutoff claims
            // would appear absent, allowing erroneous re-ingestion of conflicting claims).
            let claims = self.persistence
                .load_subject_line(&req.agent_id, subject, predicate, None)
                .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
            all_claim_refs.extend(claims.iter().map(|c| c.claim_ref().clone()));
            subject_line_claims.push(claims);
        }

        // Load ledger for disposition filtering (excludes non-live dispositions from fold),
        // scoped to the union of claims across all requested subject-lines.
        let all_ledger = self.persistence
            .load_ledger_for_claims(&req.agent_id, &all_claim_refs, None)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let latest_disposition = build_latest_disposition_map(&all_ledger);

        let mut subject_line_data: Vec<SubjectLineData> = Vec::new();

        for claims in subject_line_claims {
            let fold = truth_engine::fold(
                claims.clone(),
                |cref| {
                    self.persistence
                        .load_validity_assertions_for(&req.agent_id, cref)
                        .unwrap_or_default()
                },
                tx_time.0,
                None, // valid_at_instant: None = use as_of_tx_time for instant-selection (future wave adds query param)
                &self.config,
                &latest_disposition,
            );

            // N-wide succession check (fixes the silent chain-overlap defect): each candidate
            // is compared against EVERY raw-live claim on this subject-line (`FoldResult::all_claims`
            // filtered `is_live`), not just a single "current" incumbent. Shared across every
            // candidate on this line; `classify_conflict` filters self out internally.
            let all_live_claims: Vec<mempill_types::Claim> = fold
                .all_claims
                .iter()
                .filter(|cs| cs.is_live)
                .map(|cs| cs.claim.clone())
                .collect();

            let mut per_claim = Vec::new();
            for cs in &fold.live_claims {
                let candidate = &cs.claim;

                // Per-candidate incumbent (DIAG_silent_succession §6(b)): NEVER feed the
                // candidate itself as `incumbent` — `classify_conflict`'s step-3 same-value
                // check (reconciler.rs:143-146) would then trivially match (identical claim),
                // returning NoConflict/CheapPath for a claim that is genuinely part of a
                // contested line. Choose the first OTHER live claim in `fold.live_claims`'
                // canonical ordering-key order (I8, deterministic/arrival-independent — same
                // order the fold itself produces) as the legacy `incumbent` field (used only
                // for step 1's None-check and step 3's same-value check; the real N-wide
                // conflict/succession classification below uses `all_live_claims`, not this
                // field). `None` only when this candidate is the sole live claim on the line
                // (must NOT be fed when len() > 1 — that would short-circuit every candidate to
                // NoConflict at reconciler.rs:117-120).
                let incumbent = fold
                    .live_claims
                    .iter()
                    .find(|other| other.claim.claim_ref() != candidate.claim_ref())
                    .map(truth_engine::claim_to_belief);

                let proposal = reconciler::reconcile(
                    ReconcilerInput {
                        candidate,
                        incumbent: incumbent.as_ref(),
                        superseded_claim_refs: &[],
                        measured_confidence: candidate.confidence().value_confidence,
                        cardinality_proposal: candidate.cardinality().clone(),
                        oracle_present,
                        succession_threshold: self.config.valid_time_confidence_threshold,
                        all_live_claims: &all_live_claims,
                    },
                    &self.config,
                );
                let decision = gate::adjudicate(&proposal, &self.config);

                per_claim.push((candidate.claim_ref().clone(), decision));
            }

            subject_line_data.push(SubjectLineData { per_claim });
        }

        // ── Now open the transaction — writes only ─────────────────────────────
        // reconcile() NEVER supersedes (see module docs): the only write is the
        // AdjudicationResolved ledger entry per candidate. `submit_adjudication`'s Affirm
        // verdict remains the sole caller of `supersession::execute`.
        let mut txn = self.persistence
            .begin_atomic(&req.agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let result = (|| {
            for sld in &subject_line_data {
                for (claim_ref, decision) in &sld.per_claim {
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

                    // Escalation count: one per candidate landing on HeavyPath (Contested OR
                    // QueuedForAdjudication) — unconditional on incumbent presence, matching
                    // existing semantics now that the incumbent-gated supersession block is gone.
                    if matches!(decision.route, Route::HeavyPath) {
                        oracle_escalations += 1;
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
        fn load_subject_line(&self, _a: &AgentId, _s: &str, _p: &str, _as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<Claim>, MockErr> { Ok(vec![]) }
        fn load_claim(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }
        fn load_ledger(&self, _a: &AgentId, _f: Option<&TransactionTime>, _l: usize) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_ledger_for_claims(&self, _a: &AgentId, _refs: &[ClaimRef], _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_edges_for(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _a: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn list_predicates_for_subject(&self, _a: &AgentId, _s: &str, _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<String>, MockErr> { Ok(vec![]) }
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

    // ── Non-empty claims: overlap, no-supersession, idempotency (DIAG_silent_succession) ──

    use std::sync::Mutex;

    #[derive(Default)]
    struct SeededStore {
        claims: Mutex<Vec<Claim>>,
        assertions: Mutex<Vec<ValidityAssertion>>,
    }

    impl PersistencePort for SeededStore {
        type Transaction = MockTxn;
        type Error = MockErr;
        fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(aid.clone())) }
        fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> { Ok(c.claim_ref().clone()) }
        fn append_validity_assertion(&self, _t: &mut MockTxn, a: &ValidityAssertion) -> Result<(), MockErr> {
            self.assertions.lock().unwrap().push(a.clone());
            Ok(())
        }
        fn append_ledger_entry(&self, _t: &mut MockTxn, _e: &LedgerEntry) -> Result<(), MockErr> { Ok(()) }
        fn append_claim_edge(&self, _t: &mut MockTxn, _e: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
        fn commit(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn load_subject_line(&self, _a: &AgentId, s: &str, p: &str, _as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<Claim>, MockErr> {
            Ok(self.claims.lock().unwrap().iter()
                .filter(|c| c.fact().subject == s && c.fact().predicate == p)
                .cloned().collect())
        }
        fn load_claim(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _a: &AgentId, r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> {
            Ok(self.assertions.lock().unwrap().iter().filter(|a| &a.target_claim == r).cloned().collect())
        }
        fn load_ledger(&self, _a: &AgentId, _f: Option<&TransactionTime>, _l: usize) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_ledger_for_claims(&self, _a: &AgentId, _refs: &[ClaimRef], _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_edges_for(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _a: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn list_predicates_for_subject(&self, _a: &AgentId, _s: &str, _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<String>, MockErr> { Ok(vec![]) }
    }

    fn overlapping_claim(subject: &str, predicate: &str, value: serde_json::Value) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("a".into()),
            mempill_types::Fact { subject: subject.into(), predicate: predicate.into(), value },
            mempill_types::Cardinality::Functional,
            mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            mempill_types::ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(chrono::Utc::now()),
            mempill_types::ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
            mempill_types::Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            mempill_types::Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    /// Two overlapping live claims, oracle absent: pass 1 must leave BOTH live (no supersession
    /// written), escalate BOTH candidates (per-candidate incumbent selection, DIAG_silent_succession
    /// §6(b) — neither candidate is ever fed itself as the incumbent, so neither trivially
    /// self-matches on value and cheap-paths), and repeated calls on unchanged state must be
    /// byte-identical (I6 idempotency) — regression for DIAG_silent_succession (ii): `reconcile()`
    /// must never call `supersession::execute`.
    #[test]
    fn reconcile_overlap_pass1_contested_no_supersession_pass2_noop() {
        let store = Arc::new(SeededStore::default());
        let claim_a = overlapping_claim("acme", "ceo", serde_json::json!("Alice"));
        let claim_b = overlapping_claim("acme", "ceo", serde_json::json!("Bob"));
        store.claims.lock().unwrap().push(claim_a);
        store.claims.lock().unwrap().push(claim_b);

        let uc = ReconcileUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpOracle>>,
            EngineConfig::default(),
        );
        let req = || ReconcileRequest {
            agent_id: AgentId("a".into()),
            subject_lines: vec![("acme".into(), "ceo".into())],
        };

        let pass1 = uc.execute(req()).unwrap();
        assert_eq!(pass1.outcomes.len(), 2, "both live claims must produce an outcome");
        assert_eq!(pass1.oracle_escalations, 2,
            "both candidates land on HeavyPath — neither is ever compared against itself as \
             incumbent, so neither can trivially same-value-match and cheap-path (DIAG §6(b))");
        // Neither disposition may be CommittedCheap or Superseded — a genuinely contested line
        // must never resolve silently, and reconcile() never writes supersession.
        for (_, disposition) in &pass1.outcomes {
            assert_ne!(*disposition, mempill_types::Disposition::CommittedCheap,
                "a claim on a genuinely contested line must never resolve to CommittedCheap");
            assert_ne!(*disposition, mempill_types::Disposition::Superseded,
                "reconcile() must NEVER supersede an incumbent (A6) — only submit_adjudication may");
        }
        // No ValidityAssertion rows written by reconcile() (supersession block deleted).
        assert!(store.assertions.lock().unwrap().is_empty(),
            "reconcile() must write zero ValidityAssertion rows (no supersession)");

        // Pass 2 on unchanged state: byte-identical outcomes and escalation count (I6).
        let pass2 = uc.execute(req()).unwrap();
        assert_eq!(pass2.oracle_escalations, pass1.oracle_escalations,
            "I6: repeated reconcile() calls on unchanged state must be idempotent");
        let mut d1: Vec<_> = pass1.outcomes.iter().map(|(_, d)| d.clone()).collect();
        let mut d2: Vec<_> = pass2.outcomes.iter().map(|(_, d)| d.clone()).collect();
        d1.sort_by_key(|d| format!("{d:?}"));
        d2.sort_by_key(|d| format!("{d:?}"));
        assert_eq!(d1, d2, "I6: disposition multiset must be byte-identical across passes");
    }
}
