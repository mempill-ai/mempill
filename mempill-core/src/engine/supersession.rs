//! Invalidation / Supersession.
//!
//! Appends the atomic commit unit that closes an incumbent claim's valid-time window
//! and cascades a `PendingReview` flag to every dependent claim.
//!
//! Invariants enforced:
//!   - Non-destruction: only `append_*` calls; no DELETE or UPDATE.
//!   - Atomic commit unit: caller owns the `Txn`; this module appends within it.
//!   - Fixed-history monotonicity: supersession is a legitimate append; history is preserved.
//!   - Supersession owns the `DependsOn` cascade inside the same transaction.
//!
//! ## Transaction ownership
//! The application layer opens the `Txn` via `PersistencePort::begin_atomic()` and
//! calls `commit()` / `rollback()` as the only exit paths. This module
//! receives a `&mut Txn` that is already open and appends within it. It does NOT
//! call `begin_atomic`, `commit`, or `rollback`.

use std::collections::HashSet;

use mempill_types::{
    AgentId, AssertionKind, ClaimRef, Disposition, EdgeKind, LedgerEntry, LedgerEventKind,
    TransactionTime, ValidityAssertion,
};

use crate::ports::persistence::PersistencePort;

/// Parameters describing a belief-overturn that the adjudication gate has approved.
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
/// `agent_id`        — the owning agent; must match the Txn scope (single-writer-per-agent_id).
#[derive(Debug, Clone)]
pub(crate) struct SupersessionRequest {
    pub agent_id: AgentId,
    pub superseded_ref: ClaimRef,
    pub overturning_ref: ClaimRef,
    /// The bound-at instant for `AssertionKind::Bound`.
    pub bound_at: chrono::DateTime<chrono::Utc>,
    pub recorded_at: TransactionTime,
}

/// Execute the supersession atomic append sequence.
///
/// Performs, in order, within the ALREADY-OPEN `txn`:
///   1. Append `AssertionKind::Bound` `ValidityAssertion` closing the incumbent.
///   2. Append a `LedgerEventKind::ValidityAsserted` ledger entry for the incumbent.
///   3. For each edge in `preloaded_edges` where `edge.kind == EdgeKind::DependsOn && edge.to_claim == superseded_ref`,
///      append a `DependentFlaggedPendingReview` ledger entry for `edge.from_claim`.
///
/// `preloaded_edges` MUST be loaded by the caller BEFORE calling `begin_atomic()` to avoid
/// reads inside an open transaction.
///
/// Returns the total number of `DependentFlaggedPendingReview` entries appended (useful for
/// callers and tests).
///
/// # Errors
/// Any persistence error propagates immediately. The caller is responsible for rolling back
/// the `Txn` on error (atomic commit unit: all writes succeed or none do).
pub(crate) fn execute<P: PersistencePort>(
    port: &P,
    txn: &mut P::Transaction,
    req: &SupersessionRequest,
    preloaded_edges: &[mempill_types::ClaimEdge],
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

    // Step 3 — DependsOn edges are already loaded by the caller (preloaded_edges).
    // This avoids reads inside the open Txn (DEFECT-1 fix — see module doc).
    // `preloaded_edges` contains ALL edges for the superseded claim; we filter to
    // edges where `to_claim == superseded_ref` and `kind == DependsOn`.
    let edges = preloaded_edges;

    // Step 4 — Cascade: one DependentFlaggedPendingReview entry per dependent (A26).
    //
    // A26 IDEMPOTENCY: iterate edges in original order (preserving determinism, G1)
    // and track already-seen dependent ClaimRefs in a HashSet to ensure each distinct
    // dependent is flagged AT MOST ONCE per supersession event, even if duplicate
    // DependsOn edges exist in the adjacency table (e.g. from a bug or race condition).
    // Do NOT iterate the HashSet directly — its order is non-deterministic.
    let mut seen_dependents: HashSet<ClaimRef> = HashSet::new();
    let mut cascade_count = 0usize;
    for edge in edges {
        if edge.kind == EdgeKind::DependsOn && edge.to_claim == req.superseded_ref {
            // Skip if this dependent was already flagged in this cascade (A26 idempotency).
            if !seen_dependents.insert(edge.from_claim.clone()) {
                continue;
            }
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
        /// Tracks whether a transaction is currently open (mock hardening: reads fail when open).
        txn_open: Mutex<bool>,
    }

    impl MockPort {
        fn new(agent_id: AgentId, edges: Vec<ClaimEdge>) -> Self {
            Self {
                agent_id,
                edges,
                committed: Arc::new(Mutex::new(CommittedState::default())),
                txn_open: Mutex::new(false),
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
            *self.txn_open.lock().unwrap() = true;
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
            *self.txn_open.lock().unwrap() = false;
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
            *self.txn_open.lock().unwrap() = false;
            txn.rolled_back = true;
            // Discards all pending appends — nothing written to committed state.
            Ok(())
        }

        fn load_edges_for(
            &self,
            _agent_id: &AgentId,
            claim_ref: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockError> {
            // Mock hardening: reject reads while a transaction is open (mirrors real SqliteStore).
            if *self.txn_open.lock().unwrap() {
                return Err(MockError::InjectedFailure);
            }
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
            if *self.txn_open.lock().unwrap() { return Err(MockError::InjectedFailure); }
            Ok(vec![])
        }

        fn load_claim(
            &self, _: &AgentId, _: &ClaimRef,
        ) -> Result<Option<Claim>, MockError> {
            if *self.txn_open.lock().unwrap() { return Err(MockError::InjectedFailure); }
            Ok(None)
        }

        fn load_validity_assertions_for(
            &self, _: &AgentId, _: &ClaimRef,
        ) -> Result<Vec<ValidityAssertion>, MockError> {
            if *self.txn_open.lock().unwrap() { return Err(MockError::InjectedFailure); }
            Ok(vec![])
        }

        fn load_ledger(
            &self, _: &AgentId, _: Option<&TransactionTime>, _: usize,
        ) -> Result<Vec<LedgerEntry>, MockError> {
            if *self.txn_open.lock().unwrap() { return Err(MockError::InjectedFailure); }
            Ok(vec![])
        }

        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockError> {
            if *self.txn_open.lock().unwrap() { return Err(MockError::InjectedFailure); }
            Ok(vec![])
        }

        fn load_lineage(
            &self, _: &AgentId, _: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockError> {
            if *self.txn_open.lock().unwrap() { return Err(MockError::InjectedFailure); }
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
        let port = MockPort::new(agent.clone(), edges.clone());

        // Pre-load edges BEFORE opening the transaction (DEFECT-1 fix).
        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let cascade_n = execute(&port, &mut txn, &req, &preloaded).unwrap();
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
        let port = MockPort::new(agent.clone(), edges.clone());

        // Pre-load edges BEFORE opening the transaction (DEFECT-1 fix).
        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        // Fail on the 2nd write (the supersession ledger entry) — simulates mid-Txn failure.
        let mut txn = MockTxn::new(agent.clone()).with_fail_on(2);
        let result = execute(&port, &mut txn, &req, &preloaded);

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

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
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

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
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
    /// guarantee enforced by the trait definition (no delete/update methods exist).  At runtime we verify the
    /// number and types of calls are purely additive.
    #[test]
    fn non_destruction_i1_only_appends_no_deletes() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let _ = execute(&port, &mut txn, &req, &preloaded).unwrap();
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
            let req = build_req();
            let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
            let mut txn = port.begin_atomic(&agent).unwrap();
            let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
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

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
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

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let _ = execute(&port, &mut txn, &req, &preloaded).unwrap();
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

    // ═══════════════════════════════════════════════════════════════════════════
    // ADVERSARIAL TESTS — added by QA adversarial review (Wave 5 QA pass)
    // Properties under attack: CASCADE CORRECTNESS (A26), ATOMICITY (I9),
    // NON-DESTRUCTION (I1), MONOTONICITY (I10).
    // ═══════════════════════════════════════════════════════════════════════════

    // ── ADVERSARIAL: CASCADE CORRECTNESS (A26) ────────────────────────────────

    /// DIAMOND topology: claims B and C both DependsOn superseded claim A.
    /// D DependsOn B and also DependsOn C (diamond).
    /// When we supersede A, only B and C are flagged — D is NOT flagged
    /// (D depends on B/C, not on A).  Cascade count must be exactly 2.
    ///
    /// This tests that cascade does NOT transitively walk the graph.
    /// Only DIRECT dependents of the superseded claim are flagged (no transitive walk).
    #[test]
    fn cascade_a26_diamond_direct_dependents_only_no_transitive_walk() {
        let agent = make_agent();
        let req = make_req(&agent); // superseded = A
        let claim_b = ClaimRef::new_random();
        let claim_c = ClaimRef::new_random();
        let claim_d = ClaimRef::new_random();

        // B and C directly depend on A (the superseded claim).
        // D depends on B and C but NOT on A.
        let edges = vec![
            depends_on_edge(claim_b.clone(), req.superseded_ref.clone(), &agent),
            depends_on_edge(claim_c.clone(), req.superseded_ref.clone(), &agent),
            // D→B and D→C: these must NOT be returned by load_edges_for(A)
            depends_on_edge(claim_d.clone(), claim_b.clone(), &agent),
            depends_on_edge(claim_d.clone(), claim_c.clone(), &agent),
        ];
        let port = MockPort::new(agent.clone(), edges);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 2, "only B and C are direct dependents of A; D must not be flagged");
        let committed = port.committed.lock().unwrap();
        let flagged_refs: Vec<&ClaimRef> = committed.ledger.iter()
            .filter(|e| e.event_kind == LedgerEventKind::DependentFlaggedPendingReview)
            .map(|e| &e.claim_ref)
            .collect();
        assert!(flagged_refs.contains(&&claim_b), "B must be flagged");
        assert!(flagged_refs.contains(&&claim_c), "C must be flagged");
        assert!(!flagged_refs.contains(&&claim_d), "D must NOT be flagged (not a direct dependent of A)");
    }

    /// TWO DISTINCT DEPENDENTS sharing no edges: supersede claim P, with
    /// dependents X and Y (both DependsOn P, no shared structure).
    /// Both must be flagged — cascade count exactly 2.
    #[test]
    fn cascade_a26_two_distinct_dependents_both_flagged() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep_x = ClaimRef::new_random();
        let dep_y = ClaimRef::new_random();
        let edges = vec![
            depends_on_edge(dep_x.clone(), req.superseded_ref.clone(), &agent),
            depends_on_edge(dep_y.clone(), req.superseded_ref.clone(), &agent),
        ];
        let port = MockPort::new(agent.clone(), edges);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 2);
        let committed = port.committed.lock().unwrap();
        let x_flags = committed.ledger.iter()
            .filter(|e| e.claim_ref == dep_x && e.event_kind == LedgerEventKind::DependentFlaggedPendingReview)
            .count();
        let y_flags = committed.ledger.iter()
            .filter(|e| e.claim_ref == dep_y && e.event_kind == LedgerEventKind::DependentFlaggedPendingReview)
            .count();
        assert_eq!(x_flags, 1, "dep_x must be flagged exactly once");
        assert_eq!(y_flags, 1, "dep_y must be flagged exactly once");
    }

    /// DUPLICATE EDGES: two structurally identical DependsOn edges from the same
    /// dependent claim D to the superseded claim P (e.g., created by a bug or
    /// duplicate insert).
    ///
    /// Idempotent flagging is the expected behaviour — D should be
    /// flagged AT MOST ONCE per supersession event.  Two duplicate edges must
    /// not produce two PendingReview entries for D.
    ///
    /// FINDING (if this test FAILS): the engine double-flags D, producing 2
    /// DependentFlaggedPendingReview entries for the same dependent per
    /// supersession.  This is a DEFECT — severity: MEDIUM (doubles downstream
    /// review burden, may confuse projection.rs PendingReview aggregation).
    ///
    /// NOTE: This test is expected to FAIL against the current implementation
    /// because execute() iterates every edge returned by load_edges_for() and
    /// appends one ledger entry per edge — with no deduplication of from_claim.
    /// The mock's load_edges_for does not deduplicate edges with the same
    /// from_claim, so two duplicate edges produce two entries.
    ///
    /// Leave the assertion as == 1 to LOUDLY surface the bug if it is present.
    #[test]
    fn cascade_a26_duplicate_edges_same_dependent_flagged_exactly_once() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep_d = ClaimRef::new_random();

        // Two structurally identical DependsOn edges: D → superseded_ref (duplicate).
        let edge1 = depends_on_edge(dep_d.clone(), req.superseded_ref.clone(), &agent);
        let edge2 = depends_on_edge(dep_d.clone(), req.superseded_ref.clone(), &agent);
        // Note: edge_ids differ (uuid::Uuid::new_v4()) but from_claim == dep_d, to_claim == superseded_ref for both.

        let port = MockPort::new(agent.clone(), vec![edge1, edge2]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        // SPEC: D must be flagged exactly once regardless of duplicate edges.
        // If this assert fails, the impl double-flags D — that is the defect.
        assert_eq!(n, 1,
            "DEFECT: duplicate DependsOn edges caused double-flagging of the same dependent (A26 idempotency violated)");
        let committed = port.committed.lock().unwrap();
        let d_flags = committed.ledger.iter()
            .filter(|e| e.claim_ref == dep_d && e.event_kind == LedgerEventKind::DependentFlaggedPendingReview)
            .count();
        assert_eq!(d_flags, 1,
            "DEFECT: dependent D was flagged {} times instead of 1 (A26 idempotency violated)", d_flags);
    }

    /// WRONG-DIRECTION (source, not target): an edge where the superseded claim is
    /// the FROM (source) and some other claim is the TO.
    /// Direction: superseded_ref → other_claim, kind == DependsOn.
    /// This means: the superseded claim DEPENDS ON other_claim, not vice versa.
    /// Such an edge must NEVER trigger a cascade PendingReview for other_claim.
    ///
    /// The mock's load_edges_for already filters on `to_claim == claim_ref`, so
    /// this edge is correctly excluded at the query layer.  This test is
    /// regression armor: verifies the query contract is not accidentally loosened.
    #[test]
    fn cascade_a26_outbound_depends_on_from_superseded_does_not_cascade() {
        let agent = make_agent();
        let req = make_req(&agent);
        let other_claim = ClaimRef::new_random();

        // superseded_ref DEPENDS ON other_claim (wrong direction for cascade).
        let wrong_direction_edge = ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: req.superseded_ref.clone(), // superseded is the SOURCE
            to_claim: other_claim.clone(),
            kind: EdgeKind::DependsOn,
            created_at: TransactionTime::now(),
        };
        let port = MockPort::new(agent.clone(), vec![wrong_direction_edge]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0, "outbound DependsOn from superseded claim must not trigger cascade");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            0,
            "other_claim must NOT be flagged when it is the TO of superseded's outbound edge"
        );
    }

    /// DERIVED-FROM does not cascade: a DerivedFrom edge pointing AT the superseded
    /// claim (from_claim DerivedFrom superseded_ref) must NOT generate a PendingReview.
    /// Only DependsOn edges trigger the cascade, not DerivedFrom edges.
    #[test]
    fn cascade_a26_derived_from_inbound_does_not_cascade() {
        let agent = make_agent();
        let req = make_req(&agent);
        let derived_claim = ClaimRef::new_random();

        let derived_from_edge = ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: derived_claim.clone(),
            to_claim: req.superseded_ref.clone(),
            kind: EdgeKind::DerivedFrom, // not DependsOn
            created_at: TransactionTime::now(),
        };
        let port = MockPort::new(agent.clone(), vec![derived_from_edge]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0, "DerivedFrom edge must not cascade to PendingReview (A26 DependsOn-only)");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            0
        );
    }

    /// MUTUAL-EXCLUSION edge does not cascade: a MutualExclusion edge from another
    /// claim to the superseded claim must NOT generate a PendingReview.
    #[test]
    fn cascade_a26_mutual_exclusion_edge_does_not_cascade() {
        let agent = make_agent();
        let req = make_req(&agent);
        let peer_claim = ClaimRef::new_random();

        let mutex_edge = ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: peer_claim.clone(),
            to_claim: req.superseded_ref.clone(),
            kind: EdgeKind::MutualExclusion,
            created_at: TransactionTime::now(),
        };
        let port = MockPort::new(agent.clone(), vec![mutex_edge]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0, "MutualExclusion edge must not cascade");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            0
        );
    }

    /// SUPERSEDES edge does not cascade: a Supersedes edge pointing at the superseded
    /// claim (i.e., recording that the superseded claim was itself superseded by
    /// something else previously) must not cascade.
    #[test]
    fn cascade_a26_supersedes_edge_does_not_cascade() {
        let agent = make_agent();
        let req = make_req(&agent);
        let successor_claim = ClaimRef::new_random();

        let supersedes_edge = ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: successor_claim.clone(),
            to_claim: req.superseded_ref.clone(),
            kind: EdgeKind::Supersedes,
            created_at: TransactionTime::now(),
        };
        let port = MockPort::new(agent.clone(), vec![supersedes_edge]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0, "Supersedes edge must not cascade to PendingReview");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::DependentFlaggedPendingReview),
            0
        );
    }

    /// CASCADE ORDERING IS DETERMINISTIC: given a fixed ordered edge set, two
    /// executions produce cascade entries in the same order (stable ordering of
    /// PendingReview entries for the same edge list).
    #[test]
    fn cascade_a26_ordering_is_deterministic_for_fixed_edge_set() {
        let agent = make_agent();
        let superseded = ClaimRef::new_random();
        let overturning = ClaimRef::new_random();
        let bound_at = Utc::now();
        let recorded_at = TransactionTime::now();

        let dep_a = ClaimRef::new_random();
        let dep_b = ClaimRef::new_random();
        let dep_c = ClaimRef::new_random();

        let edges = vec![
            depends_on_edge(dep_a.clone(), superseded.clone(), &agent),
            depends_on_edge(dep_b.clone(), superseded.clone(), &agent),
            depends_on_edge(dep_c.clone(), superseded.clone(), &agent),
        ];

        let run_and_collect_order = || {
            let req = SupersessionRequest {
                agent_id: agent.clone(),
                superseded_ref: superseded.clone(),
                overturning_ref: overturning.clone(),
                bound_at,
                recorded_at: recorded_at.clone(),
            };
            let port = MockPort::new(agent.clone(), edges.clone());
            let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
            let mut txn = port.begin_atomic(&agent).unwrap();
            execute(&port, &mut txn, &req, &preloaded).unwrap();
            port.commit(txn).unwrap();
            let state = port.committed.lock().unwrap();
            state.ledger.iter()
                .filter(|e| e.event_kind == LedgerEventKind::DependentFlaggedPendingReview)
                .map(|e| e.claim_ref.clone())
                .collect::<Vec<_>>()
        };

        let order1 = run_and_collect_order();
        let order2 = run_and_collect_order();

        assert_eq!(order1, order2,
            "cascade ordering must be deterministic for a fixed edge set (A26, G1)");
    }

    // ── ADVERSARIAL: ATOMICITY (I9) — boundary failures ──────────────────────

    /// FIRST-WRITE FAILURE: fail on write 1 (the Bound ValidityAssertion).
    /// After rollback: zero committed assertions, zero ledger entries.
    /// The entire unit — including the ledger entry and any cascade — must be absent.
    #[test]
    fn atomicity_i9_fail_on_first_write_zero_committed() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep = ClaimRef::new_random();
        let edges = vec![depends_on_edge(dep, req.superseded_ref.clone(), &agent)];
        let port = MockPort::new(agent.clone(), edges);

        // Pre-load edges before opening txn; then inject failure into a raw MockTxn.
        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        // Fail on write 1 = append_validity_assertion (the Bound assertion).
        let mut txn = MockTxn::new(agent.clone()).with_fail_on(1);
        let result = execute(&port, &mut txn, &req, &preloaded);

        assert!(result.is_err(), "should fail on write 1");
        port.rollback(txn).unwrap();
        assert_eq!(port.committed_assertions(), 0, "no assertions committed after first-write failure");
        assert_eq!(port.committed_ledger(), 0, "no ledger entries committed after first-write failure");
    }

    /// LAST-CASCADE-WRITE FAILURE: with 3 dependents, writes are:
    ///   1 = Bound assertion
    ///   2 = ValidityAsserted ledger entry (incumbent)
    ///   3 = DependentFlaggedPendingReview for dep1
    ///   4 = DependentFlaggedPendingReview for dep2
    ///   5 = DependentFlaggedPendingReview for dep3  ← fail here
    ///
    /// After rollback: ZERO committed (the partial tail must not be committed).
    /// This tests that atomicity holds for the LAST write, not just mid-point.
    #[test]
    fn atomicity_i9_fail_on_last_cascade_write_zero_committed() {
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

        // Pre-load edges before opening txn; then inject failure into a raw MockTxn.
        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        // 3 dependents = 5 total writes; fail on write 5 (last cascade entry).
        let mut txn = MockTxn::new(agent.clone()).with_fail_on(5);
        let result = execute(&port, &mut txn, &req, &preloaded);

        assert!(result.is_err(), "should fail on the last cascade write (write 5)");
        port.rollback(txn).unwrap();
        assert_eq!(port.committed_assertions(), 0,
            "I9 violated: partial assertion committed despite last-write failure");
        assert_eq!(port.committed_ledger(), 0,
            "I9 violated: partial ledger committed despite last-write failure");
    }

    /// ZERO-DEPENDENT ATOMICITY: with no dependents, the atom is just
    /// {Bound assertion + ValidityAsserted ledger entry} — 2 writes.
    /// Both must commit as one unit; either both present or neither.
    #[test]
    fn atomicity_i9_zero_dependents_two_writes_atomic() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let n = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        assert_eq!(n, 0);
        assert_eq!(port.committed_assertions(), 1, "Bound assertion must be committed");
        assert_eq!(port.committed_ledger(), 1, "ValidityAsserted entry must be committed");
        assert_eq!(
            port.committed_ledger_by_kind(&LedgerEventKind::ValidityAsserted),
            1
        );
    }

    /// ZERO-DEPENDENT FIRST-WRITE FAILURE ROLLBACK: fail on write 1 with no dependents.
    /// After rollback: both assertion and ledger must be absent.
    #[test]
    fn atomicity_i9_zero_dependents_fail_first_write_zero_committed() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = MockTxn::new(agent.clone()).with_fail_on(1);
        let result = execute(&port, &mut txn, &req, &preloaded);

        assert!(result.is_err());
        port.rollback(txn).unwrap();
        assert_eq!(port.committed_assertions(), 0);
        assert_eq!(port.committed_ledger(), 0);
    }

    // ── ADVERSARIAL: NON-DESTRUCTION (I1) ────────────────────────────────────

    /// NON-DESTRUCTION with multiple dependents: even with a large cascade,
    /// no delete or update calls are made. The MockPort trait does not expose
    /// delete/update — this is a compile-time guarantee — but we verify that
    /// ALL appended items are typed as Assertion or Ledger (additive only),
    /// and that the incumbent superseded_ref claim itself does NOT have its
    /// claim_ref appear in any LedgerEntry as a delete-like event kind.
    #[test]
    fn non_destruction_i1_incumbent_not_deleted_with_cascade() {
        let agent = make_agent();
        let req = make_req(&agent);
        let dep1 = ClaimRef::new_random();
        let dep2 = ClaimRef::new_random();
        let edges = vec![
            depends_on_edge(dep1, req.superseded_ref.clone(), &agent),
            depends_on_edge(dep2, req.superseded_ref.clone(), &agent),
        ];
        let port = MockPort::new(agent.clone(), edges);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let _ = execute(&port, &mut txn, &req, &preloaded).unwrap();

        // All appended items before commit must be typed as Assertion or Ledger.
        let all_append_typed = txn.appends.iter().all(|a| {
            matches!(a, MockAppend::Assertion(_) | MockAppend::Ledger(_))
        });
        assert!(all_append_typed, "only additive Assertion/Ledger appends present (I1)");

        // The total write count: 1 (assertion) + 1 (ledger supersession) + 2 (cascade) = 4.
        assert_eq!(txn.appends.len(), 4, "write count must be exactly 4 for 2 dependents");

        port.commit(txn).unwrap();

        // After commit: the incumbent superseded_ref must not appear in a
        // DependentFlaggedPendingReview entry — it is the target, not a dependent.
        let committed = port.committed.lock().unwrap();
        let incumbent_flagged_as_dependent = committed.ledger.iter().any(|e| {
            e.claim_ref == req.superseded_ref
                && e.event_kind == LedgerEventKind::DependentFlaggedPendingReview
        });
        assert!(!incumbent_flagged_as_dependent,
            "I1 / A26: the superseded claim itself must not appear as a dependent in cascade entries");
    }

    // ── ADVERSARIAL: MONOTONICITY ─────────────────────────────────────────────

    /// Monotonicity: supersession must be recorded as a NEW Bound assertion
    /// (an append), never as a mutation of the incumbent claim row.
    /// We verify: after execute(), the ValidityAssertion in committed state has a
    /// fresh assertion_ref (uuid::Uuid), its own recorded_at, and the incumbent's
    /// claim_ref is referenced only as `target_claim` — not replaced.
    #[test]
    fn monotonicity_i10_supersession_is_append_not_rewrite() {
        let agent = make_agent();
        let req = make_req(&agent);
        let port = MockPort::new(agent.clone(), vec![]);

        let preloaded = port.load_edges_for(&agent, &req.superseded_ref).unwrap();
        let mut txn = port.begin_atomic(&agent).unwrap();
        let _ = execute(&port, &mut txn, &req, &preloaded).unwrap();
        port.commit(txn).unwrap();

        let state = port.committed.lock().unwrap();

        // Exactly one new ValidityAssertion was appended (the Bound).
        assert_eq!(state.assertions.len(), 1, "one Bound assertion appended (not zero, not two)");
        let assertion = &state.assertions[0];

        // The Bound assertion references the incumbent via target_claim — it does not
        // replace the incumbent's claim row. target_claim must be the superseded ref.
        assert_eq!(assertion.target_claim, req.superseded_ref,
            "Bound assertion must target the superseded claim ref (I10: append, not rewrite)");

        // The assertion_ref must be a non-nil UUID (freshly generated, not the superseded claim's UUID).
        assert_ne!(assertion.assertion_ref, uuid::Uuid::nil(),
            "Bound assertion must have a freshly generated assertion_ref");
        assert_ne!(assertion.assertion_ref.as_u128(), req.superseded_ref.0.as_u128(),
            "Bound assertion ref must differ from the superseded claim ref (I10: new row)");

        // The assertion kind must be Bound.
        assert!(
            matches!(assertion.kind, AssertionKind::Bound { .. }),
            "I10: supersession records a Bound assertion, not any other kind"
        );

        // The ledger entry for the incumbent uses ValidityAsserted, not a destructive event.
        let incumbent_entry = state.ledger.iter()
            .find(|e| e.claim_ref == req.superseded_ref)
            .expect("ledger entry for incumbent must exist");
        assert_eq!(incumbent_entry.event_kind, LedgerEventKind::ValidityAsserted,
            "I10: incumbent ledger event must be ValidityAsserted (append path), not a delete event");
        assert_eq!(incumbent_entry.disposition, Disposition::Superseded,
            "I10: incumbent disposition must be Superseded (marking only, not deletion)");
    }
}
