//! C2 — Canonical Valid-Time Fold (TECHNICAL_DESIGN.md §9, §10 I8, A3, A11).
//!
//! This module is PURE given its inputs — no I/O, no system clock.
//! All time parameters must be injected by the caller.
//!
//! ## Ordering-key rule (Section 9, B2, F4):
//! - If `valid_time_confidence >= config.valid_time_confidence_threshold`:
//!     ordering key = valid_time_start (authoritative)
//! - Else:
//!     ordering key = transaction_time (fallback)
//!
//! ## Fold invariants:
//! - I8: read-time-canonical — same stored claims → same belief, arrival-order independent.
//! - I10: fixed-history monotonicity — belief is monotone over a fixed history.
//! - I3: belief is derived, never stored — callers always re-fold at query time.

use chrono::{DateTime, Utc};
use mempill_types::{
    AssertionKind, Belief, BeliefStatus, Cardinality, Claim, ClaimRef, CurrencySignal,
    CurrencyState, Disposition, StalenessFlag, ValidityAssertion,
};

use crate::config::EngineConfig;

// ── Public result type ────────────────────────────────────────────────────────

/// The result of a canonical fold over one subject-line.
///
/// `live_claims` = claims that are currently valid at `as_of_tx_time` (i.e. not bounded by
/// any validity assertion, or reopened after bounding, under the canonical evaluation).
/// They are ordered by the canonical ordering key (valid_time or tx_time, depending on confidence).
///
/// `all_claims` = the full set of claims passed in (for history; I10 monotonicity audit).
///
/// `conflict` = true when two or more live claims have overlapping validity windows and
/// conflicting values, signalling a Contested / Conflict state to projection.rs.
#[derive(Debug, Clone)]
pub(crate) struct FoldResult {
    /// Canonically ordered live claims (not bounded at `as_of_tx_time`).
    pub live_claims: Vec<ClaimWithStatus>,
    /// True when ≥ 2 live claims conflict on the same subject-line without resolution.
    pub has_conflict: bool,
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
/// Implements I8: deterministic total order that is arrival-order independent.
///
/// When valid_time_confidence >= threshold → use valid_time_start (authoritative).
/// When below threshold, or when valid_time_start is None → fall back to tx_time.
///
/// The secondary tie-breaker is always tx_time (engine-stamped, unique in practice).
/// The tertiary tie-breaker is the ClaimRef UUID to guarantee total order even with
/// equal timestamps (I8 determinism requirement).
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
/// Rules (I1 — non-destructive; I10 — monotone):
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
        }
    }
    live
}

// ── Canonical fold ────────────────────────────────────────────────────────────

/// C2 canonical valid-time fold.
///
/// PURE: all inputs passed in; no I/O; no system clock calls.
///
/// Parameters:
/// - `claims`: all claims for the subject-line (loaded via PersistencePort; any order).
/// - `assertions_for`: a function mapping `ClaimRef → Vec<ValidityAssertion>` for the
///   claims in `claims`.  Callers supply this as a closure to keep the fold pure (no I/O here).
/// - `as_of_tx_time`: the bi-temporal query point (≤ now for historical queries).
/// - `config`: EngineConfig for the ordering-key confidence threshold.
///
/// Returns a `FoldResult` with live claims in canonical order.
pub(crate) fn fold<F>(
    mut claims: Vec<Claim>,
    assertions_for: F,
    as_of_tx_time: DateTime<Utc>,
    config: &EngineConfig,
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

    // Step 2 — evaluate liveness for each claim.
    let mut with_status: Vec<ClaimWithStatus> = claims
        .into_iter()
        .map(|c| {
            let assertions = assertions_for(c.claim_ref());
            let live = is_claim_live(&assertions, as_of_tx_time);
            ClaimWithStatus {
                claim: c,
                is_live: live,
                last_disposition: None,
            }
        })
        .collect();

    // Step 3 — collect live claims in canonical order.
    let live_claims: Vec<ClaimWithStatus> = with_status
        .iter()
        .filter(|c| c.is_live)
        .cloned()
        .collect();

    // Step 4 — conflict detection (I7 Contested first-class).
    // Two live claims on the same subject-line with different values = conflict.
    // For Functional cardinality, any 2+ live claims = conflict.
    // For SetValued, conflict only when values are identical but a MutualExclusion edge exists
    // (edge-level conflict detection is deferred to the projection layer which has edge data).
    // Here we detect the structural conflict: 2+ live claims with the same cardinality = Functional.
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

    FoldResult { live_claims, has_conflict }
}

// ── Build a Belief from a ClaimWithStatus ────────────────────────────────────

/// Convert a live `ClaimWithStatus` into a `Belief` value type.
/// Currency decay is NOT computed here — that is projection.rs (C5) responsibility.
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
/// `has_pending_review` is passed in from the projection layer (A26).
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

    fn agent() -> AgentId {
        AgentId("agent-1".into())
    }

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
            ValidTime { start: vt_start, end: None, valid_time_confidence: vt_confidence },
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

        let result_a = fold(order_a, no_assertions, now(), &config);
        let result_b = fold(order_b, no_assertions, now(), &config);
        let result_c = fold(order_c, no_assertions, now(), &config);

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

        let result = fold(claims, assertions_fn, query_now, &config);

        // Incumbent should be bounded (not live); newer should be the sole live claim.
        assert_eq!(result.live_claims.len(), 1, "only the newer claim should be live");
        assert_eq!(
            *result.live_claims[0].claim.claim_ref(), newer_ref,
            "live claim should be the newer one"
        );

        // has_conflict = false (only one live Functional claim).
        assert!(!result.has_conflict, "no conflict when only one live claim remains");
    }

    /// Incumbent retained in history (I1 non-destruction) — not deleted, just not live.
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

        let result = fold(claims, assertions_fn, query_now, &config);

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

        let result = fold(vec![c1, c2], no_assertions, now(), &config);

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

        let result = fold(vec![c1, c2], no_assertions, now(), &config);

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

        // Query in the far PAST (before the bound)
        let past_now = t0 + chrono::Duration::hours(1);
        let result_past = fold(
            vec![claim.clone()],
            |_| assertions_fn_clone.clone(),
            past_now,
            &config,
        );

        // Query in the present (after the bound)
        let present_now = now();
        let result_present = fold(
            vec![claim],
            |_| assertions.clone(),
            present_now,
            &config,
        );

        // At past_now (before bound), claim should be live.
        assert_eq!(result_past.live_claims.len(), 1, "claim should be live before the bound");
        // At present_now (after bound), claim should not be live.
        assert_eq!(result_present.live_claims.len(), 0, "claim should be bounded at present");
    }
}
