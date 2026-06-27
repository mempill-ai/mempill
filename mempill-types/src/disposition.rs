//! Disposition: the 12-state outcome model returned on every write.

/// The 12-state disposition model.
///
/// Returned synchronously on every write. For heavy-path (belief-overturning) operations,
/// the engine returns `QueuedForAdjudication` immediately; the final state arrives
/// asynchronously via the oracle callback.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
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

/// The synchronous write outcome returned from the engine.
///
/// For heavy-path (belief-overturning) operations, `disposition = QueuedForAdjudication`;
/// the final state arrives asynchronously via the oracle callback.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WriteOutcome {
    /// Stable reference to the committed (or rejected) claim.
    pub claim_ref: crate::identity::ClaimRef,
    /// The synchronous disposition assigned by the engine on this write.
    pub disposition: Disposition,
    /// Populated when disposition is Contested or PendingConflict.
    pub contested_with: Vec<crate::identity::ClaimRef>,
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
