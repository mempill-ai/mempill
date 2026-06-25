//! LedgerEntry: append-only audit record for every state transition.
//!
//! The audit ledger is maintained by the AuditLedger component. Every `ingest_claim`
//! call produces at least one ledger entry. Entries are queryable by `agent_id`,
//! `claim_ref`, and transaction-time range via `query_audit`.

use crate::disposition::Disposition;
use crate::identity::{AgentId, ClaimRef};
use crate::time::TransactionTime;

/// Immutable audit record appended on every disposition event by the AuditLedger.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LedgerEntry {
    pub entry_id: uuid::Uuid,
    pub agent_id: AgentId,
    pub claim_ref: ClaimRef,
    pub event_kind: LedgerEventKind,
    pub disposition: Disposition,
    /// Rationale and measured estimators from the adjudication gate (for replay audit).
    pub rationale: Option<serde_json::Value>,
    /// Engine-stamped.
    pub recorded_at: TransactionTime,
}

/// The event that triggered this ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LedgerEventKind {
    ClaimCommitted,
    ValidityAsserted,
    AdjudicationRequested,
    AdjudicationResolved,
    RecallReEntryDetected,
    Quarantined,
    /// A dependent claim was flagged PendingReview because its parent was superseded.
    DependentFlaggedPendingReview,
    /// Claim was served as a query result — recorded so the Amplification Guard can detect later recall re-entry.
    ServedAsInjected,
    /// Adjudication TTL elapsed — challenger reverted to Contested (W6 sweep / lazy expiry).
    AdjudicationExpired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disposition::Disposition;
    use crate::identity::AgentId;
    use chrono::Utc;

    fn make_entry(kind: LedgerEventKind) -> LedgerEntry {
        LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: AgentId("agent-1".into()),
            claim_ref: ClaimRef::new_random(),
            event_kind: kind,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(Utc::now()),
        }
    }

    #[test]
    fn served_as_injected_event_is_present() {
        let e = make_entry(LedgerEventKind::ServedAsInjected);
        assert_eq!(e.event_kind, LedgerEventKind::ServedAsInjected);
    }

    #[test]
    fn dependent_flagged_pending_review_event_is_present() {
        let e = make_entry(LedgerEventKind::DependentFlaggedPendingReview);
        assert_eq!(e.event_kind, LedgerEventKind::DependentFlaggedPendingReview);
    }

    #[test]
    fn all_event_kinds_round_trip_serde() {
        let kinds = [
            LedgerEventKind::ClaimCommitted,
            LedgerEventKind::ValidityAsserted,
            LedgerEventKind::AdjudicationRequested,
            LedgerEventKind::AdjudicationResolved,
            LedgerEventKind::RecallReEntryDetected,
            LedgerEventKind::Quarantined,
            LedgerEventKind::DependentFlaggedPendingReview,
            LedgerEventKind::ServedAsInjected,
            LedgerEventKind::AdjudicationExpired,
        ];
        for k in &kinds {
            let json = serde_json::to_string(k).unwrap();
            let back: LedgerEventKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, &back);
        }
    }

    #[test]
    fn ledger_entry_round_trip_serde() {
        let e = make_entry(LedgerEventKind::ClaimCommitted);
        let json = serde_json::to_string(&e).unwrap();
        let back: LedgerEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e.entry_id, back.entry_id);
        assert_eq!(e.event_kind, back.event_kind);
        assert_eq!(e.disposition, back.disposition);
    }
}
