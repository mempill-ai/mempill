#![allow(missing_docs)]
//! AuditUseCase — read-only ledger query.
//!
//! Delegates to `engine::audit_ledger::query_ledger`. No Txn opened (read path).

use std::sync::Arc;

use mempill_types::TransactionTime;

use crate::{
    engine::audit_ledger::{self, AuditQuery},
    error::MemError,
    ports::PersistencePort,
};

use super::dto::{AuditQueryRequest, AuditQueryResponse};

/// Use-case: retrieve ordered audit ledger entries for an agent (or specific claim).
pub struct AuditUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
}

impl<P> AuditUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>) -> Self {
        Self { persistence }
    }

    /// Read-only. Loads ledger entries via the audit ledger. No transaction needed.
    pub fn execute(&self, req: AuditQueryRequest) -> Result<AuditQueryResponse, MemError> {
        // Map from_tx_time DateTime<Utc> → TransactionTime (for the port).
        let from_tx_time = req.from_tx_time.map(TransactionTime);

        let query = AuditQuery {
            agent_id: req.agent_id,
            claim_ref: req.claim_ref,
            from_tx_time,
            limit: req.limit,
        };

        let result = audit_ledger::query_ledger(&*self.persistence, &query)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        Ok(AuditQueryResponse { entries: result.entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::persistence::Txn;
    use mempill_types::{
        AgentId, Claim, ClaimEdge, ClaimRef, Disposition, LedgerEntry, LedgerEventKind,
        TransactionTime, ValidityAssertion,
    };
    use std::sync::Mutex;
    use chrono::Utc;

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock")]
    struct MockErr;

    struct MockStore {
        ledger: Mutex<Vec<LedgerEntry>>,
    }

    impl MockStore {
        fn with_entries(entries: Vec<LedgerEntry>) -> Self {
            Self { ledger: Mutex::new(entries) }
        }
    }

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
        fn load_ledger(&self, _a: &AgentId, _f: Option<&TransactionTime>, limit: usize) -> Result<Vec<LedgerEntry>, MockErr> {
            // Return newest-first to match the real store's convention.
            let mut entries = self.ledger.lock().unwrap().clone();
            entries.reverse(); // newest first
            entries.truncate(limit);
            Ok(entries)
        }
        fn load_ledger_for_claims(&self, _a: &AgentId, _refs: &[ClaimRef], _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_edges_for(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _a: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _a: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
    }

    fn make_entry(agent_id: &AgentId, at: chrono::DateTime<Utc>) -> LedgerEntry {
        LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: ClaimRef::new_random(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(at),
        }
    }

    #[test]
    fn audit_empty_store_returns_empty() {
        let store = Arc::new(MockStore::with_entries(vec![]));
        let uc = AuditUseCase::new(Arc::clone(&store));
        let resp = uc.execute(AuditQueryRequest {
            agent_id: AgentId("a".into()),
            claim_ref: None,
            from_tx_time: None,
            limit: 100,
        }).unwrap();
        assert!(resp.entries.is_empty());
    }

    #[test]
    fn audit_returns_entries_in_chronological_asc_order() {
        let agent = AgentId("a".into());
        let t1 = chrono::Utc::now();
        let t2 = t1 + chrono::Duration::seconds(10);
        let e1 = make_entry(&agent, t1);
        let e2 = make_entry(&agent, t2);
        // Store them oldest-first; mock will reverse to newest-first.
        let store = Arc::new(MockStore::with_entries(vec![e1.clone(), e2.clone()]));
        let uc = AuditUseCase::new(Arc::clone(&store));
        let resp = uc.execute(AuditQueryRequest {
            agent_id: agent,
            claim_ref: None,
            from_tx_time: None,
            limit: 100,
        }).unwrap();
        assert_eq!(resp.entries.len(), 2);
        // audit_ledger reverses back to ASC; oldest entry must come first.
        assert!(resp.entries[0].recorded_at.0 <= resp.entries[1].recorded_at.0,
            "entries must be in chronological order (ASC)");
    }
}
