#![allow(missing_docs)]
//! PersistencePort — INSERT-only, agent_id-first persistence abstraction.
//!
//! All methods take `agent_id` as the primary parameter (not a filter).
//! Must enforce: single-writer per agent_id; append-only; atomic commit unit.

use mempill_types::{
    AgentId, Claim, ClaimEdge, ClaimRef, LedgerEntry, TransactionTime, ValidityAssertion,
};

/// An opaque transaction handle scoped to exactly one agent_id.
/// No cross-agent transaction is possible (atomic commit unit is per-agent_id).
pub trait Txn: Send + 'static {
    fn agent_id(&self) -> &AgentId;
}

/// The persistence port — INSERT-only, agent_id-first.
/// All methods take `agent_id` as the primary parameter (not a filter).
/// Must enforce: single-writer per agent_id; append-only; atomic commit unit.
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

    /// Load all claims on the given (agent_id, subject, predicate) subject-line,
    /// ordered by `tx_time ASC` (oldest first — callers fold in tx_time order).
    ///
    /// # Transaction-time cutoff (`as_of_tx_time`)
    ///
    /// When `as_of_tx_time` is `Some(T)`, only claims with `transaction_time <= T`
    /// are returned. This enforces correct bi-temporal tx-time semantics: a claim
    /// ingested after the query's as-of point did not exist at that point and must
    /// not be visible to the fold.
    ///
    /// When `as_of_tx_time` is `None`, all claims are returned (current view). Use
    /// `None` on all **write-path** callers (ingest, reconcile, adjudication) so
    /// that incumbent detection always sees the full current state; passing `Some`
    /// there would break conflict detection and succession.
    fn load_subject_line(
        &self,
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
        as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>,
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

    /// Load ALL ledger entries for the given claim refs, with no row cap.
    ///
    /// Intended for the read path (query_memory / query_history): builds the
    /// disposition map scoped to exactly the claims on a subject-line, avoiding
    /// the agent-wide capped scan that caused silent wrong-belief at scale.
    ///
    /// # Empty input
    ///
    /// When `claim_refs` is empty this method MUST return `Ok(vec![])` immediately
    /// without issuing any SQL (an empty `IN ()` clause is a syntax error on most
    /// backends).
    ///
    /// # No row cap
    ///
    /// Unlike `load_ledger`, this method applies no `LIMIT`. Subject-lines are
    /// small (typically 1–100 claims), so the result set is bounded naturally.
    ///
    /// # Transaction-time cutoff (`as_of_tx_time`)
    ///
    /// When `as_of_tx_time` is `Some(T)`, only ledger entries with
    /// `recorded_at <= T` are returned. This is required for correct bi-temporal
    /// tx-time travel: the disposition map must not see entries recorded after the
    /// query's as-of point, otherwise a post-T supersession would incorrectly exclude
    /// a claim that was still live at T.
    ///
    /// When `as_of_tx_time` is `None`, all entries are returned (preserves the
    /// current behaviour for callers that want the full history or the latest view).
    fn load_ledger_for_claims(
        &self,
        agent_id: &AgentId,
        claim_refs: &[ClaimRef],
        as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<LedgerEntry>, Self::Error>;

    fn load_edges_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, Self::Error>;

    /// Load the set of claims this agent served as injected context in the current session (for Amplification Guard entailment check).
    fn load_injected_claims(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<ClaimRef>, Self::Error>;

    /// Recursive CTE lineage traversal — returns the full `DerivedFrom` ancestry for a claim.
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
