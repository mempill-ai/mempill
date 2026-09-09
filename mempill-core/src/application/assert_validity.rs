#![allow(missing_docs)]
//! AssertValidityUseCase — the host-facing, oracle-free write op that bounds or reopens a
//! claim's valid-time window (SDK_CONTRACT.md §3.1 `assert_validity`, TASK-33 E2).
//!
//! This is the ONLY writer of `ValidityAssertion` outside the adjudication path
//! (`submit_adjudication.rs`). It is explicit and host-initiated: the engine never infers
//! a bound on its own (A6). `ingest_claim.rs` and `reconcile.rs` remain untouched — no
//! implicit supersession writer is added to either.
//!
//! # Gates (in order)
//!
//! 1. Provenance: `External(*)` only, else `InsufficientProvenanceForOverturn`.
//! 2. Target existence + agent-scoped lookup (`load_claim` is agent_id-filtered at the
//!    adapter layer — a cross-agent target is indistinguishable from a missing one, so
//!    this structurally enforces multi-agent isolation): `ClaimNotFound`.
//! 3. Coherence (Bound only): `at < target.valid_time.start` → `IncoherentTemporalWindow`.
//! 4. Idempotency / single-writer-per-target: same `at` on an active Bound → no-op;
//!    different `at` → `AlreadyBound` (never "later wins").
//!
//! # Transaction discipline
//!
//! All reads (target claim, existing validity assertions) happen BEFORE `begin_atomic`
//! (I9) — mirrors `submit_adjudication.rs`. The `{validity assertion + ledger entry}` unit
//! commits atomically or not at all.
//!
//! # `end_fact` resolution (`resolve_live_claim_for_line`)
//!
//! A second, read-only entry point in this module resolves a (subject, predicate) line to
//! the single claim `end_fact` should bound, by reusing the SAME canonical fold
//! (`truth_engine::fold`) that `query_memory`/`recall` use — never a heuristic
//! re-derivation (I8). 0 live → `Empty`, 1 live → `Single`, >1 live → `Ambiguous(n)`;
//! `end_fact` never guesses.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use mempill_types::{
    AgentId, AssertionKind, ClaimRef, Disposition, LedgerEntry, LedgerEventKind, TransactionTime,
    ValidityAssertion,
};

use crate::{
    application::{dto::LiveClaimResolution, ingest_claim::build_latest_disposition_map},
    config::EngineConfig,
    engine::truth_engine,
    error::MemError,
    ports::PersistencePort,
};

use super::dto::{AssertValidityRequest, AssertValidityResponse, ValidityAssertionInput};

/// Sync use-case: bound or reopen a claim's valid-time window.
///
/// Invoked via `spawn_blocking` by `EngineHandle::assert_validity`.
pub struct AssertValidityUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
}

impl<P> AssertValidityUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>) -> Self {
        Self { persistence }
    }

    /// Execute the bound/reopen algorithm. `now` is engine-stamped at the async boundary
    /// and passed in (DETERMINISM).
    pub fn execute(
        &self,
        req: AssertValidityRequest,
        now: DateTime<Utc>,
    ) -> Result<AssertValidityResponse, MemError> {
        let tx_time = TransactionTime(now);

        // ── Gate 1: provenance — External(*) only ───────────────────────────────
        if !req.provenance.is_cheap_path_eligible() {
            return Err(MemError::InsufficientProvenanceForOverturn {
                provenance: req.provenance.clone(),
            });
        }

        // ── Gate 2: target exists AND belongs to agent_id (reads BEFORE begin_atomic) ──
        let target_claim = self
            .persistence
            .load_claim(&req.agent_id, &req.target)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?
            .ok_or_else(|| MemError::ClaimNotFound { claim_ref: req.target.clone() })?;

        let existing_assertions = self
            .persistence
            .load_validity_assertions_for(&req.agent_id, &req.target)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let active_bound = latest_active_bound(&existing_assertions);

        match &req.assertion {
            ValidityAssertionInput::Bound { at } => {
                // ── Gate 3: coherence ─────────────────────────────────────────
                if let Some(start) = target_claim.valid_time().start {
                    if *at < start {
                        return Err(MemError::IncoherentTemporalWindow {
                            start: start.to_rfc3339(),
                            end: at.to_rfc3339(),
                        });
                    }
                }

                // ── Gate 4: idempotency / single-writer-per-target ─────────────
                if let Some((existing_at, existing_ref)) = active_bound {
                    if existing_at == *at {
                        // No-op: identical bound already active. No new write (I6).
                        return Ok(AssertValidityResponse {
                            claim_ref: req.target.clone(),
                            assertion_ref: Some(existing_ref),
                            kind: AssertionKind::Bound { bound_at: existing_at },
                            effective_at: Some(existing_at),
                            disposition: Disposition::Superseded,
                            no_op: true,
                        });
                    }
                    return Err(MemError::AlreadyBound {
                        target: req.target.clone(),
                        existing_bound_at: existing_at.to_rfc3339(),
                    });
                }

                // ── Writes: one atomic unit (I9) ────────────────────────────────
                let assertion_ref = uuid::Uuid::new_v4();
                let assertion = ValidityAssertion {
                    assertion_ref,
                    agent_id: req.agent_id.clone(),
                    target_claim: req.target.clone(),
                    kind: AssertionKind::Bound { bound_at: *at },
                    provenance: req.provenance.clone(),
                    confidence: req.confidence.clone(),
                    asserted_at: tx_time.clone(),
                };

                let mut txn = self
                    .persistence
                    .begin_atomic(&req.agent_id)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                let result = self.write_validity_and_ledger(
                    &mut txn,
                    &req.agent_id,
                    &req.target,
                    &assertion,
                    Disposition::Superseded,
                    LedgerEventKind::ValidityAsserted,
                    tx_time.clone(),
                    serde_json::json!({
                        "event": "assert_validity_bound",
                        "bound_at": at.to_rfc3339(),
                    }),
                );

                match result {
                    Ok(()) => {
                        self.persistence
                            .commit(txn)
                            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                        Ok(AssertValidityResponse {
                            claim_ref: req.target.clone(),
                            assertion_ref: Some(assertion_ref),
                            kind: assertion.kind.clone(),
                            effective_at: Some(*at),
                            disposition: Disposition::Superseded,
                            no_op: false,
                        })
                    }
                    Err(e) => {
                        let _ = self.persistence.rollback(txn);
                        Err(e)
                    }
                }
            }

            ValidityAssertionInput::Reopen => {
                let Some((_existing_at, _existing_ref)) = active_bound else {
                    // Nothing to reopen — no-op (non-terminal; not an error).
                    return Ok(AssertValidityResponse {
                        claim_ref: req.target.clone(),
                        assertion_ref: None,
                        kind: AssertionKind::Reopen { reopen_at: now },
                        effective_at: None,
                        disposition: Disposition::Reinstated,
                        no_op: true,
                    });
                };

                let assertion_ref = uuid::Uuid::new_v4();
                let assertion = ValidityAssertion {
                    assertion_ref,
                    agent_id: req.agent_id.clone(),
                    target_claim: req.target.clone(),
                    kind: AssertionKind::Reopen { reopen_at: now },
                    provenance: req.provenance.clone(),
                    confidence: req.confidence.clone(),
                    asserted_at: tx_time.clone(),
                };

                let mut txn = self
                    .persistence
                    .begin_atomic(&req.agent_id)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                let result = self.write_validity_and_ledger(
                    &mut txn,
                    &req.agent_id,
                    &req.target,
                    &assertion,
                    Disposition::Reinstated,
                    LedgerEventKind::ValidityAsserted,
                    tx_time.clone(),
                    serde_json::json!({
                        "event": "assert_validity_reopen",
                        "reopen_at": now.to_rfc3339(),
                    }),
                );

                match result {
                    Ok(()) => {
                        self.persistence
                            .commit(txn)
                            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                        Ok(AssertValidityResponse {
                            claim_ref: req.target.clone(),
                            assertion_ref: Some(assertion_ref),
                            kind: assertion.kind.clone(),
                            effective_at: Some(now),
                            disposition: Disposition::Reinstated,
                            no_op: false,
                        })
                    }
                    Err(e) => {
                        let _ = self.persistence.rollback(txn);
                        Err(e)
                    }
                }
            }
        }
    }

    /// Append the `ValidityAssertion` + its `LedgerEntry` inside the already-open txn.
    #[allow(clippy::too_many_arguments)]
    fn write_validity_and_ledger(
        &self,
        txn: &mut P::Transaction,
        agent_id: &AgentId,
        target_ref: &ClaimRef,
        assertion: &ValidityAssertion,
        disposition: Disposition,
        event_kind: LedgerEventKind,
        tx_time: TransactionTime,
        rationale: serde_json::Value,
    ) -> Result<(), MemError> {
        self.persistence
            .append_validity_assertion(txn, assertion)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let ledger_entry = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: target_ref.clone(),
            event_kind,
            disposition,
            rationale: Some(rationale),
            recorded_at: tx_time,
        };
        self.persistence
            .append_ledger_entry(txn, &ledger_entry)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        Ok(())
    }
}

/// Walk a claim's validity assertions in `asserted_at` order and return the active Bound's
/// `(bound_at, assertion_ref)` if the claim currently carries one (no later Reopen),
/// or `None` if the claim is currently open (never bounded, or last bounded then reopened).
///
/// Mirrors `truth_engine::is_claim_live`'s toggle logic, but tracks the `(bound_at, ref)`
/// pair instead of a boolean — this use-case needs the value to check idempotency /
/// single-writer-per-target, not just liveness.
fn latest_active_bound(assertions: &[ValidityAssertion]) -> Option<(DateTime<Utc>, uuid::Uuid)> {
    let mut sorted: Vec<&ValidityAssertion> = assertions.iter().collect();
    sorted.sort_by(|a, b| {
        a.asserted_at
            .0
            .cmp(&b.asserted_at.0)
            .then(a.assertion_ref.cmp(&b.assertion_ref))
    });

    let mut state: Option<(DateTime<Utc>, uuid::Uuid)> = None;
    for assertion in sorted {
        match &assertion.kind {
            AssertionKind::Bound { bound_at } => {
                state = Some((*bound_at, assertion.assertion_ref));
            }
            AssertionKind::Reopen { .. } => {
                state = None;
            }
            // AssertionKind is #[non_exhaustive] — future kinds are ignored (conservative no-op).
            _ => {}
        }
    }
    state
}

/// Resolve a (subject, predicate) subject-line to the single live claim `end_fact` should
/// bound, using the SAME canonical fold `query_memory`/`recall` use (I8 single source of
/// truth — never a heuristic re-derivation of "how many claims are live").
///
/// Read-only; no txn opened. `now` is engine-stamped at the async boundary (DETERMINISM).
pub fn resolve_live_claim_for_line<P>(
    persistence: &Arc<P>,
    config: &EngineConfig,
    agent_id: &AgentId,
    subject: &str,
    predicate: &str,
    now: DateTime<Utc>,
) -> Result<LiveClaimResolution, MemError>
where
    P: PersistencePort + Send + Sync + 'static,
{
    let claims = persistence
        .load_subject_line(agent_id, subject, predicate, Some(now))
        .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

    let claim_refs: Vec<_> = claims.iter().map(|c| c.claim_ref().clone()).collect();
    let ledger = persistence
        .load_ledger_for_claims(agent_id, &claim_refs, Some(now))
        .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
    let latest_disposition = build_latest_disposition_map(&ledger);

    let fold = truth_engine::fold(
        claims,
        |cref| {
            persistence
                .load_validity_assertions_for(agent_id, cref)
                .unwrap_or_default()
        },
        now,
        None, // valid_at_instant: None = narrow against `now`, matching recall's default
        config,
        &latest_disposition,
    );

    Ok(match fold.live_claims.len() {
        0 => LiveClaimResolution::Empty,
        1 => LiveClaimResolution::Single(fold.live_claims[0].claim.claim_ref().clone()),
        n => LiveClaimResolution::Ambiguous(n),
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::dto::{AssertValidityRequest, ValidityAssertionInput};
    use crate::ports::{PersistencePort, Txn as TxnTrait};
    use chrono::TimeZone;
    use mempill_types::{
        Cardinality, Claim, ClaimEdge, Confidence, Criticality, ExternalAnchor, ExternalKind,
        Fact, LedgerEntry, ProvenanceLabel, ValidTime,
    };
    use std::sync::Mutex;

    struct MockTxn(AgentId);
    impl TxnTrait for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock error")]
    struct MockErr;

    #[derive(Default)]
    struct MockStore {
        claims: Mutex<Vec<Claim>>,
        ledger: Mutex<Vec<LedgerEntry>>,
        validity_assertions: Mutex<Vec<ValidityAssertion>>,
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
        fn append_validity_assertion(&self, _: &mut MockTxn, a: &ValidityAssertion) -> Result<(), MockErr> {
            self.validity_assertions.lock().unwrap().push(a.clone());
            Ok(())
        }
        fn append_ledger_entry(&self, _: &mut MockTxn, e: &LedgerEntry) -> Result<(), MockErr> {
            let mut count = self.ledger_write_count.lock().unwrap();
            *count += 1;
            let fail_on = *self.fail_on_ledger_write.lock().unwrap();
            if fail_on == Some(*count) {
                return Err(MockErr);
            }
            self.ledger.lock().unwrap().push(e.clone());
            Ok(())
        }
        fn append_claim_edge(&self, _: &mut MockTxn, _: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
        fn commit(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _: MockTxn) -> Result<(), MockErr> {
            *self.rollback_called.lock().unwrap() = true;
            Ok(())
        }
        fn load_subject_line(&self, _: &AgentId, _: &str, _: &str, _: Option<DateTime<Utc>>) -> Result<Vec<Claim>, MockErr> {
            Ok(self.claims.lock().unwrap().clone())
        }
        fn load_claim(&self, agent_id: &AgentId, r: &ClaimRef) -> Result<Option<Claim>, MockErr> {
            Ok(self.claims.lock().unwrap().iter().find(|c| c.claim_ref() == r && c.agent_id() == agent_id).cloned())
        }
        fn load_validity_assertions_for(&self, _: &AgentId, r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> {
            Ok(self.validity_assertions.lock().unwrap().iter().filter(|a| &a.target_claim == r).cloned().collect())
        }
        fn load_ledger(&self, _: &AgentId, _: Option<&TransactionTime>, _: usize) -> Result<Vec<LedgerEntry>, MockErr> {
            Ok(self.ledger.lock().unwrap().clone())
        }
        fn load_ledger_for_claims(&self, _: &AgentId, refs: &[ClaimRef], _: Option<DateTime<Utc>>) -> Result<Vec<LedgerEntry>, MockErr> {
            Ok(self.ledger.lock().unwrap().iter().filter(|e| refs.contains(&e.claim_ref)).cloned().collect())
        }
        fn load_edges_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn list_predicates_for_subject(&self, _: &AgentId, _: &str, _: Option<DateTime<Utc>>) -> Result<Vec<String>, MockErr> { Ok(vec![]) }
    }

    fn agent() -> AgentId { AgentId("mock-agent".into()) }

    fn make_open_claim(agent_id: &AgentId, start: DateTime<Utc>) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent_id.clone(),
            Fact { subject: "s".into(), predicate: "p".into(), value: serde_json::json!("v") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(start),
            ValidTime { start: Some(start), end: None, valid_time_confidence: 0.9, start_granularity: None, end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn bound_req(agent_id: &AgentId, target: &ClaimRef, at: DateTime<Utc>) -> AssertValidityRequest {
        AssertValidityRequest {
            agent_id: agent_id.clone(),
            target: target.clone(),
            assertion: ValidityAssertionInput::Bound { at },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
        }
    }

    #[test]
    fn bound_missing_claim_returns_claim_not_found() {
        let store = Arc::new(MockStore::default());
        let uc = AssertValidityUseCase::new(Arc::clone(&store));
        let result = uc.execute(bound_req(&agent(), &ClaimRef::new_random(), Utc::now()), Utc::now());
        assert!(matches!(result, Err(MemError::ClaimNotFound { .. })));
    }

    #[test]
    fn bound_writes_one_assertion_and_one_ledger_entry_atomically() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let claim = make_open_claim(&agent, start);
        let claim_ref = claim.claim_ref().clone();
        store.claims.lock().unwrap().push(claim);

        let uc = AssertValidityUseCase::new(Arc::clone(&store));
        let at = Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap();
        let resp = uc.execute(bound_req(&agent, &claim_ref, at), at + chrono::Duration::seconds(1)).unwrap();

        assert_eq!(resp.disposition, Disposition::Superseded);
        assert!(!resp.no_op);
        assert_eq!(resp.effective_at, Some(at));
        assert_eq!(store.validity_assertions.lock().unwrap().len(), 1);
        assert_eq!(store.ledger.lock().unwrap().len(), 1);
    }

    #[test]
    fn atomicity_failure_mid_apply_rolls_back_no_partial_state() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let claim = make_open_claim(&agent, start);
        let claim_ref = claim.claim_ref().clone();
        store.claims.lock().unwrap().push(claim);
        *store.fail_on_ledger_write.lock().unwrap() = Some(1);

        let uc = AssertValidityUseCase::new(Arc::clone(&store));
        let at = Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap();
        let result = uc.execute(bound_req(&agent, &claim_ref, at), at + chrono::Duration::seconds(1));

        assert!(result.is_err());
        assert_eq!(store.ledger.lock().unwrap().len(), 0, "no ledger entry must remain after mid-apply failure");
        // The validity assertion IS appended before the failing ledger write, but rollback
        // must have been called — the mock store doesn't undo appends (unlike a real txn),
        // so this asserts the CALL happened, matching submit_adjudication.rs's convention.
        assert!(*store.rollback_called.lock().unwrap(), "rollback must be called on mid-apply failure");
    }

    #[test]
    fn reopen_with_no_active_bound_is_noop() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let start = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let claim = make_open_claim(&agent, start);
        let claim_ref = claim.claim_ref().clone();
        store.claims.lock().unwrap().push(claim);

        let uc = AssertValidityUseCase::new(Arc::clone(&store));
        let resp = uc.execute(
            AssertValidityRequest {
                agent_id: agent, target: claim_ref,
                assertion: ValidityAssertionInput::Reopen,
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            },
            Utc::now(),
        ).unwrap();
        assert!(resp.no_op);
        assert_eq!(store.validity_assertions.lock().unwrap().len(), 0);
    }

    #[test]
    fn resolve_live_claim_for_line_empty_returns_empty() {
        let store = Arc::new(MockStore::default());
        let config = EngineConfig::default();
        let resolution = resolve_live_claim_for_line(&store, &config, &agent(), "s", "p", Utc::now()).unwrap();
        assert_eq!(resolution, crate::application::dto::LiveClaimResolution::Empty);
    }
}
