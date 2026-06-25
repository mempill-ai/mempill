//! Shared valid-time helpers for read-time fold instant-selection and
//! write-time reconciler trusted-succession detection (TASK-11, §C/§D).
//!
//! PURE: no I/O, no clock reads. All instants are injected by callers.
//!
//! ## Trusted-succession predicate
//!
//! A set of claims forms a "trusted succession" iff ALL of the following hold:
//! 1. Every claim has `valid_time_confidence >= threshold` (§E — I2 fallback).
//! 2. Every claim has a bounded `start` (not None).
//! 3. The end may be None (= "until further notice"), but if present it must not
//!    cause overlap with any adjacent window.
//! 4. The windows are pairwise NON-OVERLAPPING (strict: [start, end) half-open).
//!
//! ## Instant-selection semantics (§C)
//!
//! Given a set of claims forming a trusted succession, the claim whose window
//! `[start, end)` contains `instant` is selected:
//!   - start ≤ instant  AND  (end is None OR instant < end)
//!
//! If no window contains the instant (gap), returns None → NoBelief.

use chrono::{DateTime, Utc};
use mempill_types::Claim;

// ── Low-level ValidTime helpers ───────────────────────────────────────────────

/// Returns `true` iff a `ValidTime` is "trusted": confidence >= threshold AND start is Some.
pub(crate) fn valid_time_is_trusted(vt: &mempill_types::ValidTime, threshold: f32) -> bool {
    vt.valid_time_confidence >= threshold && vt.start.is_some()
}

/// Returns `true` iff two `ValidTime` windows are NON-OVERLAPPING under half-open `[start, end)`.
///
/// Precondition: both `a.start` and `b.start` must be Some (call `valid_time_is_trusted` first).
pub(crate) fn valid_times_non_overlapping(
    a: &mempill_types::ValidTime,
    b: &mempill_types::ValidTime,
) -> bool {
    let a_start = a.start.unwrap();
    let b_start = b.start.unwrap();
    let a_end = a.end;
    let b_end = b.end;

    let a_ends_before_b_starts = match a_end {
        Some(ae) => ae <= b_start,
        None => false,
    };
    let b_ends_before_a_starts = match b_end {
        Some(be) => be <= a_start,
        None => false,
    };

    a_ends_before_b_starts || b_ends_before_a_starts
}

// ── Trusted-succession predicate ─────────────────────────────────────────────

/// Returns `true` iff a single claim has a "trusted" valid-time window:
///   - confidence >= threshold
///   - start is Some (bounded start is required for instant-selection)
///
/// A claim with end=None is treated as "open-ended" (until further notice), which is
/// valid for succession — it just means this is the current/most-recent window.
pub(crate) fn claim_is_trusted(claim: &Claim, threshold: f32) -> bool {
    let vt = claim.valid_time();
    vt.valid_time_confidence >= threshold && vt.start.is_some()
}

/// Returns `true` iff two claims have NON-OVERLAPPING valid-time windows under
/// the half-open interval semantics [start, end):
///
///   - A = [a_start, a_end)  (a_end = None → ∞)
///   - B = [b_start, b_end)  (b_end = None → ∞)
///
/// Non-overlapping means: a_end <= b_start  OR  b_end <= a_start.
/// When one end is None (= ∞), those two open-ended windows always overlap with
/// any window that starts after the other's start, so:
///   - if both ends are None and the starts differ → they overlap (both run to ∞).
///   - if a_end is None (∞) and b_start >= a_start → they overlap.
///   - Similarly for b_end is None.
///
/// Precondition: both claims must have `start` Some (ensured by caller via `claim_is_trusted`).
pub(crate) fn windows_non_overlapping(a: &Claim, b: &Claim) -> bool {
    let a_start = a.valid_time().start.unwrap(); // caller guarantees Some
    let b_start = b.valid_time().start.unwrap();
    let a_end = a.valid_time().end;
    let b_end = b.valid_time().end;

    // [a_start, a_end) does NOT overlap [b_start, b_end) iff:
    //   a_end <= b_start  OR  b_end <= a_start
    //
    // When a_end is None (∞): a runs to infinity, so a_end <= b_start is false (∞ > any b_start).
    // When b_end is None (∞): b runs to infinity, so b_end <= a_start is false.

    let a_ends_before_b_starts = match a_end {
        Some(ae) => ae <= b_start,
        None => false, // a_end = ∞ > b_start
    };
    let b_ends_before_a_starts = match b_end {
        Some(be) => be <= a_start,
        None => false,
    };

    a_ends_before_b_starts || b_ends_before_a_starts
}

/// Returns `true` iff all claims in `claims` form a trusted succession:
///   1. Every claim is trusted (confidence >= threshold, start is Some).
///   2. All windows are pairwise non-overlapping.
///
/// Empty or single-element slices return `true` (vacuously non-overlapping, but
/// callers should handle the single-claim case directly and not invoke the fold path
/// for conflict resolution).
pub(crate) fn is_trusted_succession(claims: &[&Claim], threshold: f32) -> bool {
    // Step 1: all must be trusted.
    if !claims.iter().all(|c| claim_is_trusted(c, threshold)) {
        return false;
    }
    // Step 2: pairwise non-overlapping.
    for i in 0..claims.len() {
        for j in (i + 1)..claims.len() {
            if !windows_non_overlapping(claims[i], claims[j]) {
                return false;
            }
        }
    }
    true
}

// ── Instant-selection ─────────────────────────────────────────────────────────

/// Given a set of claims that form a trusted succession, select the single claim
/// whose half-open window `[start, end)` contains `instant`.
///
/// Semantics:
///   - start ≤ instant  (start is inclusive)
///   - end > instant    (end is exclusive; None end = open-ended, always satisfies)
///
/// Returns `None` if no window contains `instant` (gap in coverage → NoBelief).
///
/// Precondition: all claims must have `start` Some (ensured by `is_trusted_succession`).
pub(crate) fn select_by_valid_time_instant<'a>(
    claims: &[&'a Claim],
    instant: DateTime<Utc>,
) -> Option<&'a Claim> {
    for claim in claims {
        let start = claim.valid_time().start.unwrap(); // guaranteed by precondition
        let end = claim.valid_time().end;

        let after_start = instant >= start;
        let before_end = end.is_none_or(|e| instant < e);

        if after_start && before_end {
            return Some(claim);
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, ExternalAnchor,
        ExternalKind, Fact, ProvenanceLabel, TransactionTime, ValidTime,
    };

    fn dt(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    fn make_claim(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>, confidence: f32) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("a".into()),
            Fact { subject: "s".into(), predicate: "p".into(), value: serde_json::json!("v") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(dt(2026, 1, 1)),
            ValidTime { start, end, valid_time_confidence: confidence },
            Confidence { value_confidence: 0.9, valid_time_confidence: confidence },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    const THRESHOLD: f32 = 0.7;

    // ── claim_is_trusted ──────────────────────────────────────────────────────

    #[test]
    fn trusted_with_start_and_high_confidence() {
        let c = make_claim(Some(dt(2024, 1, 1)), None, 0.9);
        assert!(claim_is_trusted(&c, THRESHOLD));
    }

    #[test]
    fn not_trusted_without_start() {
        let c = make_claim(None, None, 0.9);
        assert!(!claim_is_trusted(&c, THRESHOLD));
    }

    #[test]
    fn not_trusted_below_confidence() {
        let c = make_claim(Some(dt(2024, 1, 1)), None, 0.5);
        assert!(!claim_is_trusted(&c, THRESHOLD));
    }

    #[test]
    fn trusted_at_threshold_exactly() {
        let c = make_claim(Some(dt(2024, 1, 1)), None, 0.7);
        assert!(claim_is_trusted(&c, THRESHOLD));
    }

    // ── windows_non_overlapping ───────────────────────────────────────────────

    #[test]
    fn non_overlapping_a_ends_before_b_starts() {
        // A = [Jan, Mar),  B = [Mar, ∞)
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(windows_non_overlapping(&a, &b));
        assert!(windows_non_overlapping(&b, &a));
    }

    #[test]
    fn overlapping_windows() {
        // A = [Jan, Apr),  B = [Mar, ∞) — overlap in Mar
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 4, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(!windows_non_overlapping(&a, &b));
    }

    #[test]
    fn both_open_ended_overlap() {
        // A = [Jan, ∞),  B = [Mar, ∞) — both open-ended from different starts = overlap
        let a = make_claim(Some(dt(2024, 1, 1)), None, 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(!windows_non_overlapping(&a, &b));
    }

    #[test]
    fn adjacent_windows_exact_boundary() {
        // A = [Jan 1, Mar 1),  B = [Mar 1, May 1) — touching at Mar 1, not overlapping
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), Some(dt(2024, 5, 1)), 0.9);
        assert!(windows_non_overlapping(&a, &b));
    }

    // ── is_trusted_succession ─────────────────────────────────────────────────

    #[test]
    fn two_claim_trusted_succession() {
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(is_trusted_succession(&[&a, &b], THRESHOLD));
    }

    #[test]
    fn three_claim_chain_is_succession() {
        let a = make_claim(Some(dt(2020, 1, 1)), Some(dt(2022, 1, 1)), 0.9);
        let b = make_claim(Some(dt(2022, 1, 1)), Some(dt(2024, 1, 1)), 0.9);
        let c = make_claim(Some(dt(2024, 1, 1)), None, 0.9);
        assert!(is_trusted_succession(&[&a, &b, &c], THRESHOLD));
    }

    #[test]
    fn overlapping_is_not_succession() {
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 4, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(!is_trusted_succession(&[&a, &b], THRESHOLD));
    }

    #[test]
    fn low_confidence_breaks_succession() {
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.5); // below threshold
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(!is_trusted_succession(&[&a, &b], THRESHOLD));
    }

    #[test]
    fn no_start_breaks_succession() {
        let a = make_claim(None, Some(dt(2024, 3, 1)), 0.9); // no start
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        assert!(!is_trusted_succession(&[&a, &b], THRESHOLD));
    }

    // ── select_by_valid_time_instant ──────────────────────────────────────────

    #[test]
    fn select_first_window_matches_past_instant() {
        let a = make_claim(Some(dt(2020, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        let instant = dt(2022, 6, 1); // in A's window
        let selected = select_by_valid_time_instant(&[&a, &b], instant);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().claim_ref(), a.claim_ref());
    }

    #[test]
    fn select_second_window_matches_now_like_instant() {
        let a = make_claim(Some(dt(2020, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        let instant = dt(2025, 6, 1); // in B's open window
        let selected = select_by_valid_time_instant(&[&a, &b], instant);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().claim_ref(), b.claim_ref());
    }

    #[test]
    fn select_boundary_start_inclusive() {
        // Exactly at Mar 1 → end of A is exclusive, so this is in B
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 3, 1)), None, 0.9);
        let instant = dt(2024, 3, 1); // exactly at boundary
        let selected = select_by_valid_time_instant(&[&a, &b], instant);
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().claim_ref(), b.claim_ref()); // start inclusive: B selected
    }

    #[test]
    fn select_gap_returns_none() {
        // A = [Jan, Mar),  B = [May, ∞) — gap in April
        let a = make_claim(Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9);
        let b = make_claim(Some(dt(2024, 5, 1)), None, 0.9);
        let instant = dt(2024, 4, 1); // in the gap
        let selected = select_by_valid_time_instant(&[&a, &b], instant);
        assert!(selected.is_none());
    }

    #[test]
    fn select_before_all_windows_returns_none() {
        let a = make_claim(Some(dt(2024, 1, 1)), None, 0.9);
        let instant = dt(2020, 1, 1); // before A starts
        let selected = select_by_valid_time_instant(&[&a], instant);
        assert!(selected.is_none());
    }
}
