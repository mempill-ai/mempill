#![allow(missing_docs)]
//! PendingAdjudicationPort — DB-authoritative oracle queue port.
//!
//! This port is the persistence seam for the `pending_adjudications` table. It is SEPARATE
//! from `PersistencePort` because pending adjudications are oracle workflow state, not
//! claim/belief data. Keeping them apart preserves clean DDD layering.
//!
//! # DB-authoritative design
//!
//! The `pending_adjudications` table is the source of truth. In-memory caches are permitted
//! as an optimization but MUST be populated from the DB (not the reverse). Correctness never
//! depends on in-memory state surviving a restart.
//!
//! # Single-writer invariant
//!
//! Writes to this table occur after the main claim transaction commits (not inside it),
//! within the per-agent write lock already held by EngineHandle. This preserves claim-data
//! atomicity and keeps the pending-adjudication insert simple and non-transactional.

use chrono::{DateTime, Utc};
use mempill_types::{AgentId, AdjudicationRequest, ClaimRef};

/// A row in the `pending_adjudications` table.
///
/// `expires_at = None` means no TTL is configured for this row.
/// `status` starts as `'pending'`; verdict-apply marks it `'resolved'`; the sweep marks expired rows `'expired'`.
#[derive(Debug, Clone)]
pub struct PendingAdjudicationRow {
    /// Durable correlation key — returned by `OraclePort::handle_to_uuid`.
    pub handle_id: uuid::Uuid,
    pub agent_id: AgentId,
    pub subject: String,
    pub predicate: String,
    /// The incoming challenger claim.
    pub challenger_claim_ref: ClaimRef,
    /// The existing incumbent claim that was displaced.
    pub incumbent_claim_ref: ClaimRef,
    /// Full JSON-serialized `AdjudicationRequest` for oracle context reconstruction.
    pub request_payload: AdjudicationRequest,
    /// Wall-clock time the adjudication was queued (set by the engine, not the oracle).
    pub queued_at: DateTime<Utc>,
    /// TTL deadline — `None` when no TTL is configured.
    pub expires_at: Option<DateTime<Utc>>,
    /// Current status string: `"pending"`, `"resolved"`, or `"expired"`.
    pub status: String,
}

/// The pending-adjudication persistence port — read + write on the `pending_adjudications` table.
///
/// Implemented by both `mempill-sqlite` (`SqlitePendingStore`) and
/// `mempill-postgres` (`PostgresPendingStore`).
pub trait PendingAdjudicationPort: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Persist a new pending-adjudication row (status = 'pending', expires_at = NULL).
    ///
    /// Idempotent by PK: a second call with the same `handle_id` must fail (unique constraint).
    /// The engine guarantees each adjudication gets a fresh UUID handle.
    fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error>;

    /// Lookup a pending row by its `handle_id`. Returns `None` if not found.
    fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error>;

    /// List all pending rows for an agent (or all agents if `agent_id` is `None`).
    /// Ordered by `queued_at ASC`.
    fn list_pending(&self, agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error>;

    /// List all rows whose `expires_at` is not NULL, `expires_at <= now`, and `status = 'pending'`.
    /// Used by the sweep use-case. Ordered by `expires_at ASC`.
    fn list_expired(&self, now: DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error>;

    /// Mark a pending row as resolved (status = 'resolved'). Used by the verdict-apply step.
    ///
    /// Returns `Ok(())` if the row existed and was updated; `Ok(())` if the row was already
    /// resolved (idempotent). Returns `Err` only on DB error.
    fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error>;

    /// Mark a pending row as expired (status = 'expired'). Used by the sweep and lazy expiry path.
    ///
    /// Idempotent: re-marking an already-expired row is `Ok(())`. Returns `Err` only on DB error.
    fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error>;

    /// Return all `ClaimRef`s whose LATEST ledger disposition is `QueuedForAdjudication`
    /// AND that have NO matching row in `pending_adjudications` with `status = 'pending'`
    /// (i.e., crash-orphaned claims with no pending row).
    ///
    /// Used by the orphan-recovery sweep. Both adapters implement this as a cross-table SQL
    /// query. The returned tuples are `(agent_id, challenger_claim_ref, incumbent_claim_ref)`
    /// where `incumbent_claim_ref` is the most-recent CommittedCheap claim on the same
    /// (agent_id, subject, predicate) line — or `None` if not determinable (sweep skips those).
    ///
    /// NOTE: cross-table reads are safe here because orphan recovery is read-only discovery;
    /// all writes happen inside per-agent locked transactions.
    fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error>;
}

/// A QueuedForAdjudication claim with no matching pending_adjudications row.
/// Produced by `list_queued_orphan_claims` for the orphan-recovery sweep.
#[derive(Debug, Clone)]
pub struct OrphanedQueuedClaim {
    pub agent_id: AgentId,
    pub challenger_claim_ref: ClaimRef,
    /// The current live incumbent on the same (agent_id, subject, predicate) line,
    /// as determined by the adapter query. `None` if no incumbent could be found
    /// (the sweep skips such entries; they cannot be reliably reverted without knowing
    /// which incumbent to surface as Contested).
    pub incumbent_claim_ref: Option<ClaimRef>,
    pub subject: String,
    pub predicate: String,
}

// ── NoPendingStore ────────────────────────────────────────────────────────────

/// A zero-size sentinel type used as the default `S` parameter on `EngineHandle` when
/// no pending-adjudication store is wired in (i.e. `EngineHandle::new` is called without
/// calling `new_with_pending_store`).
///
/// All methods panic — they must never be called because `EngineHandle::ingest_claim`
/// checks `pending_store.is_some()` before invoking any `PendingAdjudicationPort` method.
#[derive(Debug)]
pub struct NoPendingStore;

/// Infallible error — `NoPendingStore` never returns an error because its methods panic.
#[derive(Debug)]
pub enum NoPendingStoreError {}

impl std::fmt::Display for NoPendingStoreError {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unreachable!()
    }
}

impl std::error::Error for NoPendingStoreError {}

impl PendingAdjudicationPort for NoPendingStore {
    type Error = NoPendingStoreError;

    fn insert_pending(&self, _row: &PendingAdjudicationRow) -> Result<(), Self::Error> {
        unreachable!("NoPendingStore::insert_pending must never be called (pending_store is None in EngineHandle)")
    }

    fn get_pending(&self, _handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> {
        unreachable!("NoPendingStore::get_pending must never be called")
    }

    fn list_pending(&self, _agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> {
        unreachable!("NoPendingStore::list_pending must never be called")
    }

    fn list_expired(&self, _now: DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> {
        unreachable!("NoPendingStore::list_expired must never be called")
    }

    fn mark_resolved(&self, _handle_id: uuid::Uuid) -> Result<(), Self::Error> {
        unreachable!("NoPendingStore::mark_resolved must never be called")
    }

    fn mark_expired(&self, _handle_id: uuid::Uuid) -> Result<(), Self::Error> {
        unreachable!("NoPendingStore::mark_expired must never be called")
    }

    fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> {
        unreachable!("NoPendingStore::list_queued_orphan_claims must never be called")
    }
}
