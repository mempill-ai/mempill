//! Belief projection: derived, read-time types (SDK_CONTRACT §4.2, I3).
//! BeliefProjection is never stored; recomputed on every query_belief call.

use crate::claim::{Confidence, Criticality, Fact};
use crate::identity::ClaimRef;
use crate::provenance::ProvenanceLabel;
use crate::time::{TransactionTime, ValidTime};

// Re-use Cardinality from claim — it's defined there per the design.
// belief.rs needs Fact, etc., so we import the types from the parent modules.

/// The read-time canonical projection (SDK_CONTRACT §4.2).
/// Derived, never stored (I3). Recomputed on every query_belief call.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BeliefProjection {
    pub status: BeliefStatus,
    /// The claim covering NOW under the canonical fold, if unambiguous.
    pub primary: Option<Belief>,
    /// Both claims when Contested or Conflict (never silently picked).
    pub alternatives: Vec<Belief>,
    pub currency: CurrencyState,
    pub criticality: Criticality,
    pub staleness: StalenessFlag,
    pub markers: Vec<Marker>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BeliefStatus {
    /// Single live truth.
    Resolved,
    /// External contradiction, oracle absent (V3-5).
    Contested,
    /// Multiple mutually-exclusive active beliefs.
    Conflict,
    /// Value known, valid-time window unknown (F4).
    TimingUncertain,
    /// Subject-line exists but no currently-valid claim.
    NoBelief,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Belief {
    pub claim_ref: ClaimRef,
    pub fact: Fact,
    pub provenance: ProvenanceLabel,
    pub valid_time: ValidTime,
    pub transaction_time: TransactionTime,
    pub confidence: Confidence,
    pub currency_signal: CurrencySignal,
    pub criticality: Criticality,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CurrencyState {
    Fresh,
    AgingUnconfirmed,
    Decayed,
}

/// Derived, decaying signal (I11, V3-6, V3-7).
/// Refreshes only on provenance-independent restatement.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CurrencySignal {
    /// When this claim's currency was last refreshed (by a provenance-independent restatement).
    pub last_refreshed_at: TransactionTime,
    /// Computed decay state at read time (never stored; derived from now - last_refreshed_at).
    pub state: CurrencyState,
    /// Number of provenance-independent corroborating sources (confidence annotation only; not a gate).
    pub corroboration_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StalenessFlag {
    pub is_stale: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Marker {
    Contested,
    PendingConflict,
    PendingReview,
    /// Set member beyond currency decay threshold (V3-6).
    AgedSetMember,
    /// Claim origin includes RecallReEntry provenance.
    RecallTainted,
    /// derivation_depth > cap for currency boost (OP-1).
    LowDerivationAnchor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn belief_status_round_trip_serde() {
        let statuses = [
            BeliefStatus::Resolved,
            BeliefStatus::Contested,
            BeliefStatus::Conflict,
            BeliefStatus::TimingUncertain,
            BeliefStatus::NoBelief,
        ];
        for s in &statuses {
            let json = serde_json::to_string(s).unwrap();
            let back: BeliefStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, &back);
        }
    }

    #[test]
    fn currency_state_round_trip_serde() {
        let states = [CurrencyState::Fresh, CurrencyState::AgingUnconfirmed, CurrencyState::Decayed];
        for s in &states {
            let json = serde_json::to_string(s).unwrap();
            let back: CurrencyState = serde_json::from_str(&json).unwrap();
            assert_eq!(s, &back);
        }
    }

    #[test]
    fn staleness_flag_not_stale() {
        let f = StalenessFlag { is_stale: false, reason: None };
        assert!(!f.is_stale);
        assert!(f.reason.is_none());
    }

    #[test]
    fn marker_round_trip_serde() {
        let marker = Marker::RecallTainted;
        let json = serde_json::to_string(&marker).unwrap();
        let back: Marker = serde_json::from_str(&json).unwrap();
        assert_eq!(marker, back);
    }

    #[test]
    fn currency_signal_round_trip_serde() {
        let sig = CurrencySignal {
            last_refreshed_at: TransactionTime(Utc::now()),
            state: CurrencyState::Fresh,
            corroboration_count: 3,
        };
        let json = serde_json::to_string(&sig).unwrap();
        let back: CurrencySignal = serde_json::from_str(&json).unwrap();
        assert_eq!(sig.corroboration_count, back.corroboration_count);
        assert_eq!(sig.state, back.state);
    }
}
