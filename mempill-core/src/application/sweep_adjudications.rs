#![allow(missing_docs)]
//! SweepAdjudicationsUseCase — TTL expiry and orphan recovery.
//!
//! This use-case handles two distinct reversion scenarios atomically:
//!
//! 1. **Expired pending rows** (`revert_expired_row`): a pending row whose `expires_at <= now`.
//!    Writes a Contested ledger entry for the challenger, marks the pending row expired.
//!    The incumbent is ALREADY live (CommittedCheap), so no incumbent ledger entry is needed.
//!    After the revert, `query_memory` will surface `Contested[both]` because the challenger
//!    has a Contested disposition and the incumbent has CommittedCheap.
//!
//! 2. **Orphaned QueuedForAdjudication claims** (`revert_orphan`): claims with
//!    `QueuedForAdjudication` disposition that have NO matching `pending_adjudications` row.
//!    These arise from a crash in the window between main-txn commit and pending-row insert
//!    (see the post-commit orphan window note in ingest_claim.rs). Recovery writes a Contested ledger entry so the
//!    claim is treated as `Contested[both]` from that point forward.
//!
//! # Lock invariant
//!
//! This use-case is SYNC — called from `spawn_blocking`. Locks are acquired by the
//! `EngineHandle::sweep_expired_adjudications` caller BEFORE dispatch.
//!
//! # Transaction discipline
//!
//! All reads happen BEFORE `begin_atomic`. Writes (ledger entry + pending-store update)
//! happen inside one `begin_atomic`/`commit` unit. On any error: rollback + `Err(MemError)`.
//!
//! The pending-store `mark_expired` is called AFTER commit (same pattern as `mark_resolved`
//! in `submit_adjudication`).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use mempill_types::{
    AgentId, Disposition, LedgerEntry, LedgerEventKind, TransactionTime,
};

use crate::{
    engine_handle::ErasedPendingStore,
    error::MemError,
    ports::{
        pending_adjudication::{OrphanedQueuedClaim, PendingAdjudicationRow},
        PersistencePort,
    },
};

/// Sync use-case: sweeps expired adjudication rows and recovers orphaned claims.
pub struct SweepAdjudicationsUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    pending_store: Arc<dyn ErasedPendingStore>,
}

impl<P> SweepAdjudicationsUseCase<P>
where
    P: PersistencePort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, pending_store: Arc<dyn ErasedPendingStore>) -> Self {
        Self { persistence, pending_store }
    }

    /// Revert a single expired pending row to Contested.
    ///
    /// Returns `true` if the row was successfully reverted, `false` if the challenger
    /// is already no longer `QueuedForAdjudication` (idempotency guard — another process
    /// or the lazy-expiry path may have already handled it).
    pub fn revert_expired_row(
        &self,
        row: &PendingAdjudicationRow,
        now: DateTime<Utc>,
    ) -> Result<bool, MemError> {
        let tx_time = TransactionTime(now);
        let agent_id: AgentId = row.agent_id.clone();
        let challenger_ref = row.challenger_claim_ref.clone();
        let handle_id = row.handle_id;

        // ── Pre-check: verify challenger is still QueuedForAdjudication ──────────
        // Load ledger BEFORE begin_atomic to avoid reads inside an open transaction.
        let ledger = self.persistence
            .load_ledger(&agent_id, None, 10_000)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let challenger_disp = latest_disposition_from_ledger(&ledger, &challenger_ref);
        if challenger_disp != Some(Disposition::QueuedForAdjudication) {
            // Already resolved or reverted — idempotent skip.
            return Ok(false);
        }

        // ── Atomic write: Contested ledger entry for challenger ───────────────────
        let mut txn = self.persistence
            .begin_atomic(&agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let contested_entry = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: challenger_ref.clone(),
            event_kind: LedgerEventKind::AdjudicationExpired,
            disposition: Disposition::Contested,
            rationale: Some(serde_json::json!({
                "event": "adjudication_ttl_expired",
                "handle_id": handle_id.to_string(),
                "expired_at": now.to_rfc3339(),
                "incumbent_claim_ref": row.incumbent_claim_ref.0.to_string(),
            })),
            recorded_at: tx_time,
        };

        let write_result = self.persistence
            .append_ledger_entry(&mut txn, &contested_entry)
            .map_err(|e| MemError::Persistence { source: Box::new(e) });

        match write_result {
            Ok(()) => {
                self.persistence
                    .commit(txn)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

                // Mark pending row expired AFTER commit (outside txn, within write lock).
                self.pending_store
                    .mark_expired_erased(handle_id)
                    .map_err(|e| MemError::PendingStore { source: e })?;

                Ok(true)
            }
            Err(e) => {
                let _ = self.persistence.rollback(txn);
                Err(e)
            }
        }
    }

    /// Revert an orphaned QueuedForAdjudication claim (no pending row) to Contested.
    ///
    /// Returns `true` if the reversion was applied, `false` if the claim is already
    /// no longer `QueuedForAdjudication` (idempotency guard).
    pub fn revert_orphan(
        &self,
        orphan: &OrphanedQueuedClaim,
        now: DateTime<Utc>,
    ) -> Result<bool, MemError> {
        let tx_time = TransactionTime(now);
        let agent_id: AgentId = orphan.agent_id.clone();
        let challenger_ref = orphan.challenger_claim_ref.clone();

        // ── Pre-check: verify challenger is still QueuedForAdjudication ──────────
        let ledger = self.persistence
            .load_ledger(&agent_id, None, 10_000)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let challenger_disp = latest_disposition_from_ledger(&ledger, &challenger_ref);
        if challenger_disp != Some(Disposition::QueuedForAdjudication) {
            // Already resolved or reverted — idempotent skip.
            return Ok(false);
        }

        // ── Atomic write: Contested ledger entry for orphaned challenger ──────────
        let mut txn = self.persistence
            .begin_atomic(&agent_id)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        let incumbent_ref_str = orphan.incumbent_claim_ref
            .as_ref()
            .map(|r| r.0.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let contested_entry = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: challenger_ref.clone(),
            event_kind: LedgerEventKind::AdjudicationExpired,
            disposition: Disposition::Contested,
            rationale: Some(serde_json::json!({
                "event": "orphaned_queued_claim_recovery",
                "reason": "QueuedForAdjudication claim had no matching pending_adjudications row",
                "recovered_at": now.to_rfc3339(),
                "incumbent_claim_ref": incumbent_ref_str,
                "subject": orphan.subject,
                "predicate": orphan.predicate,
            })),
            recorded_at: tx_time,
        };

        let write_result = self.persistence
            .append_ledger_entry(&mut txn, &contested_entry)
            .map_err(|e| MemError::Persistence { source: Box::new(e) });

        match write_result {
            Ok(()) => {
                self.persistence
                    .commit(txn)
                    .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
                Ok(true)
            }
            Err(e) => {
                let _ = self.persistence.rollback(txn);
                Err(e)
            }
        }
    }
}

/// Extract the latest `Disposition` for a given `ClaimRef` from a ledger slice.
fn latest_disposition_from_ledger(
    ledger: &[mempill_types::LedgerEntry],
    target: &mempill_types::ClaimRef,
) -> Option<Disposition> {
    ledger
        .iter()
        .filter(|e| &e.claim_ref == target)
        .max_by_key(|e| e.recorded_at.0)
        .map(|e| e.disposition.clone())
}
