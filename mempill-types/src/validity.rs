//! ValidityAssertion: bounds or reopens a claim's valid-time interval.

use crate::claim::Confidence;
use crate::identity::{AgentId, ClaimRef};
use crate::provenance::ProvenanceLabel;
use crate::time::{DateGranularity, TransactionTime};

/// An assertion that bounds (invalidates) or reopens (reinstates) a claim's valid-time window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ValidityAssertion {
    /// Unique ID for this validity assertion (random UUID).
    pub assertion_ref: uuid::Uuid,
    /// The agent that submitted this validity assertion.
    pub agent_id: AgentId,
    /// The claim whose valid-time this assertion modifies.
    pub target_claim: ClaimRef,
    /// Whether this assertion bounds or reopens the target claim.
    pub kind: AssertionKind,
    /// Overturning requires External(*) precedence.
    pub provenance: ProvenanceLabel,
    /// Confidence in this validity assertion.
    pub confidence: Confidence,
    /// Engine-stamped.
    pub asserted_at: TransactionTime,
}

/// The kind of validity assertion: whether it bounds or reopens a claim.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum AssertionKind {
    /// Bounds the valid-time of the target claim (marks it no-longer-true as of `bound_at`).
    Bound {
        /// The UTC instant at which the claim's validity ends.
        bound_at: chrono::DateTime<chrono::Utc>,
        /// The display precision `bound_at` was supplied at (TASK-33-W5-LIB-R2, DIAG-5):
        /// an Affirm sets this from the winning challenger's `start_granularity`; a Deny
        /// (tx-time fallback) and `assert_validity`/`end_fact` populate it from the parsed
        /// date's granularity, when known. `None` for legacy rows written before this field
        /// existed, or when the bounding instant has no tracked precision (e.g. Deny's
        /// tx_time fallback). `#[serde(default)]` so existing persisted rows (SQLite TEXT,
        /// Postgres JSONB-free relational column) deserialize unchanged — additive only.
        #[serde(default)]
        bound_at_granularity: Option<DateGranularity>,
    },
    /// Reopens the valid-time of a previously-bounded claim (Reinstated path).
    Reopen {
        /// The UTC instant at which the claim's validity is resumed.
        reopen_at: chrono::DateTime<chrono::Utc>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentId;
    use crate::claim::Confidence;
    use crate::provenance::{ExternalKind, ProvenanceLabel};
    use crate::time::{DateGranularity, TransactionTime};
    use chrono::Utc;

    #[test]
    fn validity_assertion_round_trip_serde() {
        let now = Utc::now();
        let va = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: AgentId("agent-1".into()),
            target_claim: ClaimRef::new_random(),
            kind: AssertionKind::Bound { bound_at: now, bound_at_granularity: Some(DateGranularity::Month) },
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
        let bound = AssertionKind::Bound { bound_at: now, bound_at_granularity: None };
        let reopen = AssertionKind::Reopen { reopen_at: now };
        assert_ne!(bound, reopen);
    }

    /// TASK-33-W5-LIB-R2: a `Bound` row written BEFORE `bound_at_granularity` existed (no key
    /// in the JSON at all) must still deserialize — `#[serde(default)]` makes the field
    /// optional on read, so old JSON blobs (and, by the same mechanism, old adapter rows)
    /// upgrade cleanly to `None` rather than failing to parse.
    #[test]
    fn assertion_kind_bound_deserializes_legacy_json_without_granularity_field() {
        let now = Utc::now();
        let va = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: AgentId("agent-1".into()),
            target_claim: ClaimRef::new_random(),
            kind: AssertionKind::Bound { bound_at: now, bound_at_granularity: Some(DateGranularity::Month) },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(now),
        };
        // Simulate a pre-this-change JSON blob by stripping the new key entirely (rather
        // than hand-crafting the whole envelope, which risks drifting from the real
        // `#[serde(tag = "type", content = "kind")]`/adjacently-tagged shapes elsewhere).
        let mut json = serde_json::to_value(&va).unwrap();
        json["kind"]["Bound"]
            .as_object_mut()
            .expect("Bound kind must serialize as an externally-tagged {\"Bound\": {...}} object")
            .remove("bound_at_granularity");

        let back: ValidityAssertion = serde_json::from_value(json)
            .expect("legacy Bound JSON without bound_at_granularity must still deserialize");
        match back.kind {
            AssertionKind::Bound { bound_at_granularity, .. } => {
                assert_eq!(bound_at_granularity, None, "legacy row must default to None, not fail to parse");
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }
}
