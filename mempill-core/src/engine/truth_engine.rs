#![allow(missing_docs)]
//! TruthEngine — canonical valid-time fold.
//!
//! This module is PURE given its inputs — no I/O, no system clock.
//! All time parameters are injected by the caller.
//!
//! ## Ordering-key rule:
//! - If `valid_time_confidence >= config.valid_time_confidence_threshold`:
//!   ordering key = valid_time_start (authoritative)
//! - Else:
//!   ordering key = transaction_time (fallback)
//!
//! ## Fold invariants:
//! - Read-time-canonical: same stored claims → same belief, arrival-order independent.
//! - Fixed-history monotonicity: belief is monotone over a fixed history.
//! - Belief is derived, never stored — callers always re-fold at query time.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use mempill_types::{
    AssertionKind, Belief, BeliefStatus, Cardinality, Claim, ClaimRef, CurrencySignal,
    CurrencyState, Disposition, StalenessFlag, ValidityAssertion,
};

use crate::config::EngineConfig;
use crate::engine::valid_time_helpers;

/// Dispositions that make a claim non-live regardless of ValidityAssertions.
/// Even without a Bound assertion, a claim with one of these dispositions
/// must be excluded from the live set by the disposition-based liveness filter.
fn is_non_live_disposition(d: &Disposition) -> bool {
    matches!(
        d,
        Disposition::Quarantined
            | Disposition::Superseded
            | Disposition::Invalidated
            | Disposition::Rejected
    )
}

// ── Public result type ────────────────────────────────────────────────────────

/// The result of a canonical fold over one subject-line.
///
/// `live_claims` = claims that are currently valid at `as_of_tx_time` (i.e. not bounded by
/// any validity assertion, or reopened after bounding, under the canonical evaluation).
/// They are ordered by the canonical ordering key (valid_time or tx_time, depending on confidence).
///
/// `all_claims` = the full set of claims passed in (retained for history; supports fixed-history monotonicity audit).
///
/// `conflict` = true when two or more live claims have overlapping validity windows and
/// conflicting values, signalling a Contested / Conflict state to projection.rs.
#[derive(Debug, Clone)]
pub(crate) struct FoldResult {
    /// Canonically ordered live claims (not bounded at `as_of_tx_time`).
    pub live_claims: Vec<ClaimWithStatus>,
    /// True when ≥ 2 live claims conflict on the same subject-line without resolution.
    pub has_conflict: bool,
    /// True when valid-time instant-selection was applied: the live_claims set was narrowed
    /// from a trusted succession to a single claim matching the query instant via valid-time instant-selection.
    /// When true, `has_conflict` is always false and `live_claims.len()` is 0 or 1.
    pub succession_selected: bool,
}

/// A claim with its resolved live/bounded status at the fold's `as_of_tx_time`.
#[derive(Debug, Clone)]
pub(crate) struct ClaimWithStatus {
    pub claim: Claim,
    /// True if this claim is NOT bounded (or has been reopened) at `as_of_tx_time`.
    pub is_live: bool,
    /// The disposition recorded in the last ledger entry for this claim, if known.
    pub last_disposition: Option<Disposition>,
}

// ── Ordering key ──────────────────────────────────────────────────────────────

/// Ordering key for the canonical fold.
///
/// Produces a deterministic total order that is arrival-order independent.
///
/// When valid_time_confidence >= threshold → use valid_time_start (authoritative).
/// When below threshold, or when valid_time_start is None → fall back to tx_time.
///
/// The secondary tie-breaker is always tx_time (engine-stamped, unique in practice).
/// The tertiary tie-breaker is the ClaimRef UUID to guarantee total order even with
/// equal timestamps.
/// Returns `(primary_key, tx_time_fallback, uuid_tiebreaker)` for deterministic total order.
fn ordering_key(claim: &Claim, config: &EngineConfig) -> (DateTime<Utc>, DateTime<Utc>, u128) {
    let primary = if claim.valid_time().valid_time_confidence >= config.valid_time_confidence_threshold {
        claim.valid_time().start.unwrap_or(claim.transaction_time().0)
    } else {
        claim.transaction_time().0
    };
    (primary, claim.transaction_time().0, claim.claim_ref().0.as_u128())
}

// ── Validity resolution ───────────────────────────────────────────────────────

/// Evaluate whether a claim is live at `as_of_tx_time` given the full set of
/// validity assertions for that claim.
///
/// Rules (non-destructive: no deletes; fixed-history monotone: liveness is monotone for fixed history):
///   - A `Bound` assertion with `bound_at <= as_of_tx_time` closes the claim (not live).
///   - A subsequent `Reopen` with `reopen_at <= as_of_tx_time` re-opens it.
///   - Assertions are processed in chronological order of their `asserted_at` timestamp.
///   - The final state after processing all assertions determines liveness.
pub(crate) fn is_claim_live(
    assertions: &[ValidityAssertion],
    as_of_tx_time: DateTime<Utc>,
) -> bool {
    // Start live; each Bound/Reopen toggles state.
    // Sort by asserted_at ascending for deterministic processing (I8).
    let mut sorted: Vec<&ValidityAssertion> = assertions.iter().collect();
    sorted.sort_by(|a, b| {
        a.asserted_at.0.cmp(&b.asserted_at.0)
            .then(a.assertion_ref.cmp(&b.assertion_ref)) // UUID tiebreaker for I8
    });

    let mut live = true;
    for assertion in sorted {
        // Only assertions at or before as_of_tx_time are visible (bi-temporal rule).
        if assertion.asserted_at.0 > as_of_tx_time {
            continue;
        }
        match &assertion.kind {
            AssertionKind::Bound { bound_at } => {
                if *bound_at <= as_of_tx_time {
                    live = false;
                }
            }
            AssertionKind::Reopen { reopen_at } => {
                if *reopen_at <= as_of_tx_time {
                    live = true;
                }
            }
            // AssertionKind is #[non_exhaustive] — future assertion kinds are ignored (conservative: treat as no-op).
            _ => {}
        }
    }
    live
}

// ── Canonical fold ────────────────────────────────────────────────────────────

/// Canonical valid-time fold.
///
/// PURE: all inputs passed in; no I/O; no system clock calls.
///
/// Parameters:
/// - `claims`: all claims for the subject-line (loaded via PersistencePort; any order).
/// - `assertions_for`: a function mapping `ClaimRef → Vec<ValidityAssertion>` for the
///   claims in `claims`.  Callers supply this as a closure to keep the fold pure (no I/O here).
/// - `as_of_tx_time`: the bi-temporal query point (≤ now for historical queries).
///   Controls which assertions and claims are *visible* (transaction-time axis).
/// - `valid_at_instant`: optional valid-time query instant (valid-time axis).
///   When `Some`, the result is narrowed to the claim whose valid-time window contains this
///   instant, **after** the tx-time visibility filter is applied first (D2 independence rule).
///   When `None`, the existing behaviour is preserved: `as_of_tx_time` is also used as the
///   valid-time instant for succession selection, keeping backward compatibility.
/// - `config`: EngineConfig for the ordering-key confidence threshold.
/// - `latest_disposition`: map of ClaimRef → latest Disposition from the ledger.
///   Claims whose latest disposition is Quarantined, Superseded, Invalidated, or Rejected
///   are excluded from the live set even if no ValidityAssertion::Bound was appended
///   (disposition-based liveness filter — excludes non-live dispositions from the live set).
///
/// Returns a `FoldResult` with live claims in canonical order.
pub(crate) fn fold<F>(
    mut claims: Vec<Claim>,
    assertions_for: F,
    as_of_tx_time: DateTime<Utc>,
    valid_at_instant: Option<DateTime<Utc>>,
    config: &EngineConfig,
    latest_disposition: &HashMap<ClaimRef, Disposition>,
) -> FoldResult
where
    F: Fn(&ClaimRef) -> Vec<ValidityAssertion>,
{
    // Step 1 — deterministic sort by canonical ordering key (I8 arrival-independence).
    claims.sort_by(|a, b| {
        let ka = ordering_key(a, config);
        let kb = ordering_key(b, config);
        ka.cmp(&kb)
    });

    // Step 2 — evaluate liveness for each claim (transaction-time axis).
    // A claim is live if:
    //   (a) it is not bounded by a ValidityAssertion (tx-time visibility filter applied first), AND
    //   (b) its latest ledger disposition is NOT one of the non-live dispositions
    //       (Quarantined, Superseded, Invalidated, Rejected).
    let mut with_status: Vec<ClaimWithStatus> = claims
        .into_iter()
        .map(|c| {
            let last_disp = latest_disposition.get(c.claim_ref()).cloned();
            let disposition_live = last_disp
                .as_ref()
                .map(|d| !is_non_live_disposition(d))
                .unwrap_or(true); // no ledger entry = admitted (new claim before first write)
            let assertions = assertions_for(c.claim_ref());
            let assertion_live = is_claim_live(&assertions, as_of_tx_time);
            let live = assertion_live && disposition_live;
            ClaimWithStatus {
                claim: c,
                is_live: live,
                last_disposition: last_disp,
            }
        })
        .collect();

    // Step 3 — collect live claims in canonical order.
    let live_claims: Vec<ClaimWithStatus> = with_status
        .iter()
        .filter(|c| c.is_live)
        .cloned()
        .collect();

    // Step 4 — valid-time instant-selection for trusted successions (valid-time axis).
    //
    // D2 ordering: tx-time visibility filter (step 2) runs FIRST; only then is the
    // valid-time instant applied to narrow the live set.
    //
    // The instant to select against:
    //  - `valid_at_instant` when the caller supplies an explicit valid-time query point.
    //  - `as_of_tx_time` when no explicit instant is given (backward-compatible default).
    //
    // If ALL live claims form a trusted succession (each has valid_time_confidence >= threshold,
    // bounded start, and windows are pairwise non-overlapping), select the single claim whose
    // half-open window [start, end) contains the query instant.
    //
    // Boundary semantics: start inclusive, end exclusive. Open end (None) = "until further notice".
    // Gap (instant in no window) → empty live_claims → NoBelief.
    //
    // This fires BEFORE conflict detection (step 5) so that a true succession collapses
    // to a single claim and never reaches has_conflict=true.
    let vt_instant = valid_at_instant.unwrap_or(as_of_tx_time);
    let (live_claims, succession_selected) = if live_claims.len() > 1 {
        let live_claim_refs: Vec<&Claim> = live_claims.iter().map(|cs| &cs.claim).collect();
        if valid_time_helpers::is_trusted_succession(&live_claim_refs, config.valid_time_confidence_threshold) {
            // Select the single claim whose window contains the valid-time query instant.
            let selected = valid_time_helpers::select_by_valid_time_instant(&live_claim_refs, vt_instant);
            // Extract the claim_ref before dropping live_claim_refs (which borrows live_claims).
            let selected_ref = selected.map(|c| c.claim_ref().clone());
            drop(live_claim_refs); // release borrow on live_claims
            let narrowed: Vec<ClaimWithStatus> = match selected_ref {
                Some(ref cref) => live_claims.into_iter()
                    .filter(|cs| cs.claim.claim_ref() == cref)
                    .collect(),
                None => vec![], // gap → NoBelief
            };
            (narrowed, true)
        } else {
            (live_claims, false)
        }
    } else {
        (live_claims, false)
    };

    // Step 5 — conflict detection (I7 Contested first-class).
    // Two live claims on the same subject-line with different values = conflict.
    // For Functional cardinality, any 2+ live claims = conflict.
    // For SetValued, conflict only when values are identical but a MutualExclusion edge exists
    // (edge-level conflict detection is deferred to the projection layer which has edge data).
    // Here we detect the structural conflict: 2+ live claims with the same cardinality = Functional.
    // NOTE: if succession_selected=true, live_claims.len() is 0 or 1, so has_conflict=false always.
    let functional_live_count = live_claims
        .iter()
        .filter(|c| *c.claim.cardinality() == Cardinality::Functional)
        .count();
    let has_conflict = functional_live_count > 1 || (live_claims.len() > 1 && {
        // Multiple live claims on a subject-line that aren't clearly set-valued = conflict.
        // If all are SetValued we accept them as co-existing; otherwise conflict.
        live_claims.iter().any(|c| *c.claim.cardinality() != Cardinality::SetValued)
    });

    // Update the mutable with_status for completeness (not used beyond FoldResult here).
    for cs in &mut with_status {
        cs.is_live = live_claims.iter().any(|lc| lc.claim.claim_ref() == cs.claim.claim_ref());
    }

    FoldResult { live_claims, has_conflict, succession_selected }
}

// ── Build a Belief from a ClaimWithStatus ────────────────────────────────────

/// Convert a live `ClaimWithStatus` into a `Belief` value type.
/// Currency decay is NOT computed here — that is the Projection component's responsibility.
/// The `last_refreshed_at` is set to the claim's transaction_time as the baseline;
/// projection.rs will compute the actual decay state using `now`.
pub(crate) fn claim_to_belief(cs: &ClaimWithStatus) -> Belief {
    Belief {
        claim_ref: cs.claim.claim_ref().clone(),
        fact: cs.claim.fact().clone(),
        provenance: cs.claim.provenance().clone(),
        valid_time: cs.claim.valid_time().clone(),
        transaction_time: cs.claim.transaction_time().clone(),
        confidence: cs.claim.confidence().clone(),
        currency_signal: CurrencySignal {
            last_refreshed_at: cs.claim.transaction_time().clone(),
            state: CurrencyState::Fresh, // placeholder; projection.rs computes real state
            corroboration_count: 0,
        },
        criticality: cs.claim.criticality().clone(),
    }
}

/// Derive the `BeliefStatus` for a fold result.
/// `has_pending_review` is passed in from the projection layer.
pub(crate) fn fold_status(
    fold: &FoldResult,
    has_pending_review: bool,
) -> BeliefStatus {
    let _ = has_pending_review; // surfaced as Marker, not status
    if fold.live_claims.is_empty() {
        BeliefStatus::NoBelief
    } else if fold.has_conflict {
        // Contested is set when the caller knows about an unresolved external contradiction.
        // Default is Conflict; projection.rs upgrades to Contested when appropriate.
        BeliefStatus::Conflict
    } else if fold.live_claims.len() == 1 {
        let c = &fold.live_claims[0].claim;
        if c.valid_time().is_unknown() {
            BeliefStatus::TimingUncertain
        } else {
            BeliefStatus::Resolved
        }
    } else {
        // Multiple live, no conflict flag — set-valued, treat as Resolved (all values co-exist).
        BeliefStatus::Resolved
    }
}

/// Derive a `StalenessFlag` from the fold result (simple heuristic; full decay in projection.rs).
pub(crate) fn fold_staleness(fold: &FoldResult) -> StalenessFlag {
    if fold.live_claims.is_empty() {
        StalenessFlag { is_stale: true, reason: Some("no live claim on subject-line".into()) }
    } else {
        StalenessFlag { is_stale: false, reason: None }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EngineConfig;
    use chrono::Utc;
    use mempill_types::{
        AgentId, AssertionKind, Cardinality, ClaimRef, Confidence, ExternalAnchor, ExternalKind,
        Fact, ProvenanceLabel, TransactionTime, ValidTime, ValidityAssertion,
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Empty disposition map — used in tests where no ledger entries affect liveness.
    fn no_dispositions() -> std::collections::HashMap<ClaimRef, Disposition> {
        std::collections::HashMap::new()
    }

    fn agent() -> AgentId {
        AgentId("agent-1".into())
    }

    #[allow(clippy::too_many_arguments)]
    // reason: test helper mirrors the full Claim constructor; parameters cover orthogonal axes
    fn make_claim(
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
        value: serde_json::Value,
        tx_time: DateTime<Utc>,
        vt_start: Option<DateTime<Utc>>,
        vt_confidence: f32,
        cardinality: Cardinality,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent_id.clone(),
            Fact { subject: subject.into(), predicate: predicate.into(), value },
            cardinality,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx_time),
            ValidTime { start: vt_start, end: None, valid_time_confidence: vt_confidence , granularity: None},
            Confidence { value_confidence: 0.9, valid_time_confidence: vt_confidence },
            mempill_types::Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn no_assertions(_: &ClaimRef) -> Vec<ValidityAssertion> {
        vec![]
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    // ── FOLD DETERMINISM (I8): arrival-order independence ────────────────────

    /// Same claims in different insertion orders must produce the same canonical Belief.
    #[test]
    fn fold_determinism_i8_same_claims_different_order() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(10);
        let t2 = Utc::now() - chrono::Duration::hours(5);
        let t3 = Utc::now() - chrono::Duration::hours(1);

        let c1 = make_claim(&agent, "user", "name", serde_json::json!("Alice"), t1, None, 0.0, Cardinality::Functional);
        let c2 = make_claim(&agent, "user", "name", serde_json::json!("Bob"), t2, None, 0.0, Cardinality::Functional);
        let c3 = make_claim(&agent, "user", "name", serde_json::json!("Carol"), t3, None, 0.0, Cardinality::Functional);

        // Order A: [c1, c2, c3]
        let order_a = vec![c1.clone(), c2.clone(), c3.clone()];
        // Order B: [c3, c1, c2]
        let order_b = vec![c3.clone(), c1.clone(), c2.clone()];
        // Order C: [c2, c3, c1]
        let order_c = vec![c2.clone(), c3.clone(), c1.clone()];

        let disp = no_dispositions();
        let result_a = fold(order_a, no_assertions, now(), None, &config, &disp);
        let result_b = fold(order_b, no_assertions, now(), None, &config, &disp);
        let result_c = fold(order_c, no_assertions, now(), None, &config, &disp);

        // Live claims count must be identical regardless of input order.
        assert_eq!(result_a.live_claims.len(), result_b.live_claims.len(), "live count must be arrival-order independent");
        assert_eq!(result_b.live_claims.len(), result_c.live_claims.len(), "live count must be arrival-order independent");

        // The canonical ordering key (tx_time when vt_confidence < threshold) must be consistent.
        let refs_a: Vec<ClaimRef> = result_a.live_claims.iter().map(|c| c.claim.claim_ref().clone()).collect();
        let refs_b: Vec<ClaimRef> = result_b.live_claims.iter().map(|c| c.claim.claim_ref().clone()).collect();
        let refs_c: Vec<ClaimRef> = result_c.live_claims.iter().map(|c| c.claim.claim_ref().clone()).collect();
        assert_eq!(refs_a, refs_b, "I8: canonical order must be arrival-independent (A vs B)");
        assert_eq!(refs_b, refs_c, "I8: canonical order must be arrival-independent (B vs C)");
    }

    // ── ORDERING KEY: valid-time vs tx-time by confidence threshold ──────────

    /// A high-confidence valid-time claim (≥ 0.7 threshold) orders by valid_time_start.
    /// A low-confidence claim (< 0.7) falls back to tx_time.
    #[test]
    fn ordering_key_high_confidence_uses_valid_time() {
        let config = EngineConfig::default(); // threshold = 0.7
        let agent = agent();
        let tx_early = Utc::now() - chrono::Duration::hours(20);
        let tx_late = Utc::now() - chrono::Duration::hours(1);
        let vt_very_early = Utc::now() - chrono::Duration::days(365);

        // claim_a: tx_time=early, valid_time_start=very_early, high confidence → orders by vt
        let claim_a = make_claim(
            &agent, "user", "name", serde_json::json!("Alice"),
            tx_early, Some(vt_very_early), 0.9, Cardinality::Functional,
        );
        // claim_b: tx_time=late, valid_time_start=None, low confidence → falls back to tx_time
        let claim_b = make_claim(
            &agent, "user", "name", serde_json::json!("Bob"),
            tx_late, None, 0.3, Cardinality::Functional,
        );

        let key_a = ordering_key(&claim_a, &config);
        let key_b = ordering_key(&claim_b, &config);

        // claim_a's primary ordering key = vt_very_early (very old) → should sort before claim_b
        // claim_b's primary ordering key = tx_late (recent)
        assert!(
            key_a.0 < key_b.0,
            "high-confidence valid_time_start should be the ordering key for claim_a"
        );
    }

    /// A low-confidence valid-time claim falls back to tx_time ordering.
    #[test]
    fn ordering_key_low_confidence_uses_tx_time() {
        let config = EngineConfig::default(); // threshold = 0.7
        let agent = agent();
        let tx_time = Utc::now();
        let vt_future = tx_time + chrono::Duration::days(10); // would not be valid future start

        let claim = make_claim(
            &agent, "user", "city", serde_json::json!("Paris"),
            tx_time, Some(vt_future), 0.3, // low confidence
            Cardinality::Functional,
        );
        let key = ordering_key(&claim, &config);
        // Must use tx_time, not vt_future
        assert_eq!(key.0, tx_time, "low-confidence claim must use tx_time as ordering key");
    }

    // ── SUPERSESSION FOLD: bounded incumbent + newer claim ───────────────────

    /// An incumbent claim that is bounded (via ValidityAssertion::Bound) should not appear
    /// in live_claims. A newer claim without bounding should be the sole live claim.
    #[test]
    fn supersession_fold_bounded_incumbent_not_live() {
        let config = EngineConfig::default();
        let agent = agent();
        let t_old = Utc::now() - chrono::Duration::hours(5);
        let t_new = Utc::now() - chrono::Duration::hours(1);
        let bound_time = Utc::now() - chrono::Duration::hours(3);
        let query_now = now();

        let incumbent = make_claim(
            &agent, "user", "role", serde_json::json!("viewer"),
            t_old, None, 0.0, Cardinality::Functional,
        );
        let incumbent_ref = incumbent.claim_ref().clone();

        let newer = make_claim(
            &agent, "user", "role", serde_json::json!("admin"),
            t_new, None, 0.0, Cardinality::Functional,
        );
        let newer_ref = newer.claim_ref().clone();

        let claims = vec![incumbent, newer];

        // Build a Bound assertion for the incumbent.
        let bound_assertion = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: incumbent_ref.clone(),
            kind: AssertionKind::Bound { bound_at: bound_time },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_time),
        };
        let bound_ref = incumbent_ref.clone();

        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == bound_ref {
                vec![bound_assertion.clone()]
            } else {
                vec![]
            }
        };

        let result = fold(claims, assertions_fn, query_now, None, &config, &no_dispositions());

        // Incumbent should be bounded (not live); newer should be the sole live claim.
        assert_eq!(result.live_claims.len(), 1, "only the newer claim should be live");
        assert_eq!(
            *result.live_claims[0].claim.claim_ref(), newer_ref,
            "live claim should be the newer one"
        );

        // has_conflict = false (only one live Functional claim).
        assert!(!result.has_conflict, "no conflict when only one live claim remains");
    }

    /// Incumbent retained in history (non-destruction: writes are INSERT-only) — not deleted, just not live.
    #[test]
    fn supersession_fold_incumbent_retained_in_history() {
        let config = EngineConfig::default();
        let agent = agent();
        let t_old = Utc::now() - chrono::Duration::hours(5);
        let t_new = Utc::now() - chrono::Duration::hours(1);
        let bound_time = Utc::now() - chrono::Duration::hours(3);
        let query_now = now();

        let incumbent = make_claim(
            &agent, "user", "role", serde_json::json!("viewer"),
            t_old, None, 0.0, Cardinality::Functional,
        );
        let incumbent_ref = incumbent.claim_ref().clone();

        let newer = make_claim(
            &agent, "user", "role", serde_json::json!("admin"),
            t_new, None, 0.0, Cardinality::Functional,
        );

        let claims = vec![incumbent, newer];

        let bound_assertion = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: incumbent_ref.clone(),
            kind: AssertionKind::Bound { bound_at: bound_time },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_time),
        };
        let bound_ref = incumbent_ref.clone();

        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == bound_ref {
                vec![bound_assertion.clone()]
            } else {
                vec![]
            }
        };

        let result = fold(claims, assertions_fn, query_now, None, &config, &no_dispositions());

        // Total claims passed in = 2; live = 1; the other is bounded (not deleted).
        // The fold result only tracks live — history is provided by the persistence layer.
        // This test confirms the fold does NOT drop claims it receives; all 2 are processed.
        assert_eq!(result.live_claims.len(), 1, "one live; incumbent is bounded not deleted");
    }

    // ── CONTESTED: unresolved conflict surfaces Contested ────────────────────

    /// Two live Functional claims on the same subject-line → has_conflict = true.
    /// Projection.rs will surface this as BeliefStatus::Contested or Conflict.
    #[test]
    fn contested_two_live_functional_claims_has_conflict() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(5);
        let t2 = Utc::now() - chrono::Duration::hours(1);

        let c1 = make_claim(&agent, "user", "role", serde_json::json!("admin"), t1, None, 0.0, Cardinality::Functional);
        let c2 = make_claim(&agent, "user", "role", serde_json::json!("viewer"), t2, None, 0.0, Cardinality::Functional);

        let result = fold(vec![c1, c2], no_assertions, now(), None, &config, &no_dispositions());

        assert!(result.has_conflict, "two live Functional claims must produce has_conflict=true (I7)");
        assert_eq!(result.live_claims.len(), 2, "both live claims retained (I7 never silently picks)");
    }

    // ── SET-VALUED: multiple live set-valued claims are not a conflict ────────

    #[test]
    fn set_valued_multiple_live_not_conflict() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(5);
        let t2 = Utc::now() - chrono::Duration::hours(1);

        let c1 = make_claim(&agent, "user", "tag", serde_json::json!("rust"), t1, None, 0.0, Cardinality::SetValued);
        let c2 = make_claim(&agent, "user", "tag", serde_json::json!("python"), t2, None, 0.0, Cardinality::SetValued);

        let result = fold(vec![c1, c2], no_assertions, now(), None, &config, &no_dispositions());

        assert!(!result.has_conflict, "set-valued claims should not produce conflict");
        assert_eq!(result.live_claims.len(), 2, "both set-valued claims are live");
    }

    // ── BOUNDED + REOPENED: a reopened claim is live ──────────────────────────

    #[test]
    fn bound_then_reopen_claim_is_live() {
        let _config = EngineConfig::default();
        let agent = agent();
        let t0 = Utc::now() - chrono::Duration::hours(10);
        let bound_at = Utc::now() - chrono::Duration::hours(5);
        let reopen_at = Utc::now() - chrono::Duration::hours(2);

        let claim = make_claim(
            &agent, "user", "status", serde_json::json!("active"),
            t0, None, 0.0, Cardinality::Functional,
        );
        let claim_ref = claim.claim_ref().clone();

        let bound = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_at),
        };
        let reopen = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Reopen { reopen_at },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(reopen_at),
        };

        let assertions = vec![bound, reopen];
        let is_live = is_claim_live(&assertions, now());

        assert!(is_live, "a claim that was bounded then reopened should be live");
    }

    // ── "now" injection: different now values yield different live sets ────────

    /// Passing two different `now` values must yield different decay/liveness results.
    /// This verifies no system clock calls inside the fold (determinism contract).
    #[test]
    fn now_injection_different_now_yields_different_liveness() {
        let config = EngineConfig::default();
        let agent = agent();

        let t0 = Utc::now() - chrono::Duration::hours(10);
        let bound_at_future_relative_to_past_now = Utc::now() - chrono::Duration::hours(3);

        let claim = make_claim(
            &agent, "user", "city", serde_json::json!("Berlin"),
            t0, None, 0.0, Cardinality::Functional,
        );
        let claim_ref = claim.claim_ref().clone();

        let bound = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at: bound_at_future_relative_to_past_now },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_at_future_relative_to_past_now),
        };

        let assertions = vec![bound];
        let assertions_fn_clone = assertions.clone();

        let disp = no_dispositions();
        // Query in the far PAST (before the bound)
        let past_now = t0 + chrono::Duration::hours(1);
        let result_past = fold(
            vec![claim.clone()],
            |_| assertions_fn_clone.clone(),
            past_now,
            None, // valid_at_instant: None = backward-compatible
            &config,
            &disp,
        );

        // Query in the present (after the bound)
        let present_now = now();
        let result_present = fold(
            vec![claim],
            |_| assertions.clone(),
            present_now,
            None, // valid_at_instant: None = backward-compatible
            &config,
            &disp,
        );

        // At past_now (before bound), claim should be live.
        assert_eq!(result_past.live_claims.len(), 1, "claim should be live before the bound");
        // At present_now (after bound), claim should not be live.
        assert_eq!(result_present.live_claims.len(), 0, "claim should be bounded at present");
    }

    // ── W1a — valid_at_instant parameter: D2 independence tests ──────────────

    /// Helper: create a claim with a specific trusted valid-time window (confidence=0.9, above threshold).
    fn make_vt_claim(
        agent_id: &AgentId,
        value: serde_json::Value,
        tx_time: DateTime<Utc>,
        vt_start: DateTime<Utc>,
        vt_end: Option<DateTime<Utc>>,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent_id.clone(),
            Fact { subject: "subject".into(), predicate: "predicate".into(), value },
            Cardinality::Functional,
            mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            mempill_types::ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx_time),
            mempill_types::ValidTime { start: Some(vt_start), end: vt_end, valid_time_confidence: 0.9, granularity: None },
            mempill_types::Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            mempill_types::Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    /// valid_at_instant=None preserves existing behavior: instant-selection uses as_of_tx_time.
    ///
    /// A two-claim succession [Jan–Mar) [Mar–∞) with as_of=Apr should select the second claim.
    #[test]
    fn valid_at_instant_none_uses_as_of_tx_time_for_selection() {
        let config = EngineConfig::default();
        let agent = agent();
        use chrono::TimeZone;

        let jan1  = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let feb1  = chrono::Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap();
        let mar1  = chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let apr1  = chrono::Utc.with_ymd_and_hms(2024, 4, 1, 0, 0, 0).unwrap();

        // tx times before the as_of point so both are tx-visible.
        let tx_early = jan1 - chrono::Duration::days(1);
        let tx_mid   = feb1;

        let c1 = make_vt_claim(&agent, serde_json::json!("first"),  tx_early, jan1, Some(mar1));
        let c2 = make_vt_claim(&agent, serde_json::json!("second"), tx_mid,   mar1, None);

        // as_of_tx_time = Apr 1 → April is in c2's window [Mar, ∞).
        let fold = fold(
            vec![c1.clone(), c2.clone()],
            no_assertions,
            apr1,
            None, // valid_at_instant=None → use as_of_tx_time (Apr) for selection
            &config,
            &no_dispositions(),
        );

        assert!(fold.succession_selected, "should be succession");
        assert_eq!(fold.live_claims.len(), 1, "should select one claim");
        assert_eq!(
            fold.live_claims[0].claim.fact().value,
            serde_json::json!("second"),
            "None: as_of (Apr) is in second claim's window"
        );
    }

    /// valid_at_instant=Some overrides the selection axis independently of tx-time.
    ///
    /// Succession [Jan–Mar) [Mar–∞). as_of_tx_time=Apr (both tx-visible), valid_at=Feb.
    /// D2: tx filter first (both pass), then select by valid_at=Feb → first claim.
    #[test]
    fn valid_at_instant_some_selects_independently_of_tx_time() {
        let config = EngineConfig::default();
        let agent = agent();
        use chrono::TimeZone;

        let jan1  = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let feb1  = chrono::Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap();
        let mar1  = chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let apr1  = chrono::Utc.with_ymd_and_hms(2024, 4, 1, 0, 0, 0).unwrap();

        let tx_early = jan1 - chrono::Duration::days(1);
        let tx_mid   = jan1 + chrono::Duration::days(10);

        let c1 = make_vt_claim(&agent, serde_json::json!("first"),  tx_early, jan1, Some(mar1));
        let c2 = make_vt_claim(&agent, serde_json::json!("second"), tx_mid,   mar1, None);

        // as_of=Apr → both tx-visible. valid_at=Feb → should select c1 (Feb in [Jan, Mar)).
        let fold = fold(
            vec![c1.clone(), c2.clone()],
            no_assertions,
            apr1,
            Some(feb1), // explicit valid-time instant: Feb 1 → first window
            &config,
            &no_dispositions(),
        );

        assert!(fold.succession_selected, "should be succession");
        assert_eq!(fold.live_claims.len(), 1, "should select one claim");
        assert_eq!(
            fold.live_claims[0].claim.fact().value,
            serde_json::json!("first"),
            "valid_at=Feb selects the first window [Jan, Mar)"
        );
    }

    /// valid_at_instant=Some with a gap returns NoBelief (empty live_claims).
    #[test]
    fn valid_at_instant_gap_returns_no_belief() {
        let config = EngineConfig::default();
        let agent = agent();
        use chrono::TimeZone;

        let jan1  = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let mar1  = chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let may1  = chrono::Utc.with_ymd_and_hms(2024, 5, 1, 0, 0, 0).unwrap();
        let apr1  = chrono::Utc.with_ymd_and_hms(2024, 4, 1, 0, 0, 0).unwrap();
        let query = chrono::Utc.with_ymd_and_hms(2024, 12, 1, 0, 0, 0).unwrap();

        let tx = jan1 - chrono::Duration::days(1);
        // A = [Jan, Mar),  B = [May, ∞) — gap in Apr
        let c1 = make_vt_claim(&agent, serde_json::json!("a"), tx, jan1, Some(mar1));
        let c2 = make_vt_claim(&agent, serde_json::json!("b"), tx, may1, None);

        let fold = fold(
            vec![c1, c2],
            no_assertions,
            query,      // tx as_of = Dec → both tx-visible
            Some(apr1), // valid_at = Apr → in the gap
            &config,
            &no_dispositions(),
        );

        assert!(fold.succession_selected, "gap still counts as trusted succession attempt");
        assert_eq!(fold.live_claims.len(), 0, "gap → NoBelief");
        assert!(!fold.has_conflict, "gap must not produce has_conflict");
    }

    /// D2 ordering: validity-assertion (tx-time axis) filter runs BEFORE valid-at selection.
    ///
    /// C1 is bounded (via ValidityAssertion::Bound) at a time AFTER the query as_of_tx_time,
    /// so the Bound is NOT yet visible — c1 remains live at as_of. C2 is live.
    /// Both form a trusted succession. valid_at_instant=Feb is in c1's window [Jan, Mar).
    /// D2 confirmed: tx-time assertion filter first (both live), then valid_at narrows to c1.
    #[test]
    fn d2_tx_time_filter_runs_before_valid_at_selection() {
        let config = EngineConfig::default();
        let agent = agent();
        use chrono::TimeZone;

        let jan1  = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let feb1  = chrono::Utc.with_ymd_and_hms(2024, 2, 1, 0, 0, 0).unwrap();
        let mar1  = chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let may1  = chrono::Utc.with_ymd_and_hms(2024, 5, 1, 0, 0, 0).unwrap();
        let dec1  = chrono::Utc.with_ymd_and_hms(2024, 12, 1, 0, 0, 0).unwrap();

        let tx = jan1 - chrono::Duration::days(1);

        // c1: valid_time [Jan, Mar), c2: valid_time [Mar, ∞) — non-overlapping succession.
        let c1 = make_vt_claim(&agent, serde_json::json!("first"),  tx, jan1, Some(mar1));
        let c2 = make_vt_claim(&agent, serde_json::json!("second"), tx, mar1, None);
        let c1_ref = c1.claim_ref().clone();

        // A Bound assertion for c1 asserted at May (after query as_of=Feb).
        // At query time=Feb, this Bound is NOT yet visible → c1 remains live.
        let bound = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: AgentId("agent-1".into()),
            target_claim: c1_ref.clone(),
            kind: AssertionKind::Bound { bound_at: may1 },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(may1), // asserted at May → not visible at Feb
        };

        let assertions_fn = {
            let c1_ref = c1_ref.clone();
            let bound = bound.clone();
            move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
                if cr == &c1_ref { vec![bound.clone()] } else { vec![] }
            }
        };

        // as_of=Feb: Bound for c1 is at May → not visible → both c1 and c2 pass tx-filter.
        // valid_at=Feb → in c1's window [Jan, Mar).
        // D2: assertions filtered first (both live at Feb), then valid_at selects c1.
        let fold_result = fold(
            vec![c1.clone(), c2.clone()],
            assertions_fn,
            feb1,        // as_of_tx_time: Feb → Bound(May) invisible → both live
            Some(feb1),  // valid_at: Feb → selects first window [Jan, Mar)
            &config,
            &no_dispositions(),
        );

        assert!(fold_result.succession_selected, "trusted succession should be detected");
        assert_eq!(fold_result.live_claims.len(), 1, "valid_at=Feb selects one claim");
        assert_eq!(
            fold_result.live_claims[0].claim.fact().value,
            serde_json::json!("first"),
            "D2: tx assertion filter first (both pass at Feb), then valid_at narrows to c1"
        );
        assert!(!fold_result.has_conflict, "succession → no conflict");

        // Verify the other axis: at as_of=Dec (after Bound), c1 is bounded → only c2 live.
        let assertions_fn2 = {
            let c1_ref = c1_ref.clone();
            move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
                if cr == &c1_ref { vec![bound.clone()] } else { vec![] }
            }
        };
        let fold_dec = fold(
            vec![c1, c2],
            assertions_fn2,
            dec1,        // as_of=Dec → Bound(May) visible → c1 bounded
            Some(feb1),  // valid_at=Feb (in c1's window, but c1 is now bounded)
            &config,
            &no_dispositions(),
        );
        // c1 is bounded at Dec view → only c2 live (not a succession anymore, single claim)
        assert_eq!(fold_dec.live_claims.len(), 1);
        assert_eq!(
            fold_dec.live_claims[0].claim.fact().value,
            serde_json::json!("second"),
            "at Dec view (Bound visible), c1 is bounded; c2 is the only live claim"
        );
    }
}
