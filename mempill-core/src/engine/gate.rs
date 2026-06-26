//! Adjudication Gate — the deterministic pure function at the stochastic/deterministic boundary.
//!
//! Replay-audit invariant: same `Proposal` + same `EngineConfig` → byte-identical `GateDecision`.
//! No system clock reads, no RNG, no I/O, no `HashMap` iteration-order dependence inside
//! `adjudicate()`. All timestamps enter via the `Proposal`; none are sampled here.

use mempill_types::{Cardinality, Claim, Disposition, ProvenanceLabel};
use crate::config::EngineConfig;

/// The LLM-emitted proposal. Crosses the stochastic/deterministic boundary.
/// The gate consumes this; nothing downstream re-calls the LLM.
///
/// `oracle_present` lives here in the gate, never in `mempill-types`.
/// When false and evidence is fresh first-hand external, `Contested` fires immediately.
#[derive(Debug, Clone)]
pub(crate) struct Proposal {
    /// The candidate claim, already stamped by the ingestion gateway.
    pub candidate: Claim,
    /// None = no active belief on this subject-line (first write).
    pub incumbent: Option<mempill_types::Belief>,
    pub conflict_type: ConflictType,
    /// Reconciler-measured disposition confidence (stochastic input; recorded to ledger).
    pub measured_confidence: f32,
    pub cardinality_proposal: Cardinality,
    /// Whether the `OraclePort` has a registered listener at decision time.
    /// Passed in from the engine wrapper so `adjudicate()` remains a pure function.
    /// When false + fresh first-hand external contradiction: `Contested` fires immediately.
    /// When true: route to `QueuedForAdjudication` (oracle receives `AdjudicationRequest`).
    pub oracle_present: bool,
}

/// Conflict classification emitted by the Reconciler and consumed by the adjudication gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConflictType {
    /// New non-conflicting claim — no existing belief on this subject-line.
    NoConflict,
    /// Same (subject, predicate), different value — belief-overturning candidate.
    SameLineConflict,
    /// Mutual-exclusion or entailment across predicates.
    CrossLineConflict,
    /// Parent was superseded; dependent needs review.
    DependsOnSuperseded,
    /// Clean temporal succession: candidate and incumbent have NON-OVERLAPPING trusted
    /// valid-time windows. Routes to CheapPath / CommittedCheap (NOT Contested).
    /// Audit/G1 visible: the succession is recorded in rationale.
    Succession,
}

/// Gate decision — deterministic function of `Proposal` + `EngineConfig`.
/// PURE: same input → same `GateDecision`. The LLM is not re-called here.
/// All inputs and the output are recorded to the ledger for replay audit.
#[derive(Debug, Clone)]
pub(crate) struct GateDecision {
    pub route: Route,
    pub disposition: Disposition,
    /// Logged to ledger; includes all estimators used in the decision (G1 replay basis).
    pub rationale: serde_json::Value,
}

/// Routing outcome from the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// Non-conflicting External(*) → CommittedCheap.
    CheapPath,
    /// ModelDerived → CommittedInferred (down-weighted; cannot overturn until anchored to first-hand external).
    InferredRoute,
    /// RecallReEntry → corroborate-by-identity; no new claim.
    RecallTainted,
    /// Belief-overturning → async adjudication (QueuedForAdjudication or Contested).
    HeavyPath,
    /// Incoherent tx/valid or burst detected → parked, auditable, not destroyed.
    Quarantine,
}

/// Deterministic adjudication gate function.
///
/// PURE FUNCTION — no side effects, no I/O, no clock reads.
/// Corroboration is a confidence modifier only — it adjusts confidence logged to rationale
/// but does NOT by itself flip the route or disposition.
///
/// Decision order (execute in this order; return on first match):
/// 1. RecallReEntry    → RecallTainted / CommittedCheap
/// 2. Temporal coherence check → Quarantine if incoherent
/// 3. ModelDerived     → InferredRoute / CommittedInferred
/// 4. No conflict or no incumbent → CheapPath / CommittedCheap
/// 5. Heavy path with oracle-absent branching
pub(crate) fn adjudicate(proposal: &Proposal, config: &EngineConfig) -> GateDecision {
    // Step 1: RecallReEntry — corroborate-by-identity; no new claim.
    if proposal.candidate.provenance().is_recall_reentry() {
        return GateDecision {
            route: Route::RecallTainted,
            disposition: Disposition::CommittedCheap,
            rationale: serde_json::json!({ "route": "recall_reentry" }),
        };
    }

    // Step 2: Temporal coherence check (B7, A25).
    // Quarantine incoherent windows when valid_time_confidence is above threshold.
    if is_temporally_incoherent(&proposal.candidate, config) {
        return GateDecision {
            route: Route::Quarantine,
            disposition: Disposition::Quarantined,
            rationale: serde_json::json!({
                "route": "quarantine",
                "reason": "incoherent_temporal_window",
            }),
        };
    }

    // Step 3: ModelDerived → down-weighted, never overturns (V3-4).
    if *proposal.candidate.provenance() == ProvenanceLabel::ModelDerived {
        return GateDecision {
            route: Route::InferredRoute,
            disposition: Disposition::CommittedInferred,
            rationale: serde_json::json!({
                "route": "inferred",
                "derivation_depth": proposal.candidate.external_anchor().derivation_depth,
            }),
        };
    }

    // Step 4: No conflict or no incumbent → cheap path.
    // Only External(*) reaches here (RecallReEntry and ModelDerived already handled above).
    if proposal.conflict_type == ConflictType::NoConflict || proposal.incumbent.is_none() {
        return GateDecision {
            route: Route::CheapPath,
            disposition: Disposition::CommittedCheap,
            rationale: serde_json::json!({ "route": "cheap_path" }),
        };
    }

    // Step 4b: Trusted temporal succession — non-overlapping windows, confident timestamps.
    // NOT a belief-overturning conflict; the candidate simply occupies a later valid-time window.
    // Routes to CheapPath / CommittedCheap — NOT Contested, NOT QueuedForAdjudication.
    // Audit-visible: succession logged in rationale (G1).
    if proposal.conflict_type == ConflictType::Succession {
        return GateDecision {
            route: Route::CheapPath,
            disposition: Disposition::CommittedCheap,
            rationale: serde_json::json!({
                "route": "succession_cheap_path",
                "reason": "trusted_temporal_succession",
            }),
        };
    }

    // Step 5: Heavy path — belief-overturning. Apply B11 oracle-absent branching.
    //
    // B11(a): Fresh first-hand external contradiction + oracle ABSENT
    //   → Contested fires IMMEDIATELY (V3-5). Never silent incumbent-wins.
    //   Oracle confirms later; it does not authorize.
    // B11(b): Ambiguous rivals OR oracle PRESENT
    //   → QueuedForAdjudication (async, non-blocking).
    //
    // Corroboration modulates confidence in rationale only — does NOT change routing.
    let is_fresh_external_contradiction = proposal.candidate.provenance().is_cheap_path_eligible()
        && matches!(
            proposal.conflict_type,
            ConflictType::SameLineConflict | ConflictType::CrossLineConflict
        );

    if is_fresh_external_contradiction && !proposal.oracle_present {
        // B11(a): downgrade fires without oracle. Projection returns BOTH claims + Contested.
        return GateDecision {
            route: Route::HeavyPath,
            disposition: Disposition::Contested,
            rationale: serde_json::json!({
                "route": "heavy_path_contested_oracle_absent",
                "conflict_type": format!("{:?}", proposal.conflict_type),
                "measured_confidence": proposal.measured_confidence,
                "oracle_present": false,
            }),
        };
    }

    // B11(b): oracle present or non-external contradiction → queue for adjudication.
    GateDecision {
        route: Route::HeavyPath,
        disposition: Disposition::QueuedForAdjudication,
        rationale: serde_json::json!({
            "route": "heavy_path",
            "conflict_type": format!("{:?}", proposal.conflict_type),
            "corroboration_count": 0, // modifier only; logged; does not change route
            "measured_confidence": proposal.measured_confidence,
            "oracle_present": proposal.oracle_present,
        }),
    }
}

/// Temporal coherence check.
///
/// Returns `true` iff the claim's valid_time window is incoherent AND
/// `valid_time_confidence` is at or above the threshold (below threshold → treat
/// as unknown; not incoherent, just uncertain).
///
/// Two incoherence conditions:
/// 1. `valid_time_start > valid_time_end` — physically impossible window.
/// 2. `valid_time_start > tx_time`        — a fact dated as valid AFTER it was learned.
///    (Closes the case where an extractor assigns a future valid-start to a past fact.)
fn is_temporally_incoherent(claim: &Claim, config: &EngineConfig) -> bool {
    if claim.valid_time().valid_time_confidence < config.valid_time_confidence_threshold {
        return false; // low confidence → treat as unknown; not incoherent
    }

    // Rule 1: start after end is always incoherent.
    if let (Some(start), Some(end)) = (&claim.valid_time().start, &claim.valid_time().end) {
        if start > end {
            return true;
        }
    }

    // Rule 2 (B7/A25): valid_time_start must not be AFTER tx_time.
    if let Some(start) = &claim.valid_time().start {
        if start > &claim.transaction_time().0 {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, ExternalAnchor,
        ExternalKind, Fact, ProvenanceLabel, TransactionTime, ValidTime,
    };
    use chrono::{TimeZone, Utc};

    fn tx_time_at(year: i32, month: u32, day: u32) -> TransactionTime {
        TransactionTime(Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap())
    }

    fn make_claim(
        provenance: ProvenanceLabel,
        valid_time: ValidTime,
        tx_time: TransactionTime,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent-1".into()),
            Fact {
                subject: "user".into(),
                predicate: "location".into(),
                value: serde_json::json!("Paris"),
            },
            Cardinality::Functional,
            provenance,
            ExternalAnchor {
                nearest_external_anchor: None,
                derivation_depth: 0,
            },
            tx_time,
            valid_time,
            Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.9,
            },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn external_claim(valid_time: ValidTime, tx_time: TransactionTime) -> Claim {
        make_claim(
            ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            valid_time,
            tx_time,
        )
    }

    fn model_derived_claim() -> Claim {
        make_claim(
            ProvenanceLabel::ModelDerived,
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            tx_time_at(2026, 1, 1),
        )
    }

    fn recall_reentry_claim() -> Claim {
        make_claim(
            ProvenanceLabel::RecallReEntry,
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            tx_time_at(2026, 1, 1),
        )
    }

    fn no_conflict_proposal(claim: Claim) -> Proposal {
        Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        }
    }

    fn config() -> EngineConfig {
        EngineConfig::default() // valid_time_confidence_threshold = 0.7
    }

    // ── DETERMINISM TESTS ────────────────────────────────────────────────────────

    #[test]
    fn adjudicate_is_deterministic_same_inputs_same_output() {
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = external_claim(vt, tx);
        let proposal = no_conflict_proposal(claim);
        let cfg = config();

        let d1 = adjudicate(&proposal, &cfg);
        let d2 = adjudicate(&proposal, &cfg);

        assert_eq!(d1.route, d2.route);
        assert_eq!(d1.disposition, d2.disposition);
        assert_eq!(d1.rationale.to_string(), d2.rationale.to_string());
    }

    #[test]
    fn adjudicate_deterministic_across_multiple_fixed_input_sets() {
        let cfg = config();
        let tx = tx_time_at(2026, 6, 1);

        let inputs: Vec<Proposal> = vec![
            // cheap path
            no_conflict_proposal(external_claim(
                ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
                tx.clone(),
            )),
            // model derived
            no_conflict_proposal(model_derived_claim()),
            // recall reentry
            no_conflict_proposal(recall_reentry_claim()),
        ];

        for proposal in &inputs {
            let d1 = adjudicate(proposal, &cfg);
            let d2 = adjudicate(proposal, &cfg);
            assert_eq!(
                d1.rationale.to_string(),
                d2.rationale.to_string(),
                "non-deterministic for route {:?}",
                d1.route
            );
            assert_eq!(d1.route, d2.route);
            assert_eq!(d1.disposition, d2.disposition);
        }
    }

    // ── B7 TEMPORAL COHERENCE TESTS (A25) ───────────────────────────────────────

    #[test]
    fn b7_start_after_end_is_quarantined() {
        let tx = tx_time_at(2026, 6, 1);
        let start = Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(); // end before start
        let vt = ValidTime {
            start: Some(start),
            end: Some(end),
            valid_time_confidence: 0.9, // above threshold
        };
        let claim = external_claim(vt, tx);
        let proposal = no_conflict_proposal(claim);
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::Quarantine);
        assert_eq!(decision.disposition, Disposition::Quarantined);
    }

    #[test]
    fn b7_valid_time_start_after_tx_time_is_quarantined() {
        // A fact whose valid_time_start is AFTER the tx_time it was learned — A25 violation.
        let tx = tx_time_at(2026, 1, 1);
        let future_start = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(); // after tx_time
        let vt = ValidTime {
            start: Some(future_start),
            end: None,
            valid_time_confidence: 0.9, // above threshold
        };
        let claim = external_claim(vt, tx);
        let proposal = no_conflict_proposal(claim);
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::Quarantine, "valid_time_start > tx_time must quarantine");
        assert_eq!(decision.disposition, Disposition::Quarantined);
    }

    #[test]
    fn b7_low_confidence_valid_time_not_quarantined() {
        // Low valid_time_confidence → treat as unknown; not incoherent even if start > tx_time.
        let tx = tx_time_at(2026, 1, 1);
        let future_start = Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap();
        let vt = ValidTime {
            start: Some(future_start),
            end: None,
            valid_time_confidence: 0.3, // below threshold (0.7)
        };
        let claim = external_claim(vt, tx);
        let proposal = no_conflict_proposal(claim);
        let decision = adjudicate(&proposal, &config());
        // Should be cheap path (no conflict, no incumbent), NOT quarantined
        assert_eq!(decision.route, Route::CheapPath);
        assert_eq!(decision.disposition, Disposition::CommittedCheap);
    }

    #[test]
    fn b7_coherent_temporal_window_passes() {
        let tx = tx_time_at(2026, 6, 1);
        let start = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(); // before tx_time
        let end = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();   // before tx_time, after start
        let vt = ValidTime {
            start: Some(start),
            end: Some(end),
            valid_time_confidence: 0.9,
        };
        let claim = external_claim(vt, tx);
        let proposal = no_conflict_proposal(claim);
        let decision = adjudicate(&proposal, &config());
        // Coherent window, no conflict → cheap path
        assert_eq!(decision.route, Route::CheapPath);
        assert_eq!(decision.disposition, Disposition::CommittedCheap);
    }

    // ── B11 ORACLE-ABSENT TESTS ──────────────────────────────────────────────────

    fn incumbent_belief() -> mempill_types::Belief {
        use mempill_types::{CurrencySignal, CurrencyState};
        mempill_types::Belief {
            claim_ref: ClaimRef::new_random(),
            fact: Fact {
                subject: "user".into(),
                predicate: "location".into(),
                value: serde_json::json!("Berlin"),
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            transaction_time: tx_time_at(2025, 1, 1),
            confidence: Confidence { value_confidence: 0.8, valid_time_confidence: 0.0 },
            currency_signal: CurrencySignal {
                last_refreshed_at: tx_time_at(2025, 1, 1),
                state: CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: Criticality::Medium,
        }
    }

    #[test]
    fn b11_fresh_external_oracle_absent_routes_to_contested() {
        // B11(a): fresh first-hand external contradiction + oracle_present=false → Contested immediately.
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = external_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent_belief()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.85,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false, // no oracle — B11(a) fires
        };
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::HeavyPath);
        assert_eq!(decision.disposition, Disposition::Contested,
            "oracle absent + fresh external contradiction MUST route to Contested immediately (B11)");
    }

    #[test]
    fn b11_fresh_external_oracle_present_routes_to_queued() {
        // B11(b): oracle present → QueuedForAdjudication, NOT Contested.
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = external_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent_belief()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.85,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: true, // oracle available — B11(b) fires
        };
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::HeavyPath);
        assert_eq!(decision.disposition, Disposition::QueuedForAdjudication,
            "oracle present should route to QueuedForAdjudication, not Contested");
    }

    #[test]
    fn b11_cross_line_conflict_oracle_absent_routes_to_contested() {
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = external_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent_belief()),
            conflict_type: ConflictType::CrossLineConflict,
            measured_confidence: 0.7,
            cardinality_proposal: Cardinality::SetValued,
            oracle_present: false,
        };
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.disposition, Disposition::Contested);
    }

    // ── CORROBORATION IS A MODIFIER, NOT A GATE ──────────────────────────────────

    #[test]
    fn corroboration_does_not_change_route_vs_same_case_without_corroboration() {
        // The gate does not accept a corroboration_count field in Proposal —
        // corroboration adjusts confidence externally but does not exist as a gate input.
        // Verify: two proposals with identical routing conditions produce identical routes.
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        let claim_a = external_claim(vt.clone(), tx.clone());
        let claim_b = external_claim(vt.clone(), tx.clone());

        let proposal_a = Proposal {
            candidate: claim_a,
            incumbent: Some(incumbent_belief()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.85,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let proposal_b = Proposal {
            candidate: claim_b,
            incumbent: Some(incumbent_belief()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.85,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };

        let d_a = adjudicate(&proposal_a, &config());
        let d_b = adjudicate(&proposal_b, &config());

        assert_eq!(d_a.route, d_b.route, "corroboration must not change route");
        assert_eq!(d_a.disposition, d_b.disposition, "corroboration must not change disposition");
    }

    // ── CHEAP PATH ELIGIBILITY ───────────────────────────────────────────────────

    #[test]
    fn cheap_path_requires_external_provenance() {
        // ModelDerived is NOT cheap-path eligible — must take InferredRoute.
        let proposal = no_conflict_proposal(model_derived_claim());
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::InferredRoute);
        assert_eq!(decision.disposition, Disposition::CommittedInferred);
        assert_ne!(decision.route, Route::CheapPath);
    }

    #[test]
    fn recall_reentry_is_not_cheap_path_eligible() {
        let proposal = no_conflict_proposal(recall_reentry_claim());
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::RecallTainted);
        assert_ne!(decision.route, Route::CheapPath);
    }

    #[test]
    fn external_no_conflict_takes_cheap_path() {
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = external_claim(vt, tx);
        let proposal = no_conflict_proposal(claim);
        let decision = adjudicate(&proposal, &config());
        assert_eq!(decision.route, Route::CheapPath);
        assert_eq!(decision.disposition, Disposition::CommittedCheap);
    }

    #[test]
    fn proposal_carries_oracle_present_field() {
        // Structural: verify oracle_present is a field on Proposal (A24 compliance check).
        let tx = tx_time_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = external_claim(vt, tx);
        let p = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: true,
        };
        assert!(p.oracle_present, "oracle_present must be readable as a Proposal field (A24)");
    }
}

// ── QA ADVERSARIAL TESTS ─────────────────────────────────────────────────────
//
// This module is the adversarial QA layer. It deliberately attacks the five
// correctness properties of the adjudication gate and is SEPARATE from
// the implementation's own `tests` module above so authorship is clear.
//
// Properties under attack:
//   P1 — DETERMINISM (G1): byte-identical output for identical inputs
//   P2 — B7 temporal coherence boundary cases (A25)
//   P3 — B11 oracle-absent coverage across ALL heavy-path conflict types
//   P4 — Corroboration is a confidence MODIFIER only, never a route-flipper
//   P5 — ROUTING completeness: every Proposal maps to exactly one Route; no panics
#[cfg(test)]
mod adversarial {
    use super::*;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, CurrencySignal,
        CurrencyState, ExternalAnchor, ExternalKind, Fact, ProvenanceLabel, TransactionTime,
        ValidTime,
    };
    use chrono::{TimeZone, Utc};

    // ── shared helpers ────────────────────────────────────────────────────────

    fn tx_at(year: i32, month: u32, day: u32) -> TransactionTime {
        TransactionTime(Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap())
    }

    fn dt(year: i32, month: u32, day: u32) -> chrono::DateTime<chrono::Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    fn make_claim_with_confidence(
        provenance: ProvenanceLabel,
        valid_time: ValidTime,
        tx_time: TransactionTime,
        vt_confidence: f32,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent-adv".into()),
            Fact {
                subject: "subject".into(),
                predicate: "predicate".into(),
                value: serde_json::json!("value"),
            },
            Cardinality::Functional,
            provenance,
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            tx_time,
            valid_time,
            Confidence { value_confidence: 0.9, valid_time_confidence: vt_confidence },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn ext_claim(vt: ValidTime, tx: TransactionTime) -> Claim {
        // Extract confidence BEFORE vt is moved into make_claim_with_confidence.
        let vt_conf = vt.valid_time_confidence;
        make_claim_with_confidence(
            ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            vt,
            tx,
            vt_conf,
        )
    }

    fn user_asserted_claim(vt: ValidTime, tx: TransactionTime) -> Claim {
        // Extract confidence BEFORE vt is moved into make_claim_with_confidence.
        let vt_conf = vt.valid_time_confidence;
        make_claim_with_confidence(
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            vt,
            tx,
            vt_conf,
        )
    }

    fn recall_claim() -> Claim {
        make_claim_with_confidence(
            ProvenanceLabel::RecallReEntry,
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            tx_at(2026, 1, 1),
            0.0,
        )
    }

    fn model_claim() -> Claim {
        make_claim_with_confidence(
            ProvenanceLabel::ModelDerived,
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            tx_at(2026, 1, 1),
            0.0,
        )
    }

    fn incumbent() -> mempill_types::Belief {
        mempill_types::Belief {
            claim_ref: ClaimRef::new_random(),
            fact: Fact {
                subject: "subject".into(),
                predicate: "predicate".into(),
                value: serde_json::json!("old_value"),
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            transaction_time: tx_at(2025, 1, 1),
            confidence: Confidence { value_confidence: 0.8, valid_time_confidence: 0.0 },
            currency_signal: CurrencySignal {
                last_refreshed_at: tx_at(2025, 1, 1),
                state: CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: Criticality::Medium,
        }
    }

    fn cfg() -> EngineConfig {
        EngineConfig::default() // valid_time_confidence_threshold = 0.7
    }

    // ══════════════════════════════════════════════════════════════════════════
    // P1 — DETERMINISM ATTACKS
    // ══════════════════════════════════════════════════════════════════════════

    /// Attack: call adjudicate() 100 times on the same proposal; all results must be
    /// byte-identical. Detects hidden clock reads, RNG leakage, or HashMap ordering.
    #[test]
    fn p1_repeated_calls_same_proposal_always_identical() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.88,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let reference = adjudicate(&proposal, &cfg());
        for i in 0..100 {
            let d = adjudicate(&proposal, &cfg());
            assert_eq!(d.route, reference.route, "non-deterministic route on iteration {i}");
            assert_eq!(d.disposition, reference.disposition,
                "non-deterministic disposition on iteration {i}");
            assert_eq!(d.rationale.to_string(), reference.rationale.to_string(),
                "non-deterministic rationale JSON on iteration {i}");
        }
    }

    /// Attack: verify that the rationale JSON key order is stable across calls.
    /// serde_json preserves insertion order — this test catches any future HashMap
    /// usage inside the rationale builder.
    #[test]
    fn p1_rationale_json_key_order_is_stable() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.75,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: true,
        };
        let d1 = adjudicate(&proposal, &cfg());
        let d2 = adjudicate(&proposal, &cfg());
        assert_eq!(d1.rationale.to_string(), d2.rationale.to_string(),
            "rationale JSON key order must be stable (G1 replay basis)");
    }

    /// Attack: verify determinism holds for the heavy-path contested branch specifically,
    /// including that conflict_type Debug repr in rationale is stable.
    #[test]
    fn p1_heavy_path_contested_rationale_is_deterministic() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        for conflict_type in [ConflictType::SameLineConflict, ConflictType::CrossLineConflict] {
            let claim = ext_claim(vt.clone(), tx.clone());
            let proposal = Proposal {
                candidate: claim,
                incumbent: Some(incumbent()),
                conflict_type: conflict_type.clone(),
                measured_confidence: 0.9,
                cardinality_proposal: Cardinality::Functional,
                oracle_present: false,
            };
            let d1 = adjudicate(&proposal, &cfg());
            let d2 = adjudicate(&proposal, &cfg());
            assert_eq!(d1.rationale.to_string(), d2.rationale.to_string(),
                "rationale non-deterministic for {:?}", conflict_type);
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // P2 — B7 TEMPORAL COHERENCE BOUNDARY ATTACKS
    // ══════════════════════════════════════════════════════════════════════════

    /// Attack: start == end (zero-duration window). Must NOT be quarantined.
    /// Spec says quarantine when start > end; equality is a valid instant window.
    #[test]
    fn p2_start_equals_end_is_not_quarantined() {
        let tx = tx_at(2026, 6, 1);
        let instant = dt(2025, 3, 15);
        let vt = ValidTime {
            start: Some(instant),
            end: Some(instant),
            valid_time_confidence: 0.9,
        };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_ne!(d.route, Route::Quarantine,
            "start == end is a valid zero-duration window; must NOT be quarantined (B7 only fires on start > end)");
        assert_eq!(d.route, Route::CheapPath);
    }

    /// Attack: valid_time_start == tx_time exactly. Must NOT be quarantined.
    /// Spec says quarantine when start > tx_time; equality means "learned exactly at validity start".
    #[test]
    fn p2_start_equals_tx_time_is_not_quarantined() {
        let tx = tx_at(2026, 6, 1);
        let same_moment = dt(2026, 6, 1); // exactly equal to tx_time
        let vt = ValidTime {
            start: Some(same_moment),
            end: None,
            valid_time_confidence: 0.9,
        };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_ne!(d.route, Route::Quarantine,
            "start == tx_time is allowed (B7 only fires on start > tx_time, not >=)");
        assert_eq!(d.route, Route::CheapPath);
    }

    /// Attack: valid_time_confidence is EXACTLY at the threshold (0.7).
    /// Threshold check is `< 0.7` so 0.7 is NOT below threshold — temporal check runs.
    /// An incoherent window at exactly 0.7 confidence MUST quarantine.
    #[test]
    fn p2_confidence_exactly_at_threshold_triggers_temporal_check() {
        let tx = tx_at(2026, 1, 1);
        let future_start = dt(2026, 12, 1); // after tx_time — incoherent
        let vt = ValidTime {
            start: Some(future_start),
            end: None,
            valid_time_confidence: 0.7, // exactly at threshold — check runs
        };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::Quarantine,
            "confidence == 0.7 (threshold) must NOT bypass temporal check; incoherent window must quarantine");
    }

    /// Attack: valid_time_confidence just below threshold (0.6999).
    /// The temporal check is skipped; an incoherent window at low confidence MUST pass through.
    #[test]
    fn p2_confidence_just_below_threshold_bypasses_temporal_check() {
        let tx = tx_at(2026, 1, 1);
        let future_start = dt(2026, 12, 1); // after tx_time — would be incoherent if checked
        let vt = ValidTime {
            start: Some(future_start),
            end: None,
            valid_time_confidence: 0.6999, // just below 0.7
        };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_ne!(d.route, Route::Quarantine,
            "confidence 0.6999 < 0.7 (threshold): temporal check must be skipped; claim must not quarantine");
        assert_eq!(d.route, Route::CheapPath);
    }

    /// Attack: far-past start and far-future end — both individually coherent.
    /// No quarantine expected; verifies the gate does not penalize wide valid-time windows.
    #[test]
    fn p2_far_past_start_far_future_end_is_coherent() {
        let tx = tx_at(2026, 6, 1);
        let far_past = dt(1900, 1, 1);
        let far_future = dt(9999, 12, 31);
        let vt = ValidTime {
            start: Some(far_past),
            end: Some(far_future),
            valid_time_confidence: 0.9,
        };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        // start(1900) < tx(2026) and start(1900) < end(9999) — coherent
        assert_eq!(d.route, Route::CheapPath,
            "wide but coherent valid-time window must not quarantine");
    }

    /// Attack: RecallReEntry with a deeply incoherent temporal window.
    /// Step 1 (RecallReEntry) fires BEFORE Step 2 (temporal). The claim should NOT
    /// be quarantined — it should be RecallTainted/CommittedCheap.
    ///
    /// FINDING NOTE: This documents a known ordering effect. RecallReEntry bypasses the
    /// temporal quarantine check per §6 decision order. This is intentional per spec
    /// (recall re-entries are already degraded; quarantine adds no further value and the
    /// existing claim is corroborated by identity, not the incoherent window).
    #[test]
    fn p2_recall_reentry_with_incoherent_window_bypasses_quarantine() {
        let tx = tx_at(2026, 1, 1);
        let future_start = dt(2026, 12, 1); // after tx_time — would quarantine any other provenance
        let vt = ValidTime {
            start: Some(future_start),
            end: None,
            valid_time_confidence: 0.9,
        };
        let claim = make_claim_with_confidence(
            ProvenanceLabel::RecallReEntry,
            vt,
            tx,
            0.9,
        );
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        // RecallReEntry fires at step 1, before temporal check at step 2.
        assert_eq!(d.route, Route::RecallTainted,
            "RecallReEntry is caught at step 1 before temporal check (§6 ordering); must not quarantine");
        assert_eq!(d.disposition, Disposition::CommittedCheap);
    }

    /// Attack: ModelDerived with an incoherent temporal window.
    /// Step 2 (temporal) fires BEFORE Step 3 (ModelDerived). Must quarantine, not InferredRoute.
    #[test]
    fn p2_model_derived_with_incoherent_window_is_quarantined_not_inferred() {
        let tx = tx_at(2026, 1, 1);
        let future_start = dt(2026, 12, 1); // after tx_time
        let vt = ValidTime {
            start: Some(future_start),
            end: None,
            valid_time_confidence: 0.9,
        };
        let claim = make_claim_with_confidence(
            ProvenanceLabel::ModelDerived,
            vt,
            tx,
            0.9,
        );
        let proposal = Proposal {
            candidate: claim,
            incumbent: None,
            conflict_type: ConflictType::NoConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::Quarantine,
            "ModelDerived with incoherent temporal window must quarantine (step 2 before step 3)");
        assert_ne!(d.route, Route::InferredRoute,
            "incoherent ModelDerived must NOT silently route to InferredRoute");
    }

    // ══════════════════════════════════════════════════════════════════════════
    // P3 — B11 ORACLE-ABSENT COVERAGE ACROSS ALL HEAVY-PATH CONFLICT TYPES
    // ══════════════════════════════════════════════════════════════════════════

    /// Attack: UserAsserted (also External(*)) + SameLineConflict + oracle absent.
    /// Both ExternalFirstHand AND UserAsserted are cheap-path eligible.
    /// The oracle-absent contradiction path must fire for UserAsserted too.
    #[test]
    fn p3_user_asserted_same_line_oracle_absent_routes_to_contested() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = user_asserted_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::HeavyPath);
        assert_eq!(d.disposition, Disposition::Contested,
            "UserAsserted is External(*) and thus cheap-path eligible; B11(a) must fire");
    }

    /// Attack: UserAsserted + CrossLineConflict + oracle absent.
    /// The oracle-absent contradiction path covers both SameLine and CrossLine conflicts.
    #[test]
    fn p3_user_asserted_cross_line_oracle_absent_routes_to_contested() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = user_asserted_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::CrossLineConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.disposition, Disposition::Contested,
            "CrossLineConflict + UserAsserted + oracle absent must be Contested (B11a)");
    }

    /// Attack: DependsOnSuperseded conflict + fresh External + oracle ABSENT.
    /// DependsOnSuperseded is NOT a "contradiction" — it is a "dependent needs review" case.
    /// The gate should route to QueuedForAdjudication, NOT Contested.
    /// This verifies the oracle-absent contradiction path does not over-fire.
    #[test]
    fn p3_depends_on_superseded_oracle_absent_routes_to_queued_not_contested() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::DependsOnSuperseded,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        // DependsOnSuperseded is not a SameLine/CrossLine contradiction, so B11(a) does NOT fire.
        // It routes to QueuedForAdjudication regardless of oracle_present.
        assert_eq!(d.route, Route::HeavyPath);
        assert_eq!(d.disposition, Disposition::QueuedForAdjudication,
            "DependsOnSuperseded is NOT a fresh contradiction; B11(a) must not fire; expect QueuedForAdjudication");
        assert_ne!(d.disposition, Disposition::Contested,
            "B11(a) must not over-fire for DependsOnSuperseded");
    }

    /// Attack: DependsOnSuperseded + oracle PRESENT.
    /// Should also route to QueuedForAdjudication (oracle-present path).
    #[test]
    fn p3_depends_on_superseded_oracle_present_routes_to_queued() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::DependsOnSuperseded,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: true,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.disposition, Disposition::QueuedForAdjudication);
    }

    /// Attack: oracle_present=true must NEVER short-circuit to Contested.
    /// The oracle_present=true path must always produce QueuedForAdjudication on heavy path.
    #[test]
    fn p3_oracle_present_true_never_produces_contested() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        for conflict_type in [
            ConflictType::SameLineConflict,
            ConflictType::CrossLineConflict,
            ConflictType::DependsOnSuperseded,
            ConflictType::Succession,
        ] {
            let claim = ext_claim(vt.clone(), tx.clone());
            let proposal = Proposal {
                candidate: claim,
                incumbent: Some(incumbent()),
                conflict_type: conflict_type.clone(),
                measured_confidence: 0.9,
                cardinality_proposal: Cardinality::Functional,
                oracle_present: true,
            };
            let d = adjudicate(&proposal, &cfg());
            assert_ne!(d.disposition, Disposition::Contested,
                "oracle_present=true must NEVER produce Contested (B11b); fired for {:?}", conflict_type);
        }
    }

    /// Attack: ModelDerived + SameLineConflict + oracle absent.
    /// ModelDerived is caught at Step 3, BEFORE Step 5 (heavy path).
    /// ModelDerived routes to InferredRoute at Step 3, before the oracle-absent contradiction path.
    #[test]
    fn p3_model_derived_conflict_oracle_absent_routes_to_inferred_not_contested() {
        let claim = model_claim();
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::InferredRoute,
            "ModelDerived is caught at step 3 before step 5; heavy path must not fire");
        assert_ne!(d.disposition, Disposition::Contested,
            "ModelDerived must NEVER produce Contested (V3-4)");
    }

    // ══════════════════════════════════════════════════════════════════════════
    // P4 — CORROBORATION IS A MODIFIER ONLY, NEVER A ROUTE-FLIPPER
    // ══════════════════════════════════════════════════════════════════════════

    /// Attack: The gate's Proposal struct has NO corroboration_count field.
    /// Verify that no measured_confidence value — even extremely high (implying heavy
    /// corroboration) — causes a different ROUTE than the same case without corroboration.
    /// Route must be identical; only rationale values differ.
    #[test]
    fn p4_high_confidence_implying_corroboration_does_not_flip_route() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        // "Low corroboration" scenario: measured_confidence = 0.5
        let claim_low = ext_claim(vt.clone(), tx.clone());
        let proposal_low = Proposal {
            candidate: claim_low,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.5,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };

        // "Heavy corroboration" scenario: measured_confidence = 0.999
        let claim_high = ext_claim(vt.clone(), tx.clone());
        let proposal_high = Proposal {
            candidate: claim_high,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.999,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };

        let d_low = adjudicate(&proposal_low, &cfg());
        let d_high = adjudicate(&proposal_high, &cfg());

        assert_eq!(d_low.route, d_high.route,
            "measured_confidence (proxy for corroboration) must not change Route");
        assert_eq!(d_low.disposition, d_high.disposition,
            "measured_confidence must not change Disposition");
    }

    /// Attack: verify corroboration_count is hardcoded 0 in rationale (since it has no gate
    /// input). The rationale must always record 0 — it is a logged annotation, not a gate input.
    #[test]
    fn p4_rationale_records_corroboration_count_zero_always() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: true, // oracle present → QueuedForAdjudication rationale branch
        };
        let d = adjudicate(&proposal, &cfg());
        let corr = d.rationale.get("corroboration_count");
        assert!(
            corr.is_some() && corr.unwrap() == 0,
            "corroboration_count must be 0 in rationale (modifier-only, no gate input). Got: {:?}",
            d.rationale
        );
    }

    /// Attack: verify that varying measured_confidence in the rationale-only heavy path
    /// (oracle present) produces the same route but different rationale values.
    #[test]
    fn p4_measured_confidence_only_affects_rationale_not_route() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        let confidences = [0.0f32, 0.5, 0.7, 0.99, 1.0];
        let routes: Vec<Route> = confidences.iter().map(|&mc| {
            let claim = ext_claim(vt.clone(), tx.clone());
            let proposal = Proposal {
                candidate: claim,
                incumbent: Some(incumbent()),
                conflict_type: ConflictType::SameLineConflict,
                measured_confidence: mc,
                cardinality_proposal: Cardinality::Functional,
                oracle_present: true,
            };
            adjudicate(&proposal, &cfg()).route
        }).collect();

        let first = &routes[0];
        for (i, route) in routes.iter().enumerate() {
            assert_eq!(route, first,
                "measured_confidence[{}]={} produced different route {:?} vs {:?}",
                i, confidences[i], route, first);
        }
    }

    // ══════════════════════════════════════════════════════════════════════════
    // P5 — ROUTING COMPLETENESS: EVERY PROPOSAL MAPS TO EXACTLY ONE ROUTE
    // ══════════════════════════════════════════════════════════════════════════

    /// Attack: enumerate every ConflictType x oracle_present combination for External provenance
    /// with an incumbent. No combination must panic or produce an unhandled case.
    #[test]
    fn p5_all_conflict_types_produce_valid_route_no_panic() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        let all_conflict_types = [
            ConflictType::NoConflict,
            ConflictType::SameLineConflict,
            ConflictType::CrossLineConflict,
            ConflictType::DependsOnSuperseded,
            ConflictType::Succession,
        ];

        for conflict_type in &all_conflict_types {
            for oracle_present in [false, true] {
                let claim = ext_claim(vt.clone(), tx.clone());
                let proposal = Proposal {
                    candidate: claim,
                    incumbent: Some(incumbent()),
                    conflict_type: conflict_type.clone(),
                    measured_confidence: 0.9,
                    cardinality_proposal: Cardinality::Functional,
                    oracle_present,
                };
                // Must not panic; result must be a valid route
                let d = adjudicate(&proposal, &cfg());
                let valid = matches!(
                    d.route,
                    Route::CheapPath
                        | Route::InferredRoute
                        | Route::RecallTainted
                        | Route::HeavyPath
                        | Route::Quarantine
                );
                assert!(valid,
                    "unexpected/invalid route {:?} for {:?} oracle={}", d.route, conflict_type, oracle_present);
            }
        }
    }

    /// Attack: all provenance types with NoConflict + no incumbent.
    /// Every provenance must map to a valid route without panic.
    #[test]
    fn p5_all_provenance_types_no_conflict_no_incumbent_produce_valid_route() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };

        let provenances = [
            ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ProvenanceLabel::RecallReEntry,
            ProvenanceLabel::ModelDerived,
        ];

        for prov in &provenances {
            let claim = make_claim_with_confidence(prov.clone(), vt.clone(), tx.clone(), 0.0);
            let proposal = Proposal {
                candidate: claim,
                incumbent: None,
                conflict_type: ConflictType::NoConflict,
                measured_confidence: 0.9,
                cardinality_proposal: Cardinality::Functional,
                oracle_present: false,
            };
            let d = adjudicate(&proposal, &cfg());
            let valid = matches!(
                d.route,
                Route::CheapPath | Route::InferredRoute | Route::RecallTainted | Route::HeavyPath | Route::Quarantine
            );
            assert!(valid, "invalid route for provenance {:?}: {:?}", prov, d.route);
        }
    }

    /// Attack: NoConflict with incumbent present — should still take CheapPath.
    /// Step 4 condition is `conflict_type == NoConflict || incumbent.is_none()`.
    /// NoConflict trumps the incumbent presence.
    #[test]
    fn p5_no_conflict_with_incumbent_still_routes_cheap_path() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()), // incumbent IS present
            conflict_type: ConflictType::NoConflict, // but no conflict declared
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::CheapPath,
            "NoConflict with incumbent present must still take CheapPath (step 4: NoConflict OR no incumbent)");
        assert_eq!(d.disposition, Disposition::CommittedCheap);
    }

    /// Attack: SameLineConflict with NO incumbent (None). Step 4 fires (`incumbent.is_none()`).
    /// This should route CheapPath, not HeavyPath — no incumbent means no belief to overturn.
    #[test]
    fn p5_same_line_conflict_no_incumbent_routes_cheap_path() {
        let tx = tx_at(2026, 6, 1);
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        let claim = ext_claim(vt, tx);
        let proposal = Proposal {
            candidate: claim,
            incumbent: None, // no incumbent
            conflict_type: ConflictType::SameLineConflict, // conflict declared but no one to conflict with
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::CheapPath,
            "SameLineConflict with no incumbent must route CheapPath (step 4: incumbent.is_none())");
    }

    /// Attack: RecallReEntry with a conflict type that would otherwise trigger heavy path.
    /// Step 1 (RecallReEntry) must fire BEFORE any conflict-type branching.
    #[test]
    fn p5_recall_reentry_with_heavy_conflict_type_still_routes_recall_tainted() {
        let claim = recall_claim();
        let proposal = Proposal {
            candidate: claim,
            incumbent: Some(incumbent()),
            conflict_type: ConflictType::SameLineConflict,
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
        };
        let d = adjudicate(&proposal, &cfg());
        assert_eq!(d.route, Route::RecallTainted,
            "RecallReEntry is always caught at step 1; must not reach heavy path even with SameLineConflict");
        assert_eq!(d.disposition, Disposition::CommittedCheap);
    }
}
