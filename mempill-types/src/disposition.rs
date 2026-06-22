//! Disposition: the 12-state outcome model (SDK_CONTRACT §9, B3a/B3b).

/// The 12-state disposition model (SDK_CONTRACT §9, B3a/B3b).
/// Returned synchronously on every write; may transition asynchronously for heavy-path ops.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Disposition {
    /// New non-conflicting first-hand external fact; committed Active at low currency.
    CommittedCheap,
    /// ModelDerived; committed down-weighted, ineligible to overturn until anchored.
    CommittedInferred,
    /// Belief-overturning op accepted into async adjudication (non-blocking return).
    QueuedForAdjudication,
    /// External contradiction; oracle absent; incumbent downgraded; no resolution yet.
    Contested,
    /// Not enough to overturn; both claims surfaced; awaiting evidence/oracle.
    PendingConflict,
    /// A depended-on parent was superseded; dependent flagged for review (not auto-invalidated).
    PendingReview,
    /// Ambiguous/weak source; held pending corroboration/oracle confirmation.
    PendingLowConfidence,
    /// Burst/loop signature or incoherent tx/valid; parked, auditable, not destroyed.
    Quarantined,
    /// Belief-overturning accepted; prior claim bounded and retained in history.
    Superseded,
    /// Validity assertion marks claim as no-longer-true; retained in history.
    Invalidated,
    /// Valid-time reopened by external/first-hand assertion (non-terminal).
    Reinstated,
    /// Structural failure: missing/invalid provenance, malformed fact, write-authority violation.
    Rejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_has_exactly_12_variants() {
        // Enumerate all 12 variants — this test fails to compile if any are missing or renamed.
        let variants = [
            Disposition::CommittedCheap,
            Disposition::CommittedInferred,
            Disposition::QueuedForAdjudication,
            Disposition::Contested,
            Disposition::PendingConflict,
            Disposition::PendingReview,
            Disposition::PendingLowConfidence,
            Disposition::Quarantined,
            Disposition::Superseded,
            Disposition::Invalidated,
            Disposition::Reinstated,
            Disposition::Rejected,
        ];
        assert_eq!(variants.len(), 12);
    }

    #[test]
    fn disposition_equality() {
        assert_eq!(Disposition::CommittedCheap, Disposition::CommittedCheap);
        assert_ne!(Disposition::CommittedCheap, Disposition::Rejected);
    }

    #[test]
    fn disposition_round_trip_serde() {
        let d = Disposition::PendingReview;
        let json = serde_json::to_string(&d).unwrap();
        let back: Disposition = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn all_dispositions_round_trip_serde() {
        let variants = [
            Disposition::CommittedCheap,
            Disposition::CommittedInferred,
            Disposition::QueuedForAdjudication,
            Disposition::Contested,
            Disposition::PendingConflict,
            Disposition::PendingReview,
            Disposition::PendingLowConfidence,
            Disposition::Quarantined,
            Disposition::Superseded,
            Disposition::Invalidated,
            Disposition::Reinstated,
            Disposition::Rejected,
        ];
        for d in &variants {
            let json = serde_json::to_string(d).unwrap();
            let back: Disposition = serde_json::from_str(&json).unwrap();
            assert_eq!(d, &back);
        }
    }
}
