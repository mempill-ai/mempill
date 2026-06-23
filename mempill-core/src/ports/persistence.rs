//! PersistencePort — INSERT-only, agent_id-first port (SDK_CONTRACT §6, I1, A4).
//!
//! All methods take `agent_id` as the primary parameter (not a filter).
//! MUST enforce: single-writer per agent_id; append-only; atomic commit unit.

use mempill_types::{
    AgentId, Claim, ClaimEdge, ClaimRef, LedgerEntry, TransactionTime, ValidityAssertion,
};

/// An opaque transaction handle scoped to exactly one agent_id (I9, DC-2).
/// No cross-agent transaction is possible.
pub trait Txn: Send + 'static {
    fn agent_id(&self) -> &AgentId;
}

/// The persistence port — INSERT-only, agent_id-first (SDK_CONTRACT §6, I1, A4).
/// All methods take agent_id as primary parameter (not a filter).
/// MUST enforce: single-writer per agent_id; append-only; atomic commit unit.
pub trait PersistencePort: Send + Sync + 'static {
    type Transaction: Txn;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Begin an atomic unit scoped to one agent_id. No cross-agent transaction allowed.
    fn begin_atomic(&self, agent_id: &AgentId) -> Result<Self::Transaction, Self::Error>;

    fn append_claim(
        &self,
        txn: &mut Self::Transaction,
        claim: &Claim,
    ) -> Result<ClaimRef, Self::Error>;

    fn append_validity_assertion(
        &self,
        txn: &mut Self::Transaction,
        assertion: &ValidityAssertion,
    ) -> Result<(), Self::Error>;

    fn append_ledger_entry(
        &self,
        txn: &mut Self::Transaction,
        entry: &LedgerEntry,
    ) -> Result<(), Self::Error>;

    fn append_claim_edge(
        &self,
        txn: &mut Self::Transaction,
        edge: &ClaimEdge,
    ) -> Result<(), Self::Error>;

    fn commit(&self, txn: Self::Transaction) -> Result<(), Self::Error>;
    fn rollback(&self, txn: Self::Transaction) -> Result<(), Self::Error>;

    // ── Read operations (non-mutating w.r.t. belief and history — I1, I3) ──

    fn load_subject_line(
        &self,
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
    ) -> Result<Vec<Claim>, Self::Error>;

    fn load_claim(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Option<Claim>, Self::Error>;

    fn load_validity_assertions_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ValidityAssertion>, Self::Error>;

    fn load_ledger(
        &self,
        agent_id: &AgentId,
        from: Option<&TransactionTime>,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, Self::Error>;

    fn load_edges_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, Self::Error>;

    /// Load the set of claims this agent served in session context (for C6 entailment check, F3).
    fn load_injected_claims(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<ClaimRef>, Self::Error>;

    /// Recursive CTE lineage traversal (DB_REQUIREMENTS.md §1).
    fn load_lineage(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, Self::Error>;

    /// Whether the store requires a global write serialization lock across all agent_ids.
    ///
    /// SQLite: true (single connection, no concurrent transactions possible).
    /// Postgres: false (pool provides concurrent transactions; advisory lock per agent_id).
    ///
    /// EngineHandle consults this at write-path entry to decide whether to acquire
    /// `store_write_lock`. Default = true (safe fallback for unknown adapters).
    fn requires_global_write_serialization(&self) -> bool {
        true
    }
}
