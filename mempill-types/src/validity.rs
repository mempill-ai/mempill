//! ValidityAssertion: bounds or reopens a claim's valid-time interval.

use crate::claim::Confidence;
use crate::identity::{AgentId, ClaimRef};
use crate::provenance::ProvenanceLabel;
use crate::time::TransactionTime;

/// An assertion that bounds (invalidates) or reopens (reinstates) a claim's valid-time window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ValidityAssertion {
    pub assertion_ref: uuid::Uuid,
    pub agent_id: AgentId,
    pub target_claim: ClaimRef,
    pub kind: AssertionKind,
    /// Overturning requires External(*) precedence.
    pub provenance: ProvenanceLabel,
    pub confidence: Confidence,
    /// Engine-stamped.
    pub asserted_at: TransactionTime,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AssertionKind {
    /// Bounds the valid-time of the target claim (marks it no-longer-true as of `bound_at`).
    Bound { bound_at: chrono::DateTime<chrono::Utc> },
    /// Reopens the valid-time of a previously-bounded claim (Reinstated path).
    Reopen { reopen_at: chrono::DateTime<chrono::Utc> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentId;
    use crate::claim::Confidence;
    use crate::provenance::{ExternalKind, ProvenanceLabel};
    use crate::time::TransactionTime;
    use chrono::Utc;

    #[test]
    fn validity_assertion_round_trip_serde() {
        let now = Utc::now();
        let va = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: AgentId("agent-1".into()),
            target_claim: ClaimRef::new_random(),
            kind: AssertionKind::Bound { bound_at: now },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(now),
        };
        let json = serde_json::to_string(&va).unwrap();
        let back: ValidityAssertion = serde_json::from_str(&json).unwrap();
        assert_eq!(va.assertion_ref, back.assertion_ref);
        assert_eq!(va.kind, back.kind);
    }

    #[test]
    fn assertion_kind_bound_and_reopen_are_distinct() {
        let now = Utc::now();
        let bound = AssertionKind::Bound { bound_at: now };
        let reopen = AssertionKind::Reopen { reopen_at: now };
        assert_ne!(bound, reopen);
    }
}
