//! Currency-Aware Belief Projection.
//!
//! Builds a [`BeliefProjection`] from a [`FoldResult`] by applying:
//!   - Currency decay: based on `(now - last_refreshed_at)` vs `EngineConfig` thresholds.
//!   - Contested surfacing: when the fold has an unresolved conflict + external contradiction.
//!   - PendingReview markers: by inspecting `DependentFlaggedPendingReview` ledger entries.
//!
//! CONTRACT: "now" MUST be injected as a parameter — DO NOT read the system clock here.

use chrono::{DateTime, Utc};
use mempill_types::{
    Belief, BeliefProjection, BeliefStatus, ClaimRef, CurrencySignal, CurrencyState, Disposition,
    LedgerEntry, LedgerEventKind, Marker, StalenessFlag,
};

use crate::config::EngineConfig;
use crate::engine::truth_engine::{claim_to_belief, fold_staleness, FoldResult};

// ── Currency decay ────────────────────────────────────────────────────────────

/// Compute the `CurrencyState` for a claim given `now` and the `EngineConfig` thresholds.
///
/// `last_refreshed_at` = the claim's last provenance-independent restatement (v0.1: tx_time).
/// Decision:
///   - days_since_refresh < aging_unconfirmed_threshold_days → `Fresh`
///   - days_since_refresh < decayed_threshold_days → `AgingUnconfirmed`
///   - else → `Decayed`
pub(crate) fn compute_currency_state(
    last_refreshed_at: DateTime<Utc>,
    now: DateTime<Utc>,
    config: &EngineConfig,
) -> CurrencyState {
    let duration = now.signed_duration_since(last_refreshed_at);
    // Convert to fractional days (allow sub-day precision for tests).
    let days = duration.num_seconds() as f64 / 86_400.0;

    if days < config.aging_unconfirmed_threshold_days as f64 {
        CurrencyState::Fresh
    } else if days < config.decayed_threshold_days as f64 {
        CurrencyState::AgingUnconfirmed
    } else {
        CurrencyState::Decayed
    }
}

/// Build a `CurrencySignal` for a live belief, computing the decay state at `now`.
pub(crate) fn build_currency_signal(
    belief: &Belief,
    now: DateTime<Utc>,
    config: &EngineConfig,
) -> CurrencySignal {
    let state = compute_currency_state(belief.transaction_time.0, now, config);
    CurrencySignal {
        last_refreshed_at: belief.transaction_time.clone(),
        state,
        corroboration_count: belief.currency_signal.corroboration_count,
    }
}

/// Compute the `StalenessFlag` for a projection given the primary belief's currency.
/// A claim that is `Decayed` and has no valid-time confirmation is considered stale.
pub(crate) fn compute_staleness(
    primary: Option<&Belief>,
    fold: &FoldResult,
    now: DateTime<Utc>,
    config: &EngineConfig,
) -> StalenessFlag {
    if fold.live_claims.is_empty() {
        return StalenessFlag { is_stale: true, reason: Some("no live claim".into()) };
    }
    if let Some(b) = primary {
        let state = compute_currency_state(b.transaction_time.0, now, config);
        match state {
            CurrencyState::Decayed => StalenessFlag {
                is_stale: true,
                reason: Some(format!(
                    "currency decayed: last refreshed at {}",
                    b.transaction_time.0.to_rfc3339()
                )),
            },
            CurrencyState::AgingUnconfirmed => StalenessFlag {
                is_stale: false,
                reason: Some("aging unconfirmed: asserted long ago, not yet reconfirmed".into()),
            },
            CurrencyState::Fresh => fold_staleness(fold),
            // CurrencyState is #[non_exhaustive] — future variants treated as Fresh.
            _ => fold_staleness(fold),
        }
    } else {
        fold_staleness(fold)
    }
}

// ── PendingReview detection (A26) ─────────────────────────────────────────────

/// Returns the set of claim refs that have a `DependentFlaggedPendingReview` ledger entry.
///
/// Projection.rs calls this with the ledger entries for the subject-line's claims to
/// surface the `PendingReview` marker.
pub(crate) fn pending_review_refs(ledger_entries: &[LedgerEntry]) -> Vec<ClaimRef> {
    ledger_entries
        .iter()
        .filter(|e| e.event_kind == LedgerEventKind::DependentFlaggedPendingReview
            && e.disposition == Disposition::PendingReview)
        .map(|e| e.claim_ref.clone())
        .collect()
}

/// Check whether a claim ref is in the pending-review set.
pub(crate) fn is_pending_review(claim_ref: &ClaimRef, pending_refs: &[ClaimRef]) -> bool {
    pending_refs.iter().any(|r| r == claim_ref)
}

// ── Marker assembly ───────────────────────────────────────────────────────────

/// Build the full marker set for a BeliefProjection.
///
/// Markers are additive — a claim can carry multiple.
pub(crate) fn build_markers(
    fold: &FoldResult,
    pending_review_claim_refs: &[ClaimRef],
    contested: bool,
    config: &EngineConfig,
) -> Vec<Marker> {
    let mut markers = Vec::new();

    // Contested: explicit unresolved external contradiction.
    if contested || fold.has_conflict {
        markers.push(Marker::Contested);
    }

    // PendingReview: any live claim with DependentFlaggedPendingReview ledger entry.
    let any_pending = fold.live_claims.iter().any(|cs| {
        is_pending_review(cs.claim.claim_ref(), pending_review_claim_refs)
    });
    if any_pending {
        markers.push(Marker::PendingReview);
    }

    // RecallTainted: any live claim with RecallReEntry provenance.
    let any_recall = fold.live_claims.iter().any(|cs| {
        cs.claim.provenance().is_recall_reentry()
    });
    if any_recall {
        markers.push(Marker::RecallTainted);
    }

    // LowDerivationAnchor: any live claim whose derivation_depth exceeds the currency-boost cap.
    // Claims above the cap cannot receive currency boosts — surfaced to the caller for awareness.
    let any_low_anchor = fold.live_claims.iter().any(|cs| {
        cs.claim.external_anchor().derivation_depth > config.derivation_depth_cap_for_currency_boost
    });
    if any_low_anchor {
        markers.push(Marker::LowDerivationAnchor);
    }

    markers
}

// ── Main projection entry point ───────────────────────────────────────────────

/// Build a `BeliefProjection` from a `FoldResult`.
///
/// Parameters (all injected — no system clock calls):
/// - `fold`: output of `truth_engine::fold(...)`.
/// - `ledger_entries`: all ledger entries for the subject-line's claims (for PendingReview detection).
/// - `now`: the current wall-clock instant (injected by the caller — NEVER read system clock here).
/// - `config`: `EngineConfig` for decay thresholds.
/// - `contested`: true if the adjudication gate previously set `Disposition::Contested` for a live claim.
///
/// CONTRACT: "now" must be injected. Passing two different `now` values to the same fold
/// result will yield different `CurrencyState` values — this is correct and expected (currency decays).
pub(crate) fn project(
    fold: &FoldResult,
    ledger_entries: &[LedgerEntry],
    now: DateTime<Utc>,
    config: &EngineConfig,
    contested: bool,
) -> BeliefProjection {
    let pending_refs = pending_review_refs(ledger_entries);

    // Build Belief objects from live claims, applying currency decay.
    let live_beliefs: Vec<Belief> = fold.live_claims.iter().map(|cs| {
        let mut b = claim_to_belief(cs);
        b.currency_signal = build_currency_signal(&b, now, config);
        b
    }).collect();

    // Determine primary (first in canonical order) and alternatives.
    let (primary, alternatives) = if live_beliefs.is_empty() {
        (None, vec![])
    } else if live_beliefs.len() == 1 && !fold.has_conflict {
        (Some(live_beliefs[0].clone()), vec![])
    } else {
        // Conflict or multiple live: no single primary — surface all as alternatives (I7).
        (None, live_beliefs.clone())
    };

    // Currency and staleness from the primary (or first live claim).
    let primary_for_currency = primary.as_ref().or_else(|| live_beliefs.first());
    let currency = primary_for_currency
        .map(|b| build_currency_signal(b, now, config).state)
        .unwrap_or(CurrencyState::Decayed);

    // Use the best (highest) criticality across live claims.
    let criticality = fold.live_claims.iter()
        .map(|cs| cs.claim.criticality().clone())
        .max()
        .unwrap_or(mempill_types::Criticality::Low);

    let staleness = compute_staleness(primary.as_ref(), fold, now, config);
    let markers = build_markers(fold, &pending_refs, contested, config);

    // BeliefStatus resolution (I7 — Contested surfaces when has_conflict or contested flag).
    let status = if live_beliefs.is_empty() {
        BeliefStatus::NoBelief
    } else if contested || fold.has_conflict {
        BeliefStatus::Contested
    } else if live_beliefs.len() == 1 {
        let c = &fold.live_claims[0].claim;
        if c.valid_time().is_unknown() {
            BeliefStatus::TimingUncertain
        } else {
            BeliefStatus::Resolved
        }
    } else {
        BeliefStatus::Resolved // multiple set-valued, no conflict
    };

    BeliefProjection {
        status,
        primary,
        alternatives,
        currency,
        criticality,
        staleness,
        markers,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine::fold;
    use crate::ports::persistence::PersistencePort;
    use crate::ports::persistence::Txn;
    use chrono::Utc;
    use std::collections::HashMap;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimEdge, ClaimRef, Confidence, Disposition,
        ExternalAnchor, ExternalKind, Fact, LedgerEntry, LedgerEventKind, ProvenanceLabel,
        TransactionTime, ValidTime, ValidityAssertion,
    };

    // ── Mock helpers ──────────────────────────────────────────────────────────

    fn agent() -> AgentId {
        AgentId("agent-proj".into())
    }

    fn make_claim(
        agent_id: &AgentId,
        value: serde_json::Value,
        tx_time: DateTime<Utc>,
        vt_start: Option<DateTime<Utc>>,
        vt_confidence: f32,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent_id.clone(),
            Fact { subject: "user".into(), predicate: "city".into(), value },
            Cardinality::Functional,
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

    fn no_dispositions() -> HashMap<ClaimRef, Disposition> {
        HashMap::new()
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    // ── Mock PersistencePort (minimal, for ledger reads) ──────────────────────

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock error")]
    struct MockErr;

    struct MockPort {
        ledger: Vec<LedgerEntry>,
    }

    impl PersistencePort for MockPort {
        type Transaction = MockTxn;
        type Error = MockErr;

        fn begin_atomic(&self, a: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(a.clone())) }
        fn append_claim(&self, _: &mut MockTxn, _: &Claim) -> Result<ClaimRef, MockErr> { unimplemented!() }
        fn append_validity_assertion(&self, _: &mut MockTxn, _: &ValidityAssertion) -> Result<(), MockErr> { unimplemented!() }
        fn append_ledger_entry(&self, _: &mut MockTxn, _: &LedgerEntry) -> Result<(), MockErr> { unimplemented!() }
        fn append_claim_edge(&self, _: &mut MockTxn, _: &ClaimEdge) -> Result<(), MockErr> { unimplemented!() }
        fn commit(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _: MockTxn) -> Result<(), MockErr> { Ok(()) }

        fn load_subject_line(&self, _: &AgentId, _: &str, _: &str) -> Result<Vec<Claim>, MockErr> { Ok(vec![]) }
        fn load_claim(&self, _: &AgentId, _: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }
        fn load_ledger(&self, _: &AgentId, _: Option<&TransactionTime>, _: usize) -> Result<Vec<LedgerEntry>, MockErr> {
            Ok(self.ledger.clone())
        }
        fn load_ledger_for_claims(&self, _: &AgentId, _refs: &[ClaimRef]) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_edges_for(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _: &AgentId, _: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
    }

    // ── CURRENCY DECAY (I11): decayed threshold surfaces staleness marker ──────

    /// A claim asserted long ago (now - asserted > decayed_threshold_days) surfaces
    /// CurrencyState::Decayed WITHOUT being deleted or invalidated.
    #[test]
    fn currency_decay_decayed_threshold_reached() {
        let config = EngineConfig::default(); // decayed_threshold_days = 90
        let agent = agent();
        let old_tx = Utc::now() - chrono::Duration::days(100); // > 90 days ago
        let query_now = now();

        let claim = make_claim(&agent, serde_json::json!("Paris"), old_tx, None, 0.0);
        let fold_result = fold(vec![claim.clone()], no_assertions, query_now, &config, &no_dispositions());

        let projection = project(&fold_result, &[], query_now, &config, false);

        assert_eq!(projection.currency, CurrencyState::Decayed, "should be Decayed after 100 days");
        // The claim is still live (not deleted) — I11 non-destruction.
        assert!(projection.primary.is_some() || !projection.alternatives.is_empty(),
            "I11: decayed claim must still be present in projection (never deleted)");
    }

    /// AgingUnconfirmed: between aging_unconfirmed_threshold_days (30) and decayed_threshold_days (90).
    #[test]
    fn currency_decay_aging_unconfirmed() {
        let config = EngineConfig::default();
        let agent = agent();
        let tx = Utc::now() - chrono::Duration::days(45); // 30 < 45 < 90
        let query_now = now();

        let claim = make_claim(&agent, serde_json::json!("Rome"), tx, None, 0.0);
        let fold_result = fold(vec![claim], no_assertions, query_now, &config, &no_dispositions());

        let projection = project(&fold_result, &[], query_now, &config, false);

        assert_eq!(projection.currency, CurrencyState::AgingUnconfirmed, "should be AgingUnconfirmed");
    }

    /// Fresh: claim less than aging_unconfirmed_threshold_days (30) old.
    #[test]
    fn currency_decay_fresh() {
        let config = EngineConfig::default();
        let agent = agent();
        let tx = Utc::now() - chrono::Duration::days(10);
        let query_now = now();

        let claim = make_claim(&agent, serde_json::json!("Berlin"), tx, None, 0.0);
        let fold_result = fold(vec![claim], no_assertions, query_now, &config, &no_dispositions());

        let projection = project(&fold_result, &[], query_now, &config, false);

        assert_eq!(projection.currency, CurrencyState::Fresh, "should be Fresh within 30 days");
    }

    /// Explicit negative assertion (Invalidated via Bound) → claim NOT live.
    /// This tests that an invalidated claim does not appear in projection.
    #[test]
    fn explicit_invalidation_via_bound_assertion_removes_from_live() {
        use mempill_types::AssertionKind;
        let config = EngineConfig::default();
        let agent = agent();
        let tx = Utc::now() - chrono::Duration::days(1);
        let bound_at = Utc::now() - chrono::Duration::hours(1);
        let query_now = now();

        let claim = make_claim(&agent, serde_json::json!("London"), tx, None, 0.0);
        let claim_ref = claim.claim_ref().clone();

        let bound_assertion = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_at),
        };

        let bound_ref = claim_ref.clone();
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == bound_ref {
                vec![bound_assertion.clone()]
            } else {
                vec![]
            }
        };

        let fold_result = fold(vec![claim], assertions_fn, query_now, &config, &no_dispositions());
        let projection = project(&fold_result, &[], query_now, &config, false);

        assert!(projection.primary.is_none(), "invalidated claim must not be the primary belief");
        assert!(projection.alternatives.is_empty(), "invalidated claim must not be in alternatives");
        assert_eq!(projection.status, BeliefStatus::NoBelief, "NoBelief after invalidation");
    }

    // ── CONTESTED (I7): unresolved conflict surfaces Contested ────────────────

    #[test]
    fn contested_fold_conflict_surfaces_contested_status() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc::now() - chrono::Duration::hours(5);
        let t2 = Utc::now() - chrono::Duration::hours(1);

        let c1 = make_claim(&agent, serde_json::json!("Paris"), t1, None, 0.0);
        let c2 = make_claim(&agent, serde_json::json!("Rome"), t2, None, 0.0);
        let fold_result = fold(vec![c1, c2], no_assertions, now(), &config, &no_dispositions());

        // No silent pick — project with contested=false but fold has_conflict=true.
        let projection = project(&fold_result, &[], now(), &config, false);

        assert_eq!(projection.status, BeliefStatus::Contested, "I7: conflict must surface as Contested");
        assert!(projection.primary.is_none(), "I7: no silent primary when contested");
        assert_eq!(projection.alternatives.len(), 2, "both contested claims in alternatives");
        assert!(projection.markers.contains(&Marker::Contested), "Contested marker must be present");
    }

    // ── PENDINGREVIEW (A26): ledger entry surfaces PendingReview marker ───────

    #[test]
    fn pending_review_a26_ledger_entry_surfaces_marker() {
        let config = EngineConfig::default();
        let agent = agent();
        let tx = Utc::now() - chrono::Duration::days(1);
        let query_now = now();

        let claim = make_claim(&agent, serde_json::json!("Madrid"), tx, None, 0.0);
        let claim_ref = claim.claim_ref().clone();

        let fold_result = fold(vec![claim], no_assertions, query_now, &config, &no_dispositions());

        // Simulate a DependentFlaggedPendingReview ledger entry for this claim.
        let pending_entry = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: claim_ref.clone(),
            event_kind: LedgerEventKind::DependentFlaggedPendingReview,
            disposition: Disposition::PendingReview,
            rationale: None,
            recorded_at: TransactionTime(tx),
        };

        let projection = project(&fold_result, &[pending_entry], query_now, &config, false);

        assert!(
            projection.markers.contains(&Marker::PendingReview),
            "A26: PendingReview marker must appear when DependentFlaggedPendingReview ledger entry exists"
        );
    }

    // ── "now" injection: different now → different decay ─────────────────────

    /// Passing two different `now` values for the same claim must produce different CurrencyState.
    /// This verifies the "now" injection contract — no system clock called inside projection.
    #[test]
    fn now_injection_different_now_different_currency_state() {
        let config = EngineConfig::default();
        let agent = agent();
        // Claim asserted exactly at decayed_threshold_days boundary.
        let tx = Utc::now() - chrono::Duration::days(91); // just past 90 day decayed threshold

        let claim = make_claim(&agent, serde_json::json!("Lisbon"), tx, None, 0.0);

        // "now" A = present time: 91 days after tx → Decayed
        let now_a = Utc::now();
        let disp = no_dispositions();
        let fold_a = fold(vec![claim.clone()], no_assertions, now_a, &config, &disp);
        let proj_a = project(&fold_a, &[], now_a, &config, false);

        // "now" B = 50 days ago: at that point, claim was only ~41 days old → AgingUnconfirmed
        let now_b = Utc::now() - chrono::Duration::days(50);
        let fold_b = fold(vec![claim], no_assertions, now_b, &config, &disp);
        let proj_b = project(&fold_b, &[], now_b, &config, false);

        assert_eq!(proj_a.currency, CurrencyState::Decayed, "now_a: should be Decayed");
        assert_eq!(proj_b.currency, CurrencyState::AgingUnconfirmed, "now_b: should be AgingUnconfirmed");
        assert_ne!(proj_a.currency, proj_b.currency, "different now values must yield different currency state");
    }

    // ── NoBelief: empty subject-line produces NoBelief ────────────────────────

    #[test]
    fn no_belief_for_empty_subject_line() {
        let config = EngineConfig::default();
        let fold_result = fold(vec![], no_assertions, now(), &config, &no_dispositions());
        let projection = project(&fold_result, &[], now(), &config, false);

        assert_eq!(projection.status, BeliefStatus::NoBelief);
        assert!(projection.primary.is_none());
        assert!(projection.alternatives.is_empty());
        assert!(projection.staleness.is_stale);
    }
}
