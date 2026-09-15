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
    CurrencyState, DateGranularity, Disposition, StalenessFlag, ValidityAssertion,
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
    /// Narrowed by step 4 valid-time instant-selection when a trusted succession is detected.
    pub live_claims: Vec<ClaimWithStatus>,
    /// True when ≥ 2 live claims conflict on the same subject-line without resolution.
    pub has_conflict: bool,
    /// True when valid-time instant-selection was applied: the live_claims set was narrowed
    /// from a trusted succession to a single claim matching the query instant via valid-time instant-selection.
    /// When true, `has_conflict` is always false and `live_claims.len()` is 0 or 1.
    pub succession_selected: bool,
    /// Full sorted claim set (canonical ordering key) with RAW liveness flags —
    /// tx-time visibility (`is_claim_live`) AND disposition filter only (step 2 below),
    /// captured BEFORE step 4's succession-narrowing.
    ///
    /// Deliberately RAW, not the narrowed `live_claims` set: consumers that need to know
    /// "is this the currently-selected succession member" derive that separately via an
    /// O(1) membership check against `live_claims` — this field is the single source of
    /// truth for "was this claim ever excluded by an explicit Bound/disposition", used by
    /// both the write-path succession classifier (checks a challenger against every raw-live
    /// claim, not just the narrowed current one) and the history read-path (which must show
    /// every claim's own window, honest liveness, and conflict signal).
    pub all_claims: Vec<ClaimWithStatus>,
}

/// A claim with its resolved live/bounded status at the fold's `as_of_tx_time`.
#[derive(Debug, Clone)]
pub(crate) struct ClaimWithStatus {
    pub claim: Claim,
    /// True if this claim is NOT bounded (or has been reopened) at `as_of_tx_time`.
    pub is_live: bool,
    /// The disposition recorded in the last ledger entry for this claim, if known.
    pub last_disposition: Option<Disposition>,
    /// The `bound_at` of this claim's active `AssertionKind::Bound`, if any, at
    /// `as_of_tx_time` (TASK-33 E2). `None` when the claim is currently open (never
    /// bounded, or bounded then reopened). Consumed by `compute_history_windows` so a
    /// host-asserted bound on an otherwise open-ended claim narrows its displayed
    /// `valid_until` and overlap classification — the claim's own stored row is never
    /// touched (I1); this is a read-time derivation only.
    pub bound_at: Option<DateTime<Utc>>,
    /// The display-precision granularity of `bound_at`, if known (TASK-33-W5-LIB-R2,
    /// DIAG-5). Mirrors `bound_at`: `None` when the claim is open, or when the active
    /// `Bound`'s own `bound_at_granularity` is `None` (legacy row, or a Deny's tx_time
    /// fallback with no tracked precision). Consumed by `narrowed_valid_end` so a
    /// bound-derived end can carry its true precision instead of silently rendering as
    /// full-instant/day precision.
    pub bound_at_granularity: Option<DateGranularity>,
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
pub(crate) fn ordering_key(claim: &Claim, config: &EngineConfig) -> (DateTime<Utc>, DateTime<Utc>, u128) {
    let primary = if claim.valid_time().valid_time_confidence >= config.valid_time_confidence_threshold {
        claim.valid_time().start.unwrap_or(claim.transaction_time().0)
    } else {
        claim.transaction_time().0
    };
    (primary, claim.transaction_time().0, claim.claim_ref().0.as_u128())
}

// ── Validity resolution ───────────────────────────────────────────────────────

/// The state of an active `AssertionKind::Bound` on a claim, evaluated at a tx-time cutoff.
///
/// Returned by [`active_bound_at`] — the SINGLE SOURCE OF TRUTH for the Bound/Reopen toggle
/// walk (TASK-33-W4-LIB-R1 review #1). Previously `is_claim_live`, `active_bound_at`, and
/// `assert_validity::latest_active_bound` each re-implemented this walk independently; they
/// now all delegate to one function so the toggle logic can never drift between the read path
/// (liveness, history windows) and the write path (idempotency / single-writer-per-target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoundState {
    /// The valid-time instant at which the claim's validity ends (the `bound_at` of the
    /// currently active `Bound` assertion).
    pub bound_at: DateTime<Utc>,
    /// The `assertion_ref` of the currently active `Bound` assertion — consumed by
    /// `assert_validity.rs` for idempotency / single-writer-per-target checks (gate 4).
    pub assertion_ref: uuid::Uuid,
    /// The display-precision granularity of `bound_at`, if known (TASK-33-W5-LIB-R2).
    pub bound_at_granularity: Option<DateGranularity>,
}

/// Evaluate the active `Bound`, if any, at `as_of_tx_time` given the full set of validity
/// assertions for a claim — SINGLE SOURCE OF TRUTH for the toggle walk (see [`BoundState`]).
///
/// Rules (non-destructive: no deletes; fixed-history monotone: liveness is monotone for fixed history):
///   - A `Bound` assertion with `bound_at <= as_of_tx_time` closes the claim (sets the state).
///   - A subsequent `Reopen` with `reopen_at <= as_of_tx_time` re-opens it (clears the state).
///   - Assertions are processed in chronological order of their `asserted_at` timestamp
///     (ties broken by `assertion_ref` UUID, I8 deterministic total order).
///   - Only assertions with `asserted_at <= as_of_tx_time` are visible (bi-temporal rule).
///   - The final state after processing all visible assertions is returned.
///
/// Returns `None` when the claim is currently open (never bounded, or bounded then reopened,
/// as of `as_of_tx_time`); `Some(BoundState)` when a `Bound` is active.
///
/// Consumed by:
/// - [`is_claim_live`] — delegates: `active_bound_at(...).is_none()`.
/// - `fold` (this module) — needs `.bound_at` to narrow `compute_history_windows`.
/// - `assert_validity::latest_active_bound` — needs both `.bound_at` and `.assertion_ref`.
pub(crate) fn active_bound_at(
    assertions: &[ValidityAssertion],
    as_of_tx_time: DateTime<Utc>,
) -> Option<BoundState> {
    // Sort by asserted_at ascending for deterministic processing (I8).
    let mut sorted: Vec<&ValidityAssertion> = assertions.iter().collect();
    sorted.sort_by(|a, b| {
        a.asserted_at.0.cmp(&b.asserted_at.0)
            .then(a.assertion_ref.cmp(&b.assertion_ref)) // UUID tiebreaker for I8
    });

    let mut state: Option<BoundState> = None;
    for assertion in sorted {
        // Only assertions at or before as_of_tx_time are visible (bi-temporal rule).
        if assertion.asserted_at.0 > as_of_tx_time {
            continue;
        }
        match &assertion.kind {
            AssertionKind::Bound { bound_at, bound_at_granularity } => {
                if *bound_at <= as_of_tx_time {
                    state = Some(BoundState {
                        bound_at: *bound_at,
                        assertion_ref: assertion.assertion_ref,
                        bound_at_granularity: *bound_at_granularity,
                    });
                }
            }
            AssertionKind::Reopen { reopen_at } => {
                if *reopen_at <= as_of_tx_time {
                    state = None;
                }
            }
            // AssertionKind is #[non_exhaustive] — future assertion kinds are ignored (conservative: treat as no-op).
            _ => {}
        }
    }
    state
}

/// Evaluate whether a claim is live at `as_of_tx_time` given the full set of
/// validity assertions for that claim.
///
/// Delegates to [`active_bound_at`] (the single source of truth for the toggle walk,
/// TASK-33-W4-LIB-R1 review #1): a claim is live iff there is no active `Bound`.
pub(crate) fn is_claim_live(
    assertions: &[ValidityAssertion],
    as_of_tx_time: DateTime<Utc>,
) -> bool {
    active_bound_at(assertions, as_of_tx_time).is_none()
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
    let with_status: Vec<ClaimWithStatus> = claims
        .into_iter()
        .map(|c| {
            let last_disp = latest_disposition.get(c.claim_ref()).cloned();
            let disposition_live = last_disp
                .as_ref()
                .map(|d| !is_non_live_disposition(d))
                .unwrap_or(true); // no ledger entry = admitted (new claim before first write)
            let assertions = assertions_for(c.claim_ref());
            // PERF (TASK-33-W4-LIB-R1 review #5): a single call to `active_bound_at` (one
            // sort + one walk of `assertions`) now yields BOTH the liveness flag and the
            // bound instant — before the review's single-source-of-truth refactor this was
            // two separate calls (`is_claim_live` + `active_bound_at`), each re-sorting and
            // re-walking the same assertion slice per claim.
            let active_bound = active_bound_at(&assertions, as_of_tx_time);
            let assertion_live = active_bound.is_none();
            let bound_at = active_bound.map(|s| s.bound_at);
            let bound_at_granularity = active_bound.and_then(|s| s.bound_at_granularity);
            let live = assertion_live && disposition_live;
            ClaimWithStatus {
                claim: c,
                is_live: live,
                last_disposition: last_disp,
                bound_at,
                bound_at_granularity,
            }
        })
        .collect();

    // Capture the RAW (pre-narrowing) sorted claim set with tx-time + disposition liveness
    // flags for `FoldResult.all_claims` — this is the single source of truth consumed by both
    // the write-path succession classifier (reconciler.rs) and the history read-path
    // (query_history.rs). Must be captured HERE, before step 4 narrows `live_claims`.
    let all_claims = with_status.clone();

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

    FoldResult { live_claims, has_conflict, succession_selected, all_claims }
}

// ── Shared effective-window derivation (I8: ONE narrowing computation) ────────

/// Narrow a claim's own `valid_time.end` by an active host-asserted `Bound` (TASK-33-W5-LIB A).
///
/// `min(own_end, bound_at)` — a `Bound` only ever NARROWS a claim's displayed/classified
/// window; it never widens a narrower stated end. When the claim has no own end at all, the
/// bound instant becomes its effective end outright.
///
/// THE SINGLE SOURCE OF TRUTH for this narrowing: consumed by [`compute_history_windows`]
/// (history read-path), [`narrow_live_claims_for_valid_at`] (valid_at read-path, via
/// [`claim_with_effective_window`]), and the ingest/reconcile conflict-candidate widening
/// (`ingest_claim.rs::build_conflict_candidate_claims`, `reconcile.rs`) — so a claim's
/// "believed-then" window can never drift between history, valid_at, and conflict detection.
pub(crate) fn narrowed_valid_end(
    claim: &Claim,
    bound_at: Option<DateTime<Utc>>,
    bound_at_granularity: Option<DateGranularity>,
) -> (Option<DateTime<Utc>>, Option<DateGranularity>) {
    let raw_own_end = claim.valid_time().end;
    let raw_own_end_gran = claim.valid_time().end_granularity;
    match (raw_own_end, bound_at) {
        (Some(oe), Some(ba)) if ba < oe => (Some(ba), bound_at_granularity),
        (Some(oe), _) => (Some(oe), raw_own_end_gran),
        (None, Some(ba)) => (Some(ba), bound_at_granularity),
        (None, None) => (None, None),
    }
}

/// `true` when [`narrowed_valid_end`] would return the BOUND instant rather than the claim's
/// own `valid_time.end` — i.e. the displayed end is bound-derived and its granularity comes
/// from the `ValidityAssertion`, not from the claim row. Mirrors `narrowed_valid_end`'s arms
/// one-for-one and lives next to it so the two cannot drift.
///
/// Note the `ba < oe` (strict) condition: a bound landing exactly ON the claim's own end is a
/// no-effect bound — the end still belongs to the CLAIM, with the claim's own precision.
pub(crate) fn end_is_bound_derived(claim: &Claim, bound_at: Option<DateTime<Utc>>) -> bool {
    match (claim.valid_time().end, bound_at) {
        (Some(oe), Some(ba)) => ba < oe,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// Build a transient, READ-TIME-ONLY `Claim` clone whose `valid_time.end` is narrowed via
/// [`narrowed_valid_end`] — never written back to storage (I1: the stored row is never
/// touched). Consumed wherever a bound-narrowed claim must be compared/selected using the
/// exact same half-open-window primitives (`valid_time_helpers::is_trusted_succession`,
/// `select_by_valid_time_instant`) that operate on `&Claim` — so a bound-narrowed window and
/// an own-end window can never classify differently (I8).
pub(crate) fn claim_with_effective_window(cs: &ClaimWithStatus) -> Claim {
    let (end, end_granularity) = narrowed_valid_end(&cs.claim, cs.bound_at, cs.bound_at_granularity);
    let vt = mempill_types::ValidTime { end, end_granularity, ..cs.claim.valid_time().clone() };
    Claim::new(
        cs.claim.claim_ref().clone(),
        cs.claim.agent_id().clone(),
        cs.claim.fact().clone(),
        cs.claim.cardinality().clone(),
        cs.claim.provenance().clone(),
        cs.claim.external_anchor().clone(),
        cs.claim.transaction_time().clone(),
        vt,
        cs.claim.confidence().clone(),
        cs.claim.criticality().clone(),
        cs.claim.derived_from().to_vec(),
        cs.claim.metadata().cloned(),
        cs.claim.snapshot_schema_version(),
    )
}

/// Dispositions that mean "never validly believed at any instant" — hard-excluded from the
/// valid_at bound-reentry candidate set (and from the ingest/reconcile conflict-candidate
/// widening) regardless of any active Bound. Distinct from [`is_non_live_disposition`]: this
/// list deliberately OMITS `Superseded` — a Superseded-via-Bound claim is exactly the case
/// that MAY re-enter (see [`narrow_live_claims_for_valid_at`]'s rustdoc for the full rule).
fn is_hard_excluded_disposition(d: &Disposition) -> bool {
    matches!(d, Disposition::Quarantined | Disposition::Invalidated | Disposition::Rejected)
}

/// Re-derive `FoldResult.live_claims` (+ `succession_selected`/`has_conflict`) for an EXPLICIT
/// valid-time query instant (TASK-33-W5-LIB A, DIAG-4 finding A). Called by the read-path
/// (`query_memory.rs`, and transitively `query_subject.rs`) immediately after [`fold`], ONLY
/// when the caller supplied a `valid_at` — `fold`'s own step 3/4 output is otherwise used
/// unchanged (the `valid_at = None` path is untouched by this function and by `fold` itself).
///
/// ## Why a separate pass instead of editing `fold`'s step 4 in place
///
/// `fold`'s own step 4 (raw-live-only, `len() > 1`-guarded) stays the single source of truth
/// for every OTHER caller (ingest, reconcile, the 500+ existing tests that never pass
/// `valid_at`). This function fully RE-DERIVES the valid-time selection from `FoldResult.all_claims`
/// (the RAW, pre-narrowing set — always available regardless of what `fold` selected), so it can
/// never disagree with `fold`'s own default-instant narrowing for the cases `fold` already gets
/// right (e.g. an all-live, non-bounded succession) — it only differs where `fold`'s raw-live-only
/// view is insufficient (a candidate excluded only by a Bound; a single live candidate that
/// needs a window check).
///
/// ## The candidate set (I3/I7 point-in-time correctness)
///
/// A claim is a valid_at candidate iff:
///   1. Its latest disposition is NOT hard-excluded ([`is_hard_excluded_disposition`]:
///      Quarantined / Invalidated / Rejected — "never validly believed").
///   2. It is currently live (`is_live`, unconditional — no window check needed to know it was
///      once live for windowing purposes), OR
///   3. It is excluded ONLY by an active Bound (`bound_at.is_some()`) AND it is not in
///      `denied_via_adjudication` — i.e. the Bound closed a claim that WAS genuinely believed
///      ("bound-by-succession/affirm/host"): an Affirm's losing incumbent, a host
///      `end_fact`/`assert_validity` closure, or any other succession-style Bound. A claim
///      bounded via an oracle `Deny` verdict (the challenger was found WRONG, and per
///      `submit_adjudication.rs` was `QueuedForAdjudication` — never live — for its entire
///      existence before being denied) is EXCLUDED — it was never a "believed then" state, and
///      re-entering it would let rejected content resurface as a point-in-time answer. See
///      `build_denied_via_adjudication_set` (`ingest_claim.rs`) for the derivation.
///
/// Each candidate's window is narrowed via [`claim_with_effective_window`] (the SAME derivation
/// `compute_history_windows` uses — I8), then the widened, narrowed candidate set is tested with
/// the EXACT SAME primitives `fold` itself uses (`is_trusted_succession` +
/// `select_by_valid_time_instant`) — dropping `fold`'s `len() > 1` guard: even a single candidate
/// is window-tested, so an instant outside its window (e.g. before its start) correctly yields
/// NoBelief instead of unconditionally returning it.
///
/// If the widened candidate set is NOT a clean trusted succession (untrusted claims, or a
/// genuine unresolved overlap even after narrowing), this function falls back to whatever
/// `fold` already computed for `live_claims`/`has_conflict` — i.e. behaves as if the valid_at
/// axis could not cleanly resolve, preserving `fold`'s existing Conflict/Contested semantics.
pub(crate) fn narrow_live_claims_for_valid_at(
    fold_result: FoldResult,
    valid_at_instant: DateTime<Utc>,
    denied_via_adjudication: &std::collections::HashSet<ClaimRef>,
    config: &EngineConfig,
) -> FoldResult {
    let candidates: Vec<ClaimWithStatus> = fold_result
        .all_claims
        .iter()
        .filter(|cs| {
            let hard_excluded = cs
                .last_disposition
                .as_ref()
                .map(is_hard_excluded_disposition)
                .unwrap_or(false);
            if hard_excluded {
                return false;
            }
            cs.is_live
                || (cs.bound_at.is_some() && !denied_via_adjudication.contains(cs.claim.claim_ref()))
        })
        .cloned()
        .collect();

    let narrowed_claims: Vec<Claim> = candidates.iter().map(claim_with_effective_window).collect();
    let candidate_refs: Vec<&Claim> = narrowed_claims.iter().collect();

    if valid_time_helpers::is_trusted_succession(&candidate_refs, config.valid_time_confidence_threshold) {
        let selected = valid_time_helpers::select_by_valid_time_instant(&candidate_refs, valid_at_instant);
        let selected_ref = selected.map(|c| c.claim_ref().clone());
        let live_claims: Vec<ClaimWithStatus> = match selected_ref {
            Some(ref cref) => candidates.into_iter().filter(|cs| cs.claim.claim_ref() == cref).collect(),
            None => vec![], // gap → NoBelief
        };
        FoldResult {
            live_claims,
            has_conflict: false, // succession-selected result is never Conflict (0 or 1 claim)
            succession_selected: true,
            all_claims: fold_result.all_claims,
        }
    } else {
        // Could not cleanly resolve the widened set — fall back to fold()'s own (unwidened)
        // live_claims/has_conflict, preserving existing Conflict/Contested semantics.
        fold_result
    }
}

/// Build the WIDENED conflict-candidate set consumed by `reconciler::classify_conflict`'s
/// N-wide succession check (TASK-33-W5-LIB C/D, DIAG-4 finding C — "retroactive claim over a
/// closed historical window commits silently").
///
/// Prior behavior (`ingest_claim.rs`/`reconcile.rs`) built the candidate set from raw-live
/// claims ONLY (`FoldResult.all_claims` filtered `is_live`), so a claim explicitly closed via
/// `end_fact`/`assert_validity`/Affirm was never compared against a new retroactive write —
/// letting an overlapping retroactive claim commit silently against an ended incumbent instead
/// of being contested. The candidate set is now raw-live ∪ bound-narrowed claims: every claim
/// not hard-excluded ([`is_hard_excluded_disposition`]: Quarantined/Invalidated/Rejected) AND
/// not Deny-bounded (`denied_via_adjudication`), narrowed to its effective window via
/// [`claim_with_effective_window`] — the SAME derivation [`compute_history_windows`] and
/// [`narrow_live_claims_for_valid_at`] use (I8).
///
/// Deny-bounded claims are EXCLUDED here too (not merely "conservative to include" — a
/// rejected challenger's `bound_at` is tx_time, an essentially arbitrary instant with no
/// relation to any OTHER claim's valid-time window; leaving a denied claim's phantom
/// [challenger_start, tx_time) window in the candidate set can spuriously overlap a LATER,
/// entirely unrelated, genuinely non-overlapping succession member and misclassify it as
/// SameLineConflict — confirmed by a regression: Diane/Linda/Sam(denied)/John, where Sam's
/// denied window overlapping John's real succession against Linda incorrectly forced John to
/// QueuedForAdjudication). Same discriminator [`narrow_live_claims_for_valid_at`] uses.
///
/// A clean succession is preserved: after `end_fact(A at e)`, a claim `B` starting exactly at
/// `e` does NOT overlap `A`'s narrowed window `[s, e)` (half-open touching boundary) — `B` still
/// commits cheaply. Only a claim that genuinely overlaps the ended window is caught.
///
/// Complexity: O(N) over `all_claims` (already loaded for the fold) — zero extra DB round trips.
pub(crate) fn build_conflict_candidate_claims(
    fold_result: &FoldResult,
    denied_via_adjudication: &std::collections::HashSet<ClaimRef>,
) -> Vec<Claim> {
    fold_result
        .all_claims
        .iter()
        .filter(|cs| {
            let hard_excluded = cs
                .last_disposition
                .as_ref()
                .map(is_hard_excluded_disposition)
                .unwrap_or(false);
            if hard_excluded {
                return false;
            }
            if cs.is_live {
                return true;
            }
            // "raw-live ∪ bound-narrowed": a not-live claim only widens in when it has an
            // ACTIVE Bound (`bound_at.is_some()`) — a claim excluded purely by ledger
            // disposition (e.g. a legacy direct-write Superseded entry with no accompanying
            // ValidityAssertion) carries no known valid-time end to narrow to, so
            // `claim_with_effective_window` would be a no-op and silently re-admit it as a
            // phantom OPEN-ENDED candidate — exactly the regression this guard prevents
            // (dscope[wa1]: a claim superseded by ledger-only entry, no Bound assertion, must
            // stay excluded from every candidate set, matching `is_live` exactly). A
            // Deny-bounded claim (rejected, never believed) also stays excluded regardless of
            // its `bound_at` — see this function's rustdoc.
            cs.bound_at.is_some() && !denied_via_adjudication.contains(cs.claim.claim_ref())
        })
        .map(|cs| if cs.is_live { cs.claim.clone() } else { claim_with_effective_window(cs) })
        .collect()
}

// ── Build a Belief from a ClaimWithStatus ────────────────────────────────────

/// Convert a live `ClaimWithStatus` into a `Belief` value type.
/// Currency decay is NOT computed here — that is the Projection component's responsibility.
/// The `last_refreshed_at` is set to the claim's transaction_time as the baseline;
/// projection.rs will compute the actual decay state using `now`.
///
/// GRANULARITY NOTE (DISPLAY-ONLY): `valid_time` is cloned verbatim from the claim,
/// so `start_granularity` and `end_granularity` are preserved here.  Granularity is
/// purely a display hint — it does NOT influence fold selection, matching, or ordering.
pub(crate) fn claim_to_belief(cs: &ClaimWithStatus) -> Belief {
    claim_to_belief_raw(&cs.claim)
}

/// Convert any `Claim` into a `Belief` value type — the same derivation `claim_to_belief`
/// uses, but callable on a bare `&Claim` (no `ClaimWithStatus` wrapper needed). Consumed by
/// `ingest_claim.rs`/`reconcile.rs` to build the reconciler's `incumbent: Option<&Belief>`
/// from the WIDENED conflict-candidate set (`build_conflict_candidate_claims`, TASK-33-W5-LIB
/// C/D) — that set is `Vec<Claim>`, not `Vec<ClaimWithStatus>`, since bound-narrowed candidates
/// are synthetic read-time-only claims ([`claim_with_effective_window`]) with no `ClaimWithStatus`
/// of their own.
pub(crate) fn claim_to_belief_raw(claim: &Claim) -> Belief {
    Belief {
        claim_ref: claim.claim_ref().clone(),
        fact: claim.fact().clone(),
        provenance: claim.provenance().clone(),
        valid_time: claim.valid_time().clone(),
        transaction_time: claim.transaction_time().clone(),
        confidence: claim.confidence().clone(),
        currency_signal: CurrencySignal {
            last_refreshed_at: claim.transaction_time().clone(),
            state: CurrencyState::Fresh, // placeholder; projection.rs computes real state
            corroboration_count: 0,
        },
        criticality: claim.criticality().clone(),
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

// ── History windows (engine layer — single source of truth for query_history) ─

/// Effective window for one `HistoryEntry` slot, computed by [`compute_history_windows`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HistoryWindow {
    /// Effective end of this entry's valid-time window (see module docs for derivation).
    pub valid_until: Option<DateTime<Utc>>,
    /// Granularity of whichever timestamp produced `valid_until` (own end, or successor's start).
    pub valid_until_granularity: Option<DateGranularity>,
    /// True when this entry is part of a structural (`has_conflict`) or pairwise valid-time
    /// overlap — the single Contested signal query_history surfaces.
    pub contested: bool,
    /// True when `now` falls within `[valid_from, valid_until)` (half-open; open ends always match).
    pub contains_now: bool,
}

/// The primary ordering-key component + its display granularity, for one claim.
///
/// Mirrors `ordering_key`'s primary-component rule exactly (valid_time_start when confidence
/// meets the threshold AND start is present, else transaction_time), but additionally tracks
/// the honest source granularity — `None` whenever the value came from a transaction-time
/// fallback (no user-supplied date precision) or the claim's own confidence is below threshold.
fn primary_key_and_granularity(claim: &Claim, config: &EngineConfig) -> (DateTime<Utc>, Option<DateGranularity>) {
    let vt = claim.valid_time();
    if vt.valid_time_confidence >= config.valid_time_confidence_threshold {
        match vt.start {
            Some(s) => (s, vt.start_granularity),
            None => (claim.transaction_time().0, None),
        }
    } else {
        (claim.transaction_time().0, None)
    }
}

/// Compute the effective history window for every entry in `all_claims` (already sorted by the
/// canonical ordering key — the SAME sort `fold` itself performs).
///
/// PURE, engine-layer: reuses `valid_time_helpers::claim_is_trusted` / `windows_non_overlapping`
/// — the SAME primitives `fold`'s own succession narrowing uses — so history windows and belief
/// selection can never drift (I8 single source of truth).
///
/// Semantics (see `mempill-core` TASK-33 architecture docs for the full derivation):
/// - Own `valid_time.end` is honoured whenever present; it is narrowed by a successor's
///   ordering key only via `min()`, never discarded outright.
/// - A trusted pairwise OVERLAP with the (skip-duplicate) successor marks BOTH the window
///   (own end, not narrowed) and `contested = true` — no silent narrowing over a real conflict.
/// - `contested` also incorporates the fold-wide `has_conflict` signal for any RAW-live entry —
///   the single conflict signal in the system (I7), not re-derived here.
/// - Duplicate adjacent ordering keys (same displayed instant) are skipped: the "successor" for
///   window purposes is always the next entry with a STRICTLY later primary key, eliminating
///   zero-length windows without a schema change.
pub(crate) fn compute_history_windows(
    all_claims: &[ClaimWithStatus],
    has_conflict: bool,
    now: DateTime<Utc>,
    config: &EngineConfig,
) -> Vec<HistoryWindow> {
    let threshold = config.valid_time_confidence_threshold;
    let n = all_claims.len();
    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let claim = &all_claims[i].claim;
        let (my_key, _) = primary_key_and_granularity(claim, config);

        // Skip-duplicate successor lookup: next entry with a STRICTLY later primary key.
        let succ_idx = ((i + 1)..n).find(|&j| {
            let (jk, _) = primary_key_and_granularity(&all_claims[j].claim, config);
            jk > my_key
        });

        // Own end, narrowed by an active host-asserted Bound (TASK-33 E2), if any.
        //
        // A `Bound` never touches the claim's stored row (I1) — this is a read-time-only
        // derivation, mirroring how a successor's ordering key narrows via `min()` below.
        // Bound only NARROWS (min()); it never widens a narrower stated end. When the claim
        // has no own end at all, the bound instant becomes its effective end outright — this
        // is exactly what closes DIAG-3 (an open-ended incumbent explicitly bounded via
        // `assert_validity`/`end_fact` must display + fold as ended at that instant, not stay
        // open forever). A bound-derived end now carries the bounding instant's OWN
        // granularity (`ClaimWithStatus.bound_at_granularity`, TASK-33-W5-LIB-R2, DIAG-5) —
        // an Affirm sets it from the winning challenger's `start_granularity`;
        // `assert_validity`/`end_fact` set it from the parsed `at`'s granularity; Deny and
        // legacy rows leave it `None` (honest absence, never fabricated).
        //
        // Delegates to `narrowed_valid_end` — the SAME narrowing `narrow_live_claims_for_valid_at`
        // (valid_at read-path) and the ingest/reconcile conflict-candidate widening use
        // (TASK-33-W5-LIB, I8: one narrowing computation, no parallel algorithms).
        let bound_at = all_claims[i].bound_at;
        let bound_at_gran = all_claims[i].bound_at_granularity;
        let (own_end, own_end_gran) = narrowed_valid_end(claim, bound_at, bound_at_gran);

        let (valid_until, valid_until_granularity, pairwise_overlap) = match succ_idx {
            None => (own_end, own_end_gran, false),
            Some(j) => {
                let succ = &all_claims[j].claim;
                let (succ_key, succ_gran) = primary_key_and_granularity(succ, config);
                let both_trusted = valid_time_helpers::claim_is_trusted(claim, threshold)
                    && valid_time_helpers::claim_is_trusted(succ, threshold);
                // Overlap is evaluated against the BOUND-ADJUSTED own_end, not the raw claim
                // (the raw, untouched row would otherwise still see a Bound incumbent as
                // open-ended and misreport overlap, exactly DIAG-3's root cause).
                // `claim_is_trusted` guarantees `.start` is `Some` on both sides whenever
                // `both_trusted` is true, so the `.expect()`s below never unwrap a `None`.
                //
                // Delegates to `valid_time_helpers::windows_non_overlapping_bounds` — the SAME
                // primitive `is_trusted_succession`'s `windows_non_overlapping` uses
                // (TASK-33-W4-LIB-R1 review #2) — so a bound-narrowed window and an own-end
                // window classify identically; no hand-copied comparison here.
                let overlapping = both_trusted
                    && !valid_time_helpers::windows_non_overlapping_bounds(
                        claim.valid_time().start.expect("both_trusted guarantees Some"),
                        own_end,
                        succ.valid_time().start.expect("both_trusted guarantees Some"),
                        succ.valid_time().end,
                    );
                // TASK-33-W5-LIB-R2 (DIAG-5): when the displayed end is BOUND-DERIVED, the
                // bound carries no granularity of its own (a legacy `ValidityAssertion`, or a
                // Deny closure that stamped only an instant), and it numerically coincides
                // with the successor's start instant, attribute the successor's start
                // granularity to it instead of silently downgrading to full-instant precision.
                //
                // Two hard restrictions (TASK-33-W5-LIB-R2 review F4):
                // - Bound-derived ONLY. A claim's own `valid_time.end` never takes the
                //   successor's granularity: whoever wrote that claim supplied its end
                //   precision, so `end_granularity == None` there is a deliberate "instant",
                //   not missing information. `end_is_bound_derived` gates exactly this.
                // - `own_end_gran == None` only — an honestly-tracked precision (from the
                //   bound itself) is never overridden.
                //
                // Not applied in the `overlapping` arm: overlap means the windows genuinely
                // cross, so `own_end == succ_key` cannot hold under the half-open comparison
                // that produced `overlapping` — the fallback would be dead code there, and
                // leaving it in would blur that invariant.
                let bound_derived_no_gran =
                    own_end_gran.is_none() && end_is_bound_derived(claim, bound_at);
                let gran_at_succ_key = |oe: DateTime<Utc>, g: Option<DateGranularity>| {
                    g.or_else(|| {
                        if bound_derived_no_gran && oe == succ_key { succ_gran } else { None }
                    })
                };
                match own_end {
                    Some(oe) if overlapping => (Some(oe), own_end_gran, true),
                    Some(oe) => {
                        if oe <= succ_key {
                            (Some(oe), gran_at_succ_key(oe, own_end_gran), false)
                        } else {
                            (Some(succ_key), succ_gran, false)
                        }
                    }
                    None if overlapping => (None, None, true),
                    None => (Some(succ_key), succ_gran, false),
                }
            }
        };

        let contested = pairwise_overlap || (all_claims[i].is_live && has_conflict);

        let start_ok = claim.valid_time().start.is_none_or(|s| now >= s);
        let end_ok = valid_until.is_none_or(|vu| now < vu);
        let contains_now = start_ok && end_ok;

        out.push(HistoryWindow { valid_until, valid_until_granularity, contested, contains_now });
    }

    out
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
            ValidTime { start: vt_start, end: None, valid_time_confidence: vt_confidence , start_granularity: None, end_granularity: None},
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

    // ── HISTORY WINDOW GRANULARITY (TASK-33-W5-LIB-R2, F4) ───────────────────

    /// Claim with an explicit valid_time window, optionally bound at read time.
    fn windowed(
        start: DateTime<Utc>,
        end: Option<DateTime<Utc>>,
        start_gran: Option<DateGranularity>,
        end_gran: Option<DateGranularity>,
        bound_at: Option<DateTime<Utc>>,
        bound_at_granularity: Option<DateGranularity>,
    ) -> ClaimWithStatus {
        let claim = Claim::new(
            ClaimRef::new_random(),
            agent(),
            Fact { subject: "gran-corp".into(), predicate: "ceo".into(), value: serde_json::json!("x") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(start),
            ValidTime {
                start: Some(start),
                end,
                valid_time_confidence: 0.9,
                start_granularity: start_gran,
                end_granularity: end_gran,
            },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            mempill_types::Criticality::Medium,
            vec![],
            None,
            None,
        );
        ClaimWithStatus {
            claim,
            is_live: true,
            last_disposition: None,
            bound_at,
            bound_at_granularity,
        }
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(secs, 0).unwrap()
    }

    /// A claim's OWN `valid_time.end` never inherits the successor's start granularity, even
    /// when the two instants coincide: `end_granularity == None` on the claim row means the
    /// writer supplied instant precision, not "unknown" (F4).
    #[test]
    fn own_end_equal_to_successor_key_keeps_its_own_granularity() {
        let start_a = ts(1_577_836_800); // 2020-01-01
        let start_b = ts(1_609_459_200); // 2021-01-01
        let claims = vec![
            windowed(start_a, Some(start_b), None, None, None, None),
            windowed(start_b, None, Some(DateGranularity::Year), None, None, None),
        ];
        let windows = compute_history_windows(&claims, false, ts(1_700_000_000), &EngineConfig::default());
        assert_eq!(windows[0].valid_until, Some(start_b));
        assert_eq!(
            windows[0].valid_until_granularity, None,
            "a claim's own end must keep its own (instant) precision, never the successor's"
        );
    }

    /// The DIAG-5 fallback still fires for a BOUND-DERIVED end whose bound carries no
    /// granularity and whose instant coincides with the successor's start key.
    #[test]
    fn bound_derived_end_equal_to_successor_key_inherits_successor_granularity() {
        let start_a = ts(1_577_836_800); // 2020-01-01
        let start_b = ts(1_609_459_200); // 2021-01-01
        let claims = vec![
            windowed(start_a, None, None, None, Some(start_b), None),
            windowed(start_b, None, Some(DateGranularity::Year), None, None, None),
        ];
        let windows = compute_history_windows(&claims, false, ts(1_700_000_000), &EngineConfig::default());
        assert_eq!(windows[0].valid_until, Some(start_b));
        assert_eq!(
            windows[0].valid_until_granularity,
            Some(DateGranularity::Year),
            "a granularity-less bound landing on the successor's start adopts its precision"
        );
    }

    /// A bound that carries its OWN granularity is never overridden by the successor's.
    #[test]
    fn bound_with_own_granularity_is_not_overridden() {
        let start_a = ts(1_577_836_800);
        let start_b = ts(1_609_459_200);
        let claims = vec![
            windowed(start_a, None, None, None, Some(start_b), Some(DateGranularity::Month)),
            windowed(start_b, None, Some(DateGranularity::Year), None, None, None),
        ];
        let windows = compute_history_windows(&claims, false, ts(1_700_000_000), &EngineConfig::default());
        assert_eq!(windows[0].valid_until_granularity, Some(DateGranularity::Month));
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
            kind: AssertionKind::Bound { bound_at: bound_time, bound_at_granularity: None },
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
            kind: AssertionKind::Bound { bound_at: bound_time, bound_at_granularity: None },
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
            kind: AssertionKind::Bound { bound_at, bound_at_granularity: None },
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

    // ── TASK-33-W4-LIB-R1 review #1: is_claim_live / active_bound_at agreement ──

    /// Exhaustively enumerate short Bound/Reopen toggle sequences (asserted_at strictly
    /// increasing) and assert `is_claim_live(...) == active_bound_at(...).is_none()` at every
    /// tx-time cutoff that matters (before / exactly at / just after each assertion) — proves
    /// the two functions can never drift now that `is_claim_live` delegates to
    /// `active_bound_at` (single source of truth, TASK-33-W4-LIB-R1 review #1).
    #[test]
    fn is_claim_live_agrees_with_active_bound_at_over_all_toggle_sequences() {
        let agent = agent();
        let claim_ref = ClaimRef::new_random();
        let base = Utc::now() - chrono::Duration::days(1);

        for len in 0..=4u32 {
            let combos = 1u32 << len; // 2^len toggle-kind patterns (bit i: 0=Bound, 1=Reopen)
            for pattern in 0..combos {
                let mut assertions = Vec::new();
                let mut cutoffs = vec![base - chrono::Duration::minutes(1)]; // before everything
                for i in 0..len {
                    let t = base + chrono::Duration::hours(i as i64);
                    let is_bound = (pattern >> i) & 1 == 0;
                    let kind = if is_bound {
                        AssertionKind::Bound { bound_at: t, bound_at_granularity: None }
                    } else {
                        AssertionKind::Reopen { reopen_at: t }
                    };
                    assertions.push(ValidityAssertion {
                        assertion_ref: uuid::Uuid::new_v4(),
                        agent_id: agent.clone(),
                        target_claim: claim_ref.clone(),
                        kind,
                        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                        confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
                        asserted_at: TransactionTime(t),
                    });
                    cutoffs.push(t); // exactly at this assertion's asserted_at
                    cutoffs.push(t + chrono::Duration::minutes(1)); // just after
                }

                for cutoff in &cutoffs {
                    let live = is_claim_live(&assertions, *cutoff);
                    let bound_state = active_bound_at(&assertions, *cutoff);
                    assert_eq!(
                        live,
                        bound_state.is_none(),
                        "is_claim_live/active_bound_at disagreed: pattern={pattern:#b} len={len} cutoff={cutoff:?}"
                    );
                }
            }
        }
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
            kind: AssertionKind::Bound { bound_at: bound_at_future_relative_to_past_now, bound_at_granularity: None },
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
            mempill_types::ValidTime { start: Some(vt_start), end: vt_end, valid_time_confidence: 0.9, start_granularity: None, end_granularity: None },
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

    // ── TASK-33-W5-LIB A: narrow_live_claims_for_valid_at — DIAG-4 scenarios ─────

    fn empty_denied() -> std::collections::HashSet<ClaimRef> {
        std::collections::HashSet::new()
    }

    fn bound_assertion_for(
        agent_id: &AgentId,
        target: &ClaimRef,
        bound_at: DateTime<Utc>,
        asserted_at: DateTime<Utc>,
    ) -> ValidityAssertion {
        ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent_id.clone(),
            target_claim: target.clone(),
            kind: AssertionKind::Bound { bound_at, bound_at_granularity: None },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(asserted_at),
        }
    }

    /// DIAG-4 scenario 1: Diane open [2021-04, ∞) explicitly ended (end_fact) at 2025-01;
    /// John [2025-01, ∞) live. valid_at BEFORE the bound (2022-06, 2024-12) must still return
    /// Diane (the bound-narrowed reentry); valid_at AFTER (2025-06) returns John.
    #[test]
    fn narrow_live_claims_for_valid_at_reenters_end_fact_bounded_incumbent() {
        use chrono::TimeZone;
        let config = EngineConfig::default();
        let agent = agent();

        let diane_start = chrono::Utc.with_ymd_and_hms(2021, 4, 1, 0, 0, 0).unwrap();
        let bound_at = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let tx = diane_start - chrono::Duration::days(1);

        let diane = make_vt_claim(&agent, serde_json::json!("diane"), tx, diane_start, None);
        let diane_ref = diane.claim_ref().clone();
        let john = make_vt_claim(&agent, serde_json::json!("john"), tx, bound_at, None);

        let assertion = bound_assertion_for(&agent, &diane_ref, bound_at, bound_at);
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == diane_ref { vec![assertion.clone()] } else { vec![] }
        };

        let as_of = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let claims = vec![diane.clone(), john.clone()];

        for (label, valid_at, expect) in [
            ("2022-06 (in Diane's window, before bound)", chrono::Utc.with_ymd_and_hms(2022, 6, 1, 0, 0, 0).unwrap(), "diane"),
            ("2024-12 (still in Diane's window)", chrono::Utc.with_ymd_and_hms(2024, 12, 1, 0, 0, 0).unwrap(), "diane"),
            ("2025-06 (in John's window)", chrono::Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap(), "john"),
        ] {
            let fold_result = fold(claims.clone(), assertions_fn.clone(), as_of, Some(valid_at), &config, &no_dispositions());
            let narrowed = narrow_live_claims_for_valid_at(fold_result, valid_at, &empty_denied(), &config);
            assert_eq!(narrowed.live_claims.len(), 1, "{label}: exactly one claim selected");
            assert_eq!(
                narrowed.live_claims[0].claim.fact().value,
                serde_json::json!(expect),
                "{label}: expected {expect}"
            );
        }
    }

    /// DIAG-4 scenario 2 (control): both claims bounded AT INGEST (own valid_time.end set,
    /// no ValidityAssertion involved) — must behave identically to scenario 1's reentry path.
    #[test]
    fn narrow_live_claims_for_valid_at_control_bounded_at_ingest() {
        use chrono::TimeZone;
        let config = EngineConfig::default();
        let agent = agent();

        let start = chrono::Utc.with_ymd_and_hms(2021, 4, 1, 0, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let tx = start - chrono::Duration::days(1);

        let diane = make_vt_claim(&agent, serde_json::json!("diane"), tx, start, Some(end));
        let john = make_vt_claim(&agent, serde_json::json!("john"), tx, end, None);
        let as_of = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let valid_at = chrono::Utc.with_ymd_and_hms(2022, 6, 1, 0, 0, 0).unwrap();

        let fold_result = fold(vec![diane, john], no_assertions, as_of, Some(valid_at), &config, &no_dispositions());
        let narrowed = narrow_live_claims_for_valid_at(fold_result, valid_at, &empty_denied(), &config);
        assert_eq!(narrowed.live_claims.len(), 1);
        assert_eq!(narrowed.live_claims[0].claim.fact().value, serde_json::json!("diane"));
    }

    /// DIAG-4 scenario 3 (Affirm): the losing incumbent (Diane) is bounded at the winning
    /// challenger's (Joan) valid-time start (TASK-33-W5-LIB B fix). valid_at BEFORE that
    /// instant must still return Diane, not the winner.
    #[test]
    fn narrow_live_claims_for_valid_at_reenters_affirm_bounded_incumbent() {
        use chrono::TimeZone;
        let config = EngineConfig::default();
        let agent = agent();

        let diane_start = chrono::Utc.with_ymd_and_hms(2021, 4, 1, 0, 0, 0).unwrap();
        let joan_start = chrono::Utc.with_ymd_and_hms(2024, 9, 1, 0, 0, 0).unwrap(); // Affirm bound_at
        let tx = diane_start - chrono::Duration::days(1);

        let diane = make_vt_claim(&agent, serde_json::json!("diane"), tx, diane_start, None);
        let diane_ref = diane.claim_ref().clone();
        let joan = make_vt_claim(&agent, serde_json::json!("joan"), tx, joan_start, None);

        // Simulates submit_adjudication's Affirm bound_claim: bound_at = Joan's valid_time.start.
        let assertion = bound_assertion_for(&agent, &diane_ref, joan_start, joan_start);
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == diane_ref { vec![assertion.clone()] } else { vec![] }
        };

        let as_of = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let valid_at = chrono::Utc.with_ymd_and_hms(2022, 6, 1, 0, 0, 0).unwrap(); // before Joan starts

        let fold_result = fold(vec![diane, joan], assertions_fn, as_of, Some(valid_at), &config, &no_dispositions());
        let narrowed = narrow_live_claims_for_valid_at(fold_result, valid_at, &empty_denied(), &config);
        assert_eq!(narrowed.live_claims.len(), 1, "valid_at=2022-06 must select exactly one claim");
        assert_eq!(
            narrowed.live_claims[0].claim.fact().value,
            serde_json::json!("diane"),
            "valid_at=2022-06 (before the Affirm bound at Joan's start) must return diane"
        );
    }

    /// DIAG-4 scenario 6: a single open, unbounded claim (Eve [2010, ∞)) with a valid_at
    /// BEFORE its start must return NoBelief, not the claim unconditionally (the `len() > 1`
    /// guard drop).
    #[test]
    fn narrow_live_claims_for_valid_at_single_claim_pre_history_gap() {
        use chrono::TimeZone;
        let config = EngineConfig::default();
        let agent = agent();

        let eve_start = chrono::Utc.with_ymd_and_hms(2010, 1, 1, 0, 0, 0).unwrap();
        let tx = eve_start - chrono::Duration::days(1);
        let eve = make_vt_claim(&agent, serde_json::json!("eve"), tx, eve_start, None);

        let as_of = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let valid_at = chrono::Utc.with_ymd_and_hms(1995, 1, 1, 0, 0, 0).unwrap(); // before Eve's start

        let fold_result = fold(vec![eve], no_assertions, as_of, Some(valid_at), &config, &no_dispositions());
        let narrowed = narrow_live_claims_for_valid_at(fold_result, valid_at, &empty_denied(), &config);
        assert_eq!(narrowed.live_claims.len(), 0, "valid_at before Eve's only window must be NoBelief");
        assert!(narrowed.succession_selected);
        assert!(!narrowed.has_conflict);
    }

    /// A Deny-bounded challenger must NEVER re-enter the valid_at candidate set — it was
    /// `QueuedForAdjudication` (never live) for its entire existence before being denied, so
    /// it was never "believed then". Fixed-end incumbent [2021, 2024) + a denied challenger
    /// claiming [2024, ∞) (bounded at 2026 by the Deny): valid_at=2025 (inside the denied
    /// claim's raw window, after the incumbent's real end) must be NoBelief, not the rejected
    /// challenger's value.
    #[test]
    fn narrow_live_claims_for_valid_at_excludes_denied_challenger() {
        use chrono::TimeZone;
        let config = EngineConfig::default();
        let agent = agent();

        let incumbent_start = chrono::Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap();
        let incumbent_end = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let deny_bound_at = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(); // Deny keeps tx_time
        let tx = incumbent_start - chrono::Duration::days(1);

        let incumbent = make_vt_claim(&agent, serde_json::json!("incumbent"), tx, incumbent_start, Some(incumbent_end));
        let challenger = make_vt_claim(&agent, serde_json::json!("rejected"), tx, incumbent_end, None);
        let challenger_ref = challenger.claim_ref().clone();

        let assertion = bound_assertion_for(&agent, &challenger_ref, deny_bound_at, deny_bound_at);
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == challenger_ref { vec![assertion.clone()] } else { vec![] }
        };

        let mut denied = std::collections::HashSet::new();
        denied.insert(challenger.claim_ref().clone());

        let as_of = chrono::Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
        let valid_at = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(); // inside the denied claim's raw window

        let fold_result = fold(vec![incumbent, challenger], assertions_fn, as_of, Some(valid_at), &config, &no_dispositions());
        let narrowed = narrow_live_claims_for_valid_at(fold_result, valid_at, &denied, &config);
        assert_eq!(
            narrowed.live_claims.len(), 0,
            "a Deny-bounded (rejected) claim must never resurface as a valid_at belief"
        );
    }

    /// as_of_tx_time interplay: an `as_of` BEFORE the Bound was asserted sees the incumbent as
    /// raw-live (Bound not tx-visible yet); an `as_of` AFTER sees it bounded (reentry kicks in).
    /// A valid_at instant INSIDE the incumbent's real (eventually-bounded) window must resolve
    /// to the SAME claim in both cases — D2 independence holds even with bound-reentry.
    #[test]
    fn narrow_live_claims_for_valid_at_as_of_tx_time_interplay() {
        use chrono::TimeZone;
        let config = EngineConfig::default();
        let agent = agent();

        let alice_start = chrono::Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let bound_at = chrono::Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap(); // valid-time end
        let asserted_at = chrono::Utc.with_ymd_and_hms(2023, 6, 1, 0, 0, 0).unwrap(); // tx-time of the end_fact write
        let tx = alice_start - chrono::Duration::days(1);

        let alice = make_vt_claim(&agent, serde_json::json!("alice"), tx, alice_start, None);
        let alice_ref = alice.claim_ref().clone();
        let assertion = bound_assertion_for(&agent, &alice_ref, bound_at, asserted_at);
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == alice_ref { vec![assertion.clone()] } else { vec![] }
        };

        let valid_at = chrono::Utc.with_ymd_and_hms(2021, 6, 1, 0, 0, 0).unwrap(); // inside [2020, 2022)

        // Query A: as_of BEFORE asserted_at → Bound not tx-visible → Alice raw-live, unbounded.
        let as_of_before = chrono::Utc.with_ymd_and_hms(2022, 6, 1, 0, 0, 0).unwrap();
        let fold_before = fold(vec![alice.clone()], assertions_fn.clone(), as_of_before, Some(valid_at), &config, &no_dispositions());
        let narrowed_before = narrow_live_claims_for_valid_at(fold_before, valid_at, &empty_denied(), &config);
        assert_eq!(narrowed_before.live_claims.len(), 1, "before the bound: valid_at in-window still selects Alice");
        assert_eq!(narrowed_before.live_claims[0].claim.fact().value, serde_json::json!("alice"));

        // Query B: as_of AFTER asserted_at → Bound tx-visible → Alice bounded; reentry narrows
        // to [2020, 2022) — the SAME in-window instant still selects Alice.
        let as_of_after = chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let fold_after = fold(vec![alice], assertions_fn, as_of_after, Some(valid_at), &config, &no_dispositions());
        let narrowed_after = narrow_live_claims_for_valid_at(fold_after, valid_at, &empty_denied(), &config);
        assert_eq!(narrowed_after.live_claims.len(), 1, "after the bound: valid_at in-window still selects Alice via reentry");
        assert_eq!(narrowed_after.live_claims[0].claim.fact().value, serde_json::json!("alice"));
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
            kind: AssertionKind::Bound { bound_at: may1, bound_at_granularity: None },
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
