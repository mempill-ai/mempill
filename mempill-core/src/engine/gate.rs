//! C7 — Adjudication Gate (TECHNICAL_DESIGN.md §6, A6, A24, A25).
//!
//! The deterministic pure function at the stochastic/deterministic boundary.
//! INVARIANT (G1): same `Proposal` + same `EngineConfig` → byte-identical `GateDecision`.
//! No system clock reads, no RNG, no I/O, no HashMap iteration-order dependence inside
//! `adjudicate()`. All timestamps enter via the `Proposal`; none are sampled here.

use mempill_types::{Cardinality, Claim, Disposition, ProvenanceLabel};
use crate::config::EngineConfig;

/// The LLM-emitted proposal. Crosses the stochastic/deterministic boundary.
/// The gate consumes this; nothing downstream re-calls the LLM.
///
/// `oracle_present` (A24): lives HERE in gate.rs — never in mempill-types.
/// When false and evidence is fresh first-hand external, Contested fires immediately (B11).
#[derive(Debug, Clone)]
pub(crate) struct Proposal {
    /// The candidate claim, already stamped by C1 (gateway.rs).
    pub candidate: Claim,
    /// None = no active belief on this subject-line (first write).
    pub incumbent: Option<mempill_types::Belief>,
    pub conflict_type: ConflictType,
    /// C3-measured disposition confidence (stochastic input; recorded to ledger).
    pub measured_confidence: f32,
    pub cardinality_proposal: Cardinality,
    /// Whether the OraclePort has a registered listener at decision time (B11, V3-5, A24).
    /// Passed in from the engine wrapper so `adjudicate()` remains a pure function.
    /// When false + fresh first-hand external contradiction: Contested fires immediately.
    /// When true: route to QueuedForAdjudication (oracle receives AdjudicationRequest).
    pub oracle_present: bool,
}

/// Conflict classification emitted by C3 (reconciler) and consumed by C7 (gate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConflictType {
    /// New non-conflicting claim — no existing belief on this subject-line.
    NoConflict,
    /// Same (subject, predicate), different value — belief-overturning candidate.
    SameLineConflict,
    /// Mutual-exclusion or entailment across predicates.
    CrossLineConflict,
    /// Parent was superseded; dependent needs review (V3-8).
    DependsOnSuperseded,
}

/// Gate decision — deterministic function of Proposal + EngineConfig.
/// PURE: same input → same GateDecision. The LLM is not re-called here.
/// All inputs and the output are recorded to the ledger for G1 replay audit.
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
    /// ModelDerived → CommittedInferred (down-weighted, V3-4).
    InferredRoute,
    /// RecallReEntry → corroborate-by-identity; no new claim.
    RecallTainted,
    /// Belief-overturning → async adjudication (QueuedForAdjudication or Contested).
    HeavyPath,
    /// Incoherent tx/valid or burst detected → parked, auditable, not destroyed.
    Quarantine,
}

/// Deterministic gate decision function (TECHNICAL_DESIGN.md §6).
///
/// PURE FUNCTION — no side effects, no I/O, no clock reads.
/// Corroboration is a CONFIDENCE MODIFIER only — it adjusts confidence logged to rationale
/// but does NOT by itself flip the route or disposition (F2, A6).
///
/// Decision order (execute in this order; return on first match):
/// 1. RecallReEntry  → RecallTainted / CommittedCheap
/// 2. Temporal coherence check (B7, A25) → Quarantine if incoherent
/// 3. ModelDerived   → InferredRoute / CommittedInferred
/// 4. No conflict or no incumbent → CheapPath / CommittedCheap
/// 5. Heavy path with B11 oracle-absent branching
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

/// Temporal coherence check (B7, A25, F4).
///
/// Returns `true` iff the claim's valid_time window is incoherent AND
/// `valid_time_confidence` is at or above the threshold (below threshold → treat
/// as unknown; not incoherent, just uncertain).
///
/// Two incoherence conditions (A25):
/// 1. `valid_time_start > valid_time_end` — physically impossible window.
/// 2. `valid_time_start > tx_time`        — a fact dated as valid AFTER it was learned.
///    ("valid-start AFTER it was learned" — closes the case where an extractor assigns
///     a future valid-start to a past fact.)
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
