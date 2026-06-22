//! C8 — Provenance and Audit Ledger reads (TECHNICAL_DESIGN.md §1, §10 I1/I3, G1).
//!
//! READ-ONLY: this module only calls PersistencePort read methods.
//! No mutations. No Txn opened here (read path does not need I9 atomicity).
//!
//! Provides ordered ledger retrieval for:
//!   - A specific claim (by ClaimRef)
//!   - All claims for an agent (full ledger)
//!
//! The ledger is the G1 replay-audit basis: every decision recorded here is deterministic
//! and replayable (same inputs → same outcomes).

use mempill_types::{AgentId, ClaimRef, LedgerEntry, TransactionTime};

use crate::ports::persistence::PersistencePort;

// ── Audit query types ─────────────────────────────────────────────────────────

/// Request parameters for a ledger audit query (C8).
#[derive(Debug, Clone)]
pub(crate) struct AuditQuery {
    pub agent_id: AgentId,
    /// If `Some`, filter to entries for this specific claim. If `None`, return full agent ledger.
    pub claim_ref: Option<ClaimRef>,
    /// Optional lower-bound on `recorded_at` (inclusive). `None` = no lower bound.
    pub from_tx_time: Option<TransactionTime>,
    /// Maximum number of entries to return. Passed through to the persistence layer.
    pub limit: usize,
}

/// Response from a ledger audit query.
#[derive(Debug, Clone)]
pub(crate) struct AuditResult {
    /// Ledger entries ordered by `recorded_at` ASC (chronological for replay, G1).
    pub entries: Vec<LedgerEntry>,
}

// ── Audit read functions ──────────────────────────────────────────────────────

/// C8 — retrieve the ordered audit ledger for an agent (or a specific claim).
///
/// Delegates to `PersistencePort::load_ledger` (full agent ledger) and optionally
/// filters to a specific `claim_ref`.
///
/// ## Ordering
/// The persistence layer returns entries ordered by `recorded_at DESC` (newest first).
/// This function reverses to chronological (ASC) order for G1 replay correctness.
///
/// ## No side-effects
/// This is a pure read. No Txn opened. No ledger entry appended.
pub(crate) fn query_ledger<P: PersistencePort>(
    port: &P,
    query: &AuditQuery,
) -> Result<AuditResult, P::Error> {
    // Load from persistence (returns newest-first per index definition).
    let from_ref = query.from_tx_time.as_ref();
    let raw = port.load_ledger(&query.agent_id, from_ref, query.limit)?;

    // Filter by claim_ref if specified.
    let mut entries: Vec<LedgerEntry> = if let Some(cref) = &query.claim_ref {
        raw.into_iter().filter(|e| &e.claim_ref == cref).collect()
    } else {
        raw
    };

    // Reverse to chronological (ASC) order for G1 audit replay.
    entries.reverse();

    Ok(AuditResult { entries })
}

/// C8 — retrieve all edges for a claim (provenance lineage audit).
///
/// Uses `PersistencePort::load_lineage` which performs the recursive CTE traversal
/// (DB_REQUIREMENTS.md §1) to return the full DerivedFrom ancestry.
pub(crate) fn query_lineage<P: PersistencePort>(
    port: &P,
    agent_id: &AgentId,
    claim_ref: &ClaimRef,
) -> Result<Vec<mempill_types::ClaimEdge>, P::Error> {
    port.load_lineage(agent_id, claim_ref)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::persistence::Txn;
    use mempill_types::{
        AgentId, Claim, ClaimEdge, ClaimRef, Disposition, EdgeKind,
        LedgerEntry, LedgerEventKind,
        TransactionTime, ValidityAssertion,
    };
    use chrono::Utc;

    // ── Mock ──────────────────────────────────────────────────────────────────

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock error")]
    struct MockErr;

    struct MockPort {
        /// Ledger stored newest-first (simulating DB DESC ordering).
        ledger: Vec<LedgerEntry>,
        edges: Vec<ClaimEdge>,
    }

    impl MockPort {
        fn new_with_ledger(ledger: Vec<LedgerEntry>) -> Self {
            Self { ledger, edges: vec![] }
        }
        fn new_with_edges(edges: Vec<ClaimEdge>) -> Self {
            Self { ledger: vec![], edges }
        }
    }

    impl crate::ports::persistence::PersistencePort for MockPort {
        type Transaction = MockTxn;
        type Error = MockErr;

        fn begin_atomic(&self, a: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(a.clone())) }
        fn append_claim(&self, _: &mut MockTxn, _: &Claim) -> Result<ClaimRef, MockErr> { unimplemented!() }
        fn append_validity_assertion(&self, _: &mut MockTxn, _: &ValidityAssertion) -> Result<(), MockErr> { unimplemented!() }
        fn append_ledger_entry(&self, _: &mut MockTxn, _: &LedgerEntry) -> Result<(), MockErr> { unimplemented!() }
        fn append_claim_edge(&self, _: &mut MockTxn, _: &ClaimEdge) -> Result<(), MockErr> { unimplemented!() }
        fn commit(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn load_subject_line(&self, _: &AgentId, _: &str, _: &str) -> Result<Vec<Claim>, MockErr> { Ok(vec![]) }
        fn load_claim(&self, _: &AgentId, _: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }

        fn load_ledger(
            &self, _: &AgentId, _: Option<&TransactionTime>, _limit: usize,
        ) -> Result<Vec<LedgerEntry>, MockErr> {
            // Return newest-first (simulating DB DESC order).
            let mut entries = self.ledger.clone();
            entries.sort_by(|a, b| b.recorded_at.0.cmp(&a.recorded_at.0));
            Ok(entries)
        }

        fn load_edges_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> {
            Ok(self.edges.clone())
        }
        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> {
            Ok(self.edges.clone())
        }
    }

    fn agent() -> AgentId {
        AgentId("audit-agent".into())
    }

    fn make_ledger_entry(
        agent_id: &AgentId,
        claim_ref: ClaimRef,
        kind: LedgerEventKind,
        recorded_at: chrono::DateTime<Utc>,
    ) -> LedgerEntry {
        LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref,
            event_kind: kind,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(recorded_at),
        }
    }

    // ── Full ledger query ─────────────────────────────────────────────────────

    #[test]
    fn query_ledger_full_returns_chronological_asc() {
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(3);
        let t2 = Utc::now() - chrono::Duration::hours(2);
        let t3 = Utc::now() - chrono::Duration::hours(1);

        let e1 = make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ClaimCommitted, t1);
        let e2 = make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ValidityAsserted, t2);
        let e3 = make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::AdjudicationRequested, t3);

        let port = MockPort::new_with_ledger(vec![e1.clone(), e2.clone(), e3.clone()]);

        let query = AuditQuery {
            agent_id: agent.clone(),
            claim_ref: None,
            from_tx_time: None,
            limit: 100,
        };

        let result = query_ledger(&port, &query).unwrap();

        assert_eq!(result.entries.len(), 3);
        // Verify chronological order (ASC by recorded_at).
        assert_eq!(result.entries[0].recorded_at.0, t1, "first entry should be oldest (t1)");
        assert_eq!(result.entries[1].recorded_at.0, t2, "second entry should be t2");
        assert_eq!(result.entries[2].recorded_at.0, t3, "third entry should be newest (t3)");
    }

    // ── Claim-scoped ledger query ─────────────────────────────────────────────

    #[test]
    fn query_ledger_claim_scoped_filters_to_claim_ref() {
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(3);
        let t2 = Utc::now() - chrono::Duration::hours(2);

        let target_ref = ClaimRef::new_random();
        let other_ref = ClaimRef::new_random();

        let e1 = make_ledger_entry(&agent, target_ref.clone(), LedgerEventKind::ClaimCommitted, t1);
        let e2 = make_ledger_entry(&agent, other_ref.clone(), LedgerEventKind::ClaimCommitted, t2);

        let port = MockPort::new_with_ledger(vec![e1.clone(), e2.clone()]);

        let query = AuditQuery {
            agent_id: agent.clone(),
            claim_ref: Some(target_ref.clone()),
            from_tx_time: None,
            limit: 100,
        };

        let result = query_ledger(&port, &query).unwrap();

        assert_eq!(result.entries.len(), 1, "only the target claim's entries should be returned");
        assert_eq!(result.entries[0].claim_ref, target_ref);
    }

    // ── Empty ledger ──────────────────────────────────────────────────────────

    #[test]
    fn query_ledger_empty_returns_empty() {
        let agent = agent();
        let port = MockPort::new_with_ledger(vec![]);

        let query = AuditQuery {
            agent_id: agent.clone(),
            claim_ref: None,
            from_tx_time: None,
            limit: 100,
        };

        let result = query_ledger(&port, &query).unwrap();
        assert!(result.entries.is_empty(), "empty ledger should return empty result");
    }

    // ── Claim-scoped on non-existent claim ───────────────────────────────────

    #[test]
    fn query_ledger_claim_scoped_nonexistent_returns_empty() {
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(1);
        let e1 = make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ClaimCommitted, t1);
        let port = MockPort::new_with_ledger(vec![e1]);

        let query = AuditQuery {
            agent_id: agent.clone(),
            claim_ref: Some(ClaimRef::new_random()), // does not exist
            from_tx_time: None,
            limit: 100,
        };

        let result = query_ledger(&port, &query).unwrap();
        assert!(result.entries.is_empty(), "nonexistent claim should return empty audit");
    }

    // ── Lineage query ─────────────────────────────────────────────────────────

    #[test]
    fn query_lineage_returns_edges() {
        let agent = agent();
        let c1 = ClaimRef::new_random();
        let c2 = ClaimRef::new_random();

        let edge = ClaimEdge {
            edge_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: c1.clone(),
            to_claim: c2.clone(),
            kind: EdgeKind::DerivedFrom,
            created_at: TransactionTime(Utc::now()),
        };

        let port = MockPort::new_with_edges(vec![edge.clone()]);
        let edges = query_lineage(&port, &agent, &c1).unwrap();

        assert_eq!(edges.len(), 1, "lineage query should return the DerivedFrom edge");
        assert_eq!(edges[0].kind, EdgeKind::DerivedFrom);
        assert_eq!(edges[0].from_claim, c1);
        assert_eq!(edges[0].to_claim, c2);
    }

    // ── No mutations: read-only contract ────────────────────────────────────

    /// This test verifies that query_ledger does not call append_* methods on the port.
    /// The MockPort panics (unimplemented!) on any append call — if the test passes,
    /// no appends were made (I1, I3 compliance for the read path).
    #[test]
    fn query_ledger_makes_no_append_calls_read_only() {
        let agent = agent();
        let t1 = Utc::now();
        let e1 = make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ClaimCommitted, t1);
        let port = MockPort::new_with_ledger(vec![e1]);

        let query = AuditQuery {
            agent_id: agent.clone(),
            claim_ref: None,
            from_tx_time: None,
            limit: 100,
        };

        // If any append is called inside query_ledger, MockPort will panic (unimplemented!).
        let result = query_ledger(&port, &query);
        assert!(result.is_ok(), "read-only ledger query should succeed without panicking on appends");
    }

    // ── Chronological ordering determinism ────────────────────────────────────

    /// Two runs on the same ledger must return entries in the same order (G1 determinism).
    #[test]
    fn query_ledger_chronological_order_is_deterministic() {
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(5);
        let t2 = Utc::now() - chrono::Duration::hours(3);
        let t3 = Utc::now() - chrono::Duration::hours(1);

        let entries = vec![
            make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ClaimCommitted, t2),
            make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ClaimCommitted, t1),
            make_ledger_entry(&agent, ClaimRef::new_random(), LedgerEventKind::ClaimCommitted, t3),
        ];

        let port1 = MockPort::new_with_ledger(entries.clone());
        let port2 = MockPort::new_with_ledger(entries.clone());

        let query = AuditQuery {
            agent_id: agent.clone(),
            claim_ref: None,
            from_tx_time: None,
            limit: 100,
        };

        let result1 = query_ledger(&port1, &query).unwrap();
        let result2 = query_ledger(&port2, &query).unwrap();

        let times1: Vec<_> = result1.entries.iter().map(|e| e.recorded_at.0).collect();
        let times2: Vec<_> = result2.entries.iter().map(|e| e.recorded_at.0).collect();

        assert_eq!(times1, times2, "G1: audit ledger order must be deterministic across runs");
        // Verify ascending order.
        assert!(times1[0] <= times1[1] && times1[1] <= times1[2], "entries must be in ASC order");
    }
}
