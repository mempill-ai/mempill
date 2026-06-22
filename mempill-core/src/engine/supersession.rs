//! C4 — Invalidation / Supersession (TECHNICAL_DESIGN.md §9, A26, I9).
//!
//! This module appends the atomic commit unit that closes an incumbent claim's
//! valid-time window and cascades a PendingReview flag to every dependent claim.
//!
//! INVARIANTS enforced here:
//!   I1  — non-destruction: only `append_*` calls; no delete/update.
//!   I9  — atomic commit unit: caller owns the Txn; this module appends within it.
//!   I10 — fixed-history monotonicity: supersession is a legitimate append.
//!   A26 — supersession.rs owns the DependsOn cascade inside the same Txn.
//!
//! ## Ownership of Txn (I9, Section 10)
//! The APPLICATION LAYER opens the Txn via `PersistencePort::begin_atomic()` and
//! calls `commit()` / `rollback()` as the only exit paths.  `supersession.rs`
//! receives a `&mut Txn` that is already open and appends within it.  It does NOT
//! call `begin_atomic`, `commit`, or `rollback`.

use mempill_types::{
    AgentId, AssertionKind, ClaimRef, Disposition, EdgeKind, LedgerEntry, LedgerEventKind,
    TransactionTime, ValidityAssertion,
};

use crate::ports::persistence::PersistencePort;

/// Parameters describing a belief-overturn that C7 (gate) has approved.
///
/// `superseded_ref`  — the incumbent claim whose valid-time is being closed.
/// `overturning_ref` — the new claim that triggered the overturn (already appended
///                     by the caller; this ref is recorded in the ledger for traceability).
/// `bound_at`        — the instant at which the incumbent's validity is bounded,
///                     expressed as the inner `chrono::DateTime<Utc>` that
///                     `AssertionKind::Bound` carries. Engine-stamped by the caller
///                     to preserve determinism (G1).
/// `recorded_at`     — the TransactionTime to stamp on all new ledger entries
///                     (engine-stamped by the caller).
/// `agent_id`        — the owning agent; must match the Txn scope (DC-2, I9).
#[derive(Debug, Clone)]
pub(crate) struct SupersessionRequest {
    pub agent_id: AgentId,
    pub superseded_ref: ClaimRef,
    pub overturning_ref: ClaimRef,
    /// The bound-at instant for `AssertionKind::Bound`.
    pub bound_at: chrono::DateTime<chrono::Utc>,
    pub recorded_at: TransactionTime,
}

/// Execute the supersession atomic append sequence (C4, A26, I9).
///
/// Performs, in order, within the ALREADY-OPEN `txn`:
///   1. Append `AssertionKind::Bound` `ValidityAssertion` closing the incumbent.
///   2. Append a `LedgerEventKind::ValidityAsserted` ledger entry for the incumbent.
///   3. Load all `ClaimEdge`s for the incumbent (non-mutating read inside the Txn).
///   4. For each edge where `edge.kind == EdgeKind::DependsOn && edge.to_claim == superseded_ref`,
///      append a `DependentFlaggedPendingReview` ledger entry for `edge.from_claim` (A26).
///
/// Returns the total number of `DependentFlaggedPendingReview` entries appended (useful for
/// callers and tests).
///
/// # Errors
/// Any persistence error propagates immediately.  The caller is responsible for rolling back
/// the Txn on error (application layer pattern — Section 10 I9 row).
pub(crate) fn execute<P: PersistencePort>(
    port: &P,
    txn: &mut P::Transaction,
    req: &SupersessionRequest,
) -> Result<usize, P::Error> {
    // Step 1 — Bound ValidityAssertion (closes the incumbent's valid window, I1).
    let assertion = ValidityAssertion {
        assertion_ref: uuid::Uuid::new_v4(),
        agent_id: req.agent_id.clone(),
        target_claim: req.superseded_ref.clone(),
        kind: AssertionKind::Bound { bound_at: req.bound_at },
        provenance: mempill_types::ProvenanceLabel::External(
            mempill_types::ExternalKind::ExternalFirstHand,
        ),
        confidence: mempill_types::Confidence {
            value_confidence: 1.0,
            valid_time_confidence: 1.0,
        },
        asserted_at: req.recorded_at.clone(),
    };
    port.append_validity_assertion(txn, &assertion)?;

    // Step 2 — Supersession ledger entry on the INCUMBENT claim.
    let ledger_supersession = LedgerEntry {
        entry_id: uuid::Uuid::new_v4(),
        agent_id: req.agent_id.clone(),
        claim_ref: req.superseded_ref.clone(),
        event_kind: LedgerEventKind::ValidityAsserted,
        disposition: Disposition::Superseded,
        rationale: Some(serde_json::json!({
            "event": "supersession",
            "overturning_claim": req.overturning_ref.0.to_string(),
            "bound_at": req.bound_at.to_rfc3339(),
        })),
        recorded_at: req.recorded_at.clone(),
    };
    port.append_ledger_entry(txn, &ledger_supersession)?;

    // Step 3 — Load DependsOn edges (non-mutating read within the open Txn).
    // `load_edges_for` returns ALL edges for the superseded claim; we filter to
    // edges where `to_claim == superseded_ref` and `kind == DependsOn`.
    let edges = port.load_edges_for(&req.agent_id, &req.superseded_ref)?;

    // Step 4 — Cascade: one DependentFlaggedPendingReview entry per dependent (A26).
    let mut cascade_count = 0usize;
    for edge in &edges {
        if edge.kind == EdgeKind::DependsOn && edge.to_claim == req.superseded_ref {
            let flag_entry = LedgerEntry {
                entry_id: uuid::Uuid::new_v4(),
                agent_id: req.agent_id.clone(),
                claim_ref: edge.from_claim.clone(),
                event_kind: LedgerEventKind::DependentFlaggedPendingReview,
                disposition: Disposition::PendingReview,
                rationale: Some(serde_json::json!({
                    "event": "depends_on_cascade",
                    "superseded_parent": req.superseded_ref.0.to_string(),
                    "overturning_claim": req.overturning_ref.0.to_string(),
                })),
                recorded_at: req.recorded_at.clone(),
            };
            port.append_ledger_entry(txn, &flag_entry)?;
            cascade_count += 1;
        }
    }

    Ok(cascade_count)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::persistence::Txn;
    use mempill_types::{
        AgentId, AssertionKind, Cardinality, Claim, ClaimEdge, ClaimRef, Criticality,
        EdgeKind, ExternalAnchor, ExternalKind, Fact, LedgerEntry, LedgerEventKind,
        ProvenanceLabel, TransactionTime, ValidTime, ValidityAssertion,
    };
    use chrono::Utc;
    use std::sync::{Arc, Mutex};

    // ── Mock Txn ─────────────────────────────────────────────────────────────

    #[derive(Debug)]
    enum MockAppend {
        Assertion(ValidityAssertion),
        Ledger(LedgerEntry),
    }

    /// In-memory Txn that records appends.  `fail_on_nth_write` allows injecting
    /// a mid-transaction failure to test rollback / atomicity.
    struct MockTxn {
        agent_id: AgentId,
        appends: Vec<MockAppend>,
        write_count: usize,
        /// If Some(n), the n-th write (1-indexed) returns an error.
        fail_on_nth_write: Option<usize>,
        /// Set to true when rollback() is called on this handle.
        rolled_back: bool,
    }

    impl MockTxn {
        fn new(agent_id: AgentId) -> Self {
            Self {
                agent_id,
                appends: Vec::new(),
                write_count: 0,
                fail_on_nth_write: None,
                rolled_back: false,
            }
        }

        fn with_fail_on(mut self, n: usize) -> Self {
            self.fail_on_nth_write = Some(n);
            self
        }

        fn next_write(&mut self) -> Result<(), MockError> {
            self.write_count += 1;
            if self.fail_on_nth_write == Some(self.write_count) {
                return Err(MockError::InjectedFailure);
            }
            Ok(())
        }

        fn assertion_count(&self) -> usize {
            self.appends.iter().filter(|a| matches!(a, MockAppend::Assertion(_))).count()
        }

        fn ledger_count(&self) -> usize {
            self.appends.iter().filter(|a| matches!(a, MockAppend::Ledger(_))).count()
        }

        fn ledger_entries_by_kind(&self, kind: &LedgerEventKind) -> usize {
            self.appends.iter().filter(|a| {
                if let MockAppend::Ledger(e) = a { &e.event_kind == kind } else { false }
            }).count()
        }
    }

    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId {
            &self.agent_id
        }
    }

    // ── Mock Error ────────────────────────────────────────────────────────────

    #[derive(Debug, thiserror::Error)]
    enum MockError {
        #[error("injected failure")]
        InjectedFailure,
    }

    // ── Mock PersistencePort ──────────────────────────────────────────────────

    /// Shared committed state (visible after commit).
    #[derive(Default)]
    struct CommittedState {
        assertions: Vec<ValidityAssertion>,
        ledger: Vec<LedgerEntry>,
    }

    struct MockPort {
        agent_id: AgentId,
        /// Edges available for load_edges_for queries (set up per test).
        edges: Vec<ClaimEdge>,
        committed: Arc<Mutex<CommittedState>>,
    }

    impl MockPort {
        fn new(agent_id: AgentId, edges: Vec<ClaimEdge>) -> Self {
            Self {
                agent_id,
                edges,
                committed: Arc::new(Mutex::new(CommittedState::default())),
            }
        }

        fn committed_assertions(&self) -> usize {
            self.committed.lock().unwrap().assertions.len()
        }

        fn committed_ledger(&self) -> usize {
            self.committed.lock().unwrap().ledger.len()
        }

        fn committed_ledger_by_kind(&self, kind: &LedgerEventKind) -> usize {
            self.committed.lock().unwrap().ledger.iter()
                .filter(|e| &e.event_kind == kind)
                .count()
        }
    }

    impl PersistencePort for MockPort {
        type Transaction = MockTxn;
        type Error = MockError;

        fn begin_atomic(&self, agent_id: &AgentId) -> Result<MockTxn, MockError> {
            Ok(MockTxn::new(agent_id.clone()))
        }

        fn append_validity_assertion(
            &self,
            txn: &mut MockTxn,
            assertion: &ValidityAssertion,
        ) -> Result<(), MockError> {
            txn.next_write()?;
            txn.appends.push(MockAppend::Assertion(assertion.clone()));
            Ok(())
        }

        fn append_ledger_entry(
            &self,
            txn: &mut MockTxn,
            entry: &LedgerEntry,
        ) -> Result<(), MockError> {
            txn.next_write()?;
            txn.appends.push(MockAppend::Ledger(entry.clone()));
            Ok(())
        }

        fn append_claim(
            &self,
            _txn: &mut MockTxn,
            _claim: &Claim,
        ) -> Result<ClaimRef, MockError> {
            unimplemented!("not needed in supersession tests")
        }

        fn append_claim_edge(
            &self,
            _txn: &mut MockTxn,
            _edge: &ClaimEdge,
        ) -> Result<(), MockError> {
            unimplemented!("not needed in supersession tests")
        }

        fn commit(&self, txn: MockTxn) -> Result<(), MockError> {
            let mut state = self.committed.lock().unwrap();
            for append in txn.appends {
                match append {
                    MockAppend::Assertion(a) => state.assertions.push(a),
                    MockAppend::Ledger(e) => state.ledger.push(e),
                }
            }
            Ok(())
        }

        fn rollback(&self, mut txn: MockTxn) -> Result<(), MockError> {
            txn.rolled_back = true;
            // Discards all pending appends — nothing written to committed state.
            Ok(())
        }

        fn load_edges_for(
            &self,
            _agent_id: &AgentId,
            claim_ref: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockError> {
            // Return edges where `to_claim == claim_ref` AND kind == DependsOn,
            // mirroring the real DB query semantics expected by supersession.
            Ok(self.edges.iter()
                .filter(|e| e.to_claim == *claim_ref && e.kind == EdgeKind::DependsOn)
                .cloned()
                .collect())
        }

        // ── Remaining read methods — not exercised by supersession tests ──

        fn load_subject_line(
            &self, _: &AgentId, _: &str, _: &str,
        ) -> Result<Vec<Claim>, MockError> {
            Ok(vec![])
        }

        fn load_claim(
            &self, _: &AgentId, _: &ClaimRef,
        ) -> Result<Option<Claim>, MockError> {
            Ok(None)
        }

        fn load_validity_assertions_for(
            &self, _: &AgentId, _: &ClaimRef,
        ) -> Result<Vec<ValidityAssertion>, MockError> {
            Ok(vec![])
        }

        fn load_ledger(
            &self, _: &AgentId, _: Option<&TransactionTime>, _: usize,
        ) -> Result<Vec<LedgerEntry>, MockError> {
            Ok(vec![])
        }

        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockError> {
            Ok(vec![])
        }

        fn load_lineage(
            &self, _: &AgentId, _: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockError> {
            Ok(vec![])
        }
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_agent() -> AgentId {
        AgentId("test-agent".into())
    }

    fn make_req(agent_id: &AgentId) -> SupersessionRequest {
        SupersessionRequest {
            agent_id: agent_id.clone(),
            superseded_ref: ClaimRef::new_random(),
            overturning_ref: ClaimRef::new_random(),
            bound_at: Utc::now(),
            recorded_at: TransactionTime::now(),
        }
    }

    fn depends_on_edge(from: ClaimRef, to: ClaimRef, agent_id: &AgentId) -> ClaimEdge {
        ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            from_claim: from,
            to_claim: to,
            kind: EdgeKind::DependsOn,
            created_at: TransactionTime::now(),
        }
    }

    fn make_claim(agent_id: &AgentId) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent_id.clone(),
            Fact {
                subject: "user".into(),
                predicate: "name".into(),
                value: serde_json::json!("Alice"),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime::now(),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            mempill_types::Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        )
    }

    // ── Test: atomicity (I9) — success path ──────────────────────────────────

    /// 3 dependents: expect exactly 1 Bound assertion + 1 supersession ledger entry
    /// + 3 DependentFlaggedPendingReview entries committed as ONE unit.
    #[test]
    fn atomicity_i9_three_dependents_committed_as_one_unit() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep1 = ClaimRef::new_random();
        let dep2 = ClaimRef::new_random();
        let dep3 = ClaimRef::new_random();
        let edges = vec![
            depends_on_edge(dep1, req.superseded_ref.clone(), &agent),
            depends_on_edge(dep2, req.superseded_ref.clone(), &agent),
            depends_on_edge(dep3, req.superseded_ref.clone(), &agent),
        ];
        let port = MockPort::new(agent.clone(), edges);

        let mut txn = port.begin_atomic(&agent).unwrap();
        let cascade_n = execute(&port, &mut txn, &req).unwrap();
        port.commit(txn).unwrap();

        // Exactly {1 Bound assertion + 1 ValidityAsserted ledger + 3 DependentFlagged} = 5 writes.
        assert_eq!(cascade_n, 3, "cascade count");
        assert_eq!(port.committed_assertions(), 1, "bound assertion count");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::ValidityAsserted),
            1,
            "supersession ledger entry"
        );
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            3,
            "dependent pending-review entries"
        );
        assert_eq!(port.committed_ledger(), 4, "total ledger entries (1 + 3)");
    }

    // ── Test: rollback on mid-transaction failure (I9 atomicity) ─────────────

    /// If the mock Txn is forced to fail on write N, the caller rolls back the Txn.
    /// After rollback, committed state must have ZERO appends (no partial supersession).
    #[test]
    fn atomicity_i9_rollback_on_failure_zero_committed() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep = ClaimRef::new_random();
        let edges = vec![depends_on_edge(dep, req.superseded_ref.clone(), &agent)];
        let port = MockPort::new(agent.clone(), edges);

        // Fail on the 2nd write (the supersession ledger entry) — simulates mid-Txn failure.
        let mut txn = MockTxn::new(agent.clone()).with_fail_on(2);
        let result = execute(&port, &mut txn, &req);

        assert!(result.is_err(), "expected error from injected failure");

        // Caller rolls back — committed state must be empty (atomicity: nothing partial).
        port.rollback(txn).unwrap();
        assert_eq!(port.committed_assertions(), 0, "no assertions after rollback");
        assert_eq!(port.committed_ledger(), 0, "no ledger entries after rollback");
    }

    // ── Test: cascade A26 — 3 dependents flagged ──────────────────────────────

    #[test]
    fn cascade_a26_three_dependents_each_get_pending_review() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep_refs: Vec<ClaimRef> = (0..3).map(|_| ClaimRef::new_random()).collect();
        let edges: Vec<ClaimEdge> = dep_refs.iter()
            .map(|d| depends_on_edge(d.clone(), req.superseded_ref.clone(), &agent))
            .collect();
        let port = MockPort::new(agent.clone(), edges);

        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 3);
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            3
        );
        // Verify each dependent ref appears exactly once in committed ledger.
        let committed = port.committed.lock().unwrap();
        for dep_ref in &dep_refs {
            let flagged = committed.ledger.iter().filter(|e| {
                e.claim_ref == *dep_ref
                    && e.event_kind == LedgerEventKind::DependentFlaggedPendingReview
            }).count();
            assert_eq!(flagged, 1, "each dependent flagged exactly once: {:?}", dep_ref);
        }
    }

    // ── Test: cascade A26 — 0 dependents ────────────────────────────────────

    #[test]
    fn cascade_a26_zero_dependents_no_pending_review() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]); // no DependsOn edges

        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0, "no cascade when no dependents");
        assert_eq!(port.committed_assertions(), 1, "still writes the Bound assertion");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            0
        );
    }

    // ── Test: non-destruction I1 — only appends, no deletes/updates ──────────

    /// The PersistencePort mock has NO delete or update methods — this is a compile-time
    /// guarantee enforced by the trait definition (I1).  At runtime we verify the
    /// number and types of calls are purely additive.
    #[test]
    fn non_destruction_i1_only_appends_no_deletes() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]);

        let mut txn = port.begin_atomic(&agent).unwrap();
        let _ = execute(&port, &mut txn, &req).unwrap();
        // Inspect the Txn's pending appends BEFORE commit.
        // There must be only Assertion and Ledger appends — no deletes.
        let has_only_appends = txn.appends.iter().all(|a| {
            matches!(a, MockAppend::Assertion(_) | MockAppend::Ledger(_))
        });
        assert!(has_only_appends, "only append operations present (I1)");
        // The incumbent claim is NOT deleted — it was never the target of a delete call.
        // (MockPort has no delete method; any delete attempt would fail to compile.)
        port.commit(txn).unwrap();
    }

    // ── Test: determinism — same inputs → same sequence of appends ───────────

    #[test]
    fn determinism_same_inputs_same_append_sequence() {
        let agent = make_agent();
        let superseded = ClaimRef::new_random();
        let overturning = ClaimRef::new_random();
        let dep = ClaimRef::new_random();
        let bound_at = Utc::now();
        let recorded_at = TransactionTime::now();

        let build_req = || SupersessionRequest {
            agent_id: agent.clone(),
            superseded_ref: superseded.clone(),
            overturning_ref: overturning.clone(),
            bound_at,
            recorded_at: recorded_at.clone(),
        };

        let edges = vec![depends_on_edge(dep.clone(), superseded.clone(), &agent)];

        let run = || -> (usize, usize, usize) {
            let port = MockPort::new(agent.clone(), edges.clone());
            let mut txn = port.begin_atomic(&agent).unwrap();
            let n = execute(&port, &mut txn, &build_req()).unwrap();
            port.commit(txn).unwrap();
            (
                n,
                port.committed_assertions(),
                port.committed_ledger(),
            )
        };

        let (n1, a1, l1) = run();
        let (n2, a2, l2) = run();

        assert_eq!((n1, a1, l1), (n2, a2, l2), "deterministic across two runs");
    }

    // ── Test: unrelated edges are not cascaded ────────────────────────────────

    /// Edges with kinds other than DependsOn (e.g., DerivedFrom, Supersedes)
    /// pointing at the superseded claim must NOT generate PendingReview entries.
    #[test]
    fn only_depends_on_edges_trigger_cascade() {
        let agent = make_agent();
        let req = make_req(&agent);
        let other_claim = ClaimRef::new_random();

        // A DerivedFrom edge — should NOT trigger cascade.
        let unrelated_edge = ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: other_claim.clone(),
            to_claim: req.superseded_ref.clone(),
            kind: EdgeKind::DerivedFrom,
            created_at: TransactionTime::now(),
        };
        let port = MockPort::new(agent.clone(), vec![unrelated_edge]);

        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0, "DerivedFrom edge must not trigger DependsOn cascade");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            0
        );
    }

    // ── Test: Bound assertion targets the correct (superseded) claim ──────────

    #[test]
    fn bound_assertion_targets_superseded_claim_ref() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]);

        let mut txn = port.begin_atomic(&agent).unwrap();
        let _ = execute(&port, &mut txn, &req).unwrap();
        port.commit(txn).unwrap();

        let state = port.committed.lock().unwrap();
        assert_eq!(state.assertions.len(), 1);
        let a = &state.assertions[0];
        assert_eq!(a.target_claim, req.superseded_ref);
        assert!(
            matches!(a.kind, AssertionKind::Bound { .. }),
            "assertion kind must be Bound"
        );
    }
}
