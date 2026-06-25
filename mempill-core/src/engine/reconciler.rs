//! C3 — Contradiction Detector / Reconciler (TECHNICAL_DESIGN.md §9, I5, I7).
//!
//! STOCHASTIC PROPOSER — this module classifies contradictions and builds gate Proposals.
//! It NEVER commits. All disposition decisions are made by C7 (gate.rs::adjudicate()).
//!
//! # Separation of concerns
//! - Reconciler PROPOSES: determines ConflictType and assembles a Proposal for the gate.
//! - Gate ADJUDICATES: receives the Proposal and applies the deterministic routing logic.
//!
//! # ConflictType classification (reuses types from gate.rs — no redefinition)
//! - `NoConflict`            — no existing belief on this subject-line (first write).
//! - `SameLineConflict`      — same (subject, predicate), different value.
//! - `CrossLineConflict`     — mutual exclusion or entailment violation across predicates.
//! - `DependsOnSuperseded`   — candidate's derived_from lineage includes a superseded claim.
//!
//! # Determinism note
//! The reconciler's ConflictType classification given fixed inputs is deterministic.
//! The `measured_confidence` field may be populated by a stochastic LLM call at a higher
//! layer; this module accepts it as an input parameter and records it unchanged.
//!
//! G1 compliance: given the same StampedClaim + same incumbent + same config, the
//! Proposal produced is byte-identical (same ConflictType, same cardinality_proposal,
//! same oracle_present value fed in). The only stochastic input is measured_confidence,
//! which is passed in, not sampled here.

use mempill_types::{Belief, Cardinality, Claim};
use crate::config::EngineConfig;
use crate::engine::gate::{ConflictType, Proposal};
use crate::engine::valid_time_helpers;

/// Input to the reconciler for a single candidate claim.
///
/// `incumbent` — the current canonical belief on this subject-line, if any.
///   `None` = first write to this (agent_id, subject, predicate) line.
///
/// `superseded_claim_refs` — set of ClaimRefs that are currently in Superseded/Invalidated
///   disposition. The reconciler checks whether the candidate's derived_from lineage intersects
///   this set to detect `DependsOnSuperseded` (V3-8).
///
/// `measured_confidence` — confidence score from the stochastic extractor (C3-recorded to ledger).
///   Populated at a higher layer; reconciler records it into the Proposal without re-sampling.
///
/// `oracle_present` — whether the OraclePort has a registered listener (B11, A24).
///   Passed in from the engine wrapper; reconciler threads it into the Proposal for the gate.
///
/// `succession_threshold` — the `valid_time_confidence_threshold` from EngineConfig; used to
///   determine whether the candidate + incumbent form a trusted temporal succession (TASK-11 §C).
///
/// `n_gt_1_live_incumbents` — true when the fold produced more than one live claim on this
///   subject-line. Resolution #2: when N>1 live incumbents exist, succession check is SKIPPED
///   (conservative: stay SameLineConflict). The single incumbent passed via `incumbent` is
///   still the fold's FIRST live claim for backward-compat.
#[derive(Debug)]
pub(crate) struct ReconcilerInput<'a> {
    pub candidate: &'a Claim,
    pub incumbent: Option<&'a Belief>,
    pub superseded_claim_refs: &'a [mempill_types::ClaimRef],
    pub measured_confidence: f32,
    pub cardinality_proposal: Cardinality,
    pub oracle_present: bool,
    /// valid_time_confidence_threshold from EngineConfig for succession detection.
    pub succession_threshold: f32,
    /// True when fold returned N>1 live claims (succession check disabled per resolution #2).
    pub n_gt_1_live_incumbents: bool,
}

/// Detect the conflict type between a candidate claim and the incumbent belief on the same
/// subject-line, then build the gate Proposal.
///
/// # Conflict classification (deterministic given inputs):
/// 1. No incumbent → `NoConflict`.
/// 2. Candidate's derived_from intersects superseded_claim_refs → `DependsOnSuperseded`.
/// 3. Same (subject, predicate) as incumbent, different value → `SameLineConflict`.
///    3a. If candidate + incumbent form a trusted temporal succession (non-overlapping windows,
///        confidence >= threshold, N=1 live incumbent) → `Succession` instead.
/// 4. Cross-predicate mutual exclusion (subject matches, predicate differs but facts are
///    mutually exclusive by declaration) → `CrossLineConflict`.
///    In v0.1: detected via the MutualExclusion edge kind; without edges we classify as
///    `SameLineConflict` if same-predicate or `NoConflict` if different-predicate
///    (cross-line edges are a W5 feature; see RECOMMENDATIONS).
/// 5. Same value as incumbent → `NoConflict` (value-identical re-statement, not a contradiction).
///
/// # Returns
/// A `Proposal` ready for `gate::adjudicate()`. No I/O, no commits.
pub(crate) fn reconcile(input: ReconcilerInput<'_>, _config: &EngineConfig) -> Proposal {
    let conflict_type = classify_conflict(&input);

    Proposal {
        candidate: input.candidate.clone(),
        incumbent: input.incumbent.cloned(),
        conflict_type,
        measured_confidence: input.measured_confidence,
        cardinality_proposal: input.cardinality_proposal,
        oracle_present: input.oracle_present,
    }
}

/// Classify the conflict between the candidate and the incumbent (if any).
///
/// PURE FUNCTION — deterministic given fixed inputs.
///
/// Decision order:
/// 1. No incumbent → NoConflict (first write).
/// 2. derived_from intersects superseded_claim_refs → DependsOnSuperseded.
/// 3. Same (subject, predicate) + same JSON value → NoConflict (idempotent re-statement).
/// 4. Same (subject, predicate) + different value:
///    4a. Exactly ONE live incumbent + both windows trusted-non-overlapping → Succession.
///    4b. Otherwise → SameLineConflict.
/// 5. Different predicate (on same subject) with mutual exclusion → CrossLineConflict.
/// 6. Different predicate without mutual exclusion → NoConflict.
fn classify_conflict(input: &ReconcilerInput<'_>) -> ConflictType {
    // Step 1: No incumbent → first write on this subject-line.
    let incumbent = match input.incumbent {
        Some(b) => b,
        None => return ConflictType::NoConflict,
    };

    // Step 2: DependsOnSuperseded — candidate's lineage references a superseded claim.
    // V3-8: when a parent claim is superseded, any claim derived from it is flagged PendingReview.
    let lineage = input.candidate.derived_from();
    for ancestor_ref in lineage {
        if input.superseded_claim_refs.contains(ancestor_ref) {
            return ConflictType::DependsOnSuperseded;
        }
    }

    // Step 3 + 4 + 5 + 6: compare subject-line and value.
    let cand_subject = &input.candidate.fact().subject;
    let cand_predicate = &input.candidate.fact().predicate;
    let cand_value = &input.candidate.fact().value;

    let incumb_subject = &incumbent.fact.subject;
    let incumb_predicate = &incumbent.fact.predicate;
    let incumb_value = &incumbent.fact.value;

    if cand_subject == incumb_subject && cand_predicate == incumb_predicate {
        // Same subject-line.
        if cand_value == incumb_value {
            // Step 3: identical value — idempotent re-statement, not a contradiction.
            ConflictType::NoConflict
        } else {
            // Step 4: same line, different value — check for trusted temporal succession.
            //
            // Resolution #2: only when there is exactly ONE live incumbent (the reconciler
            // receives a single `incumbent: Option<&Belief>` from the fold's first live claim).
            // If the fold produced N>1 live claims, the fold already set has_conflict=true
            // and the gate/disposition is determined by the fold; the reconciler here sees
            // only the FIRST live claim as incumbent. We conservatively skip the succession
            // check when `n_live_incumbents > 1` (caller passes this as a flag).
            let threshold = input.succession_threshold;
            let cand_vt = input.candidate.valid_time();
            let incumb_vt = &incumbent.valid_time;
            let is_succession = !input.n_gt_1_live_incumbents
                && valid_time_helpers::valid_time_is_trusted(cand_vt, threshold)
                && valid_time_helpers::valid_time_is_trusted(incumb_vt, threshold)
                && valid_time_helpers::valid_times_non_overlapping(cand_vt, incumb_vt);

            if is_succession {
                // Step 4a: clean temporal succession — NOT a conflict.
                ConflictType::Succession
            } else {
                // Step 4b: same line, different value, no clean succession → overturning.
                ConflictType::SameLineConflict
            }
        }
    } else if cand_subject == incumb_subject && cand_predicate != incumb_predicate {
        // Step 5/6: different predicate on same subject.
        // In v0.1, cross-line mutual exclusion edges are not implemented (W5 feature).
        // We conservatively classify as CrossLineConflict only when subjects match but
        // predicates differ AND the subjects are identical (same entity, different attribute).
        // This is a structural cross-line relationship — the gate handles the adjudication.
        //
        // IMPORTANT: this is a CONSERVATIVE classification. Without MutualExclusion edges
        // (W5), we detect structural cross-line relationships by subject-match/predicate-diff.
        // The gate will determine whether to route to heavy or cheap path.
        ConflictType::CrossLineConflict
    } else {
        // Unrelated subject or predicate — no conflict.
        ConflictType::NoConflict
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::gate::ConflictType;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, CurrencySignal,
        CurrencyState, ExternalAnchor, ExternalKind, Fact, ProvenanceLabel, TransactionTime,
        ValidTime,
    };
    use chrono::{TimeZone, Utc};

    // ── Shared helpers ────────────────────────────────────────────────────────

    fn tx() -> TransactionTime {
        TransactionTime(Utc.with_ymd_and_hms(2026, 6, 22, 0, 0, 0).unwrap())
    }

    fn tx_past() -> TransactionTime {
        TransactionTime(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
    }

    fn no_vt() -> ValidTime {
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 }
    }

    fn make_claim(
        subject: &str,
        predicate: &str,
        value: serde_json::Value,
        derived_from: Vec<ClaimRef>,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent-rc".into()),
            Fact {
                subject: subject.into(),
                predicate: predicate.into(),
                value,
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            tx(),
            no_vt(),
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            derived_from,
            None,
            None,
        )
    }

    fn make_belief(subject: &str, predicate: &str, value: serde_json::Value) -> Belief {
        Belief {
            claim_ref: ClaimRef::new_random(),
            fact: Fact {
                subject: subject.into(),
                predicate: predicate.into(),
                value,
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: no_vt(),
            transaction_time: tx_past(),
            confidence: Confidence { value_confidence: 0.8, valid_time_confidence: 0.0 },
            currency_signal: CurrencySignal {
                last_refreshed_at: tx_past(),
                state: CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: Criticality::Medium,
        }
    }

    fn cfg() -> EngineConfig {
        EngineConfig::default()
    }

    fn input<'a>(
        candidate: &'a Claim,
        incumbent: Option<&'a Belief>,
        superseded: &'a [ClaimRef],
        oracle: bool,
    ) -> ReconcilerInput<'a> {
        ReconcilerInput {
            candidate,
            incumbent,
            superseded_claim_refs: superseded,
            measured_confidence: 0.85,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: oracle,
            succession_threshold: 0.7,
            n_gt_1_live_incumbents: false,
        }
    }

    // ── SAME-LINE CONTRADICTION ───────────────────────────────────────────────

    #[test]
    fn same_line_different_value_is_same_line_conflict() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        let inp = input(&candidate, Some(&incumbent), &[], false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::SameLineConflict,
            "same (subject, predicate) with different values must be SameLineConflict");
    }

    #[test]
    fn same_line_conflict_proposal_carries_candidate_and_incumbent() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        let inp = input(&candidate, Some(&incumbent), &[], false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.candidate.fact().value, serde_json::json!("Paris"));
        assert!(proposal.incumbent.is_some());
        assert_eq!(
            proposal.incumbent.as_ref().unwrap().fact.value,
            serde_json::json!("Berlin")
        );
    }

    // ── CROSS-LINE CONFLICT ───────────────────────────────────────────────────

    #[test]
    fn cross_line_different_predicate_same_subject_is_cross_line_conflict() {
        let candidate = make_claim("user", "allergies", serde_json::json!("none"), vec![]);
        let incumbent = make_belief("user", "medications", serde_json::json!("penicillin"));
        let inp = input(&candidate, Some(&incumbent), &[], false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::CrossLineConflict,
            "same subject, different predicate → CrossLineConflict (v0.1 structural detection)");
    }

    #[test]
    fn cross_line_conflict_builds_correct_proposal_for_gate() {
        let candidate = make_claim("user", "country", serde_json::json!("France"), vec![]);
        let incumbent = make_belief("user", "location", serde_json::json!("Berlin"));
        let inp = input(&candidate, Some(&incumbent), &[], true);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::CrossLineConflict);
        assert!(proposal.oracle_present, "oracle_present must be threaded into the proposal");
    }

    // ── IDENTICAL VALUE → NO CONFLICT ────────────────────────────────────────

    #[test]
    fn same_line_same_value_is_no_conflict() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let incumbent = make_belief("user", "city", serde_json::json!("Paris"));
        let inp = input(&candidate, Some(&incumbent), &[], false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::NoConflict,
            "identical value re-statement must be NoConflict (idempotent write)");
    }

    #[test]
    fn same_line_same_value_different_type_is_same_line_conflict() {
        // JSON distinguishes "1" (string) from 1 (number) — different values even if visually similar.
        let candidate = make_claim("user", "age", serde_json::json!("30"), vec![]);
        let incumbent = make_belief("user", "age", serde_json::json!(30));
        let inp = input(&candidate, Some(&incumbent), &[], false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::SameLineConflict,
            "JSON type mismatch (string vs number) must be treated as different values → SameLineConflict");
    }

    // ── DEPENDS ON SUPERSEDED ─────────────────────────────────────────────────

    #[test]
    fn derived_from_superseded_claim_is_depends_on_superseded() {
        let superseded_ref = ClaimRef::new_random();
        let candidate = make_claim(
            "user", "city", serde_json::json!("Paris"),
            vec![superseded_ref.clone()],
        );
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        let superseded = vec![superseded_ref];
        let inp = input(&candidate, Some(&incumbent), &superseded, false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::DependsOnSuperseded,
            "candidate derived_from a superseded claim must classify as DependsOnSuperseded (V3-8)");
    }

    #[test]
    fn derived_from_non_superseded_claim_does_not_trigger_depends_on_superseded() {
        let ancestor_ref = ClaimRef::new_random();
        let candidate = make_claim(
            "user", "city", serde_json::json!("Paris"),
            vec![ancestor_ref.clone()],
        );
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        // superseded_claim_refs does NOT contain ancestor_ref
        let superseded: Vec<ClaimRef> = vec![];
        let inp = input(&candidate, Some(&incumbent), &superseded, false);
        let proposal = reconcile(inp, &cfg());
        // Should be SameLineConflict (same pred, different value), not DependsOnSuperseded.
        assert_eq!(proposal.conflict_type, ConflictType::SameLineConflict);
    }

    #[test]
    fn depends_on_superseded_fires_before_same_line_check() {
        // If derived_from a superseded claim, DependsOnSuperseded wins even if same-line conflict exists.
        let superseded_ref = ClaimRef::new_random();
        let candidate = make_claim(
            "user", "city", serde_json::json!("Paris"),
            vec![superseded_ref.clone()],
        );
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        let superseded = vec![superseded_ref];
        let inp = input(&candidate, Some(&incumbent), &superseded, false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::DependsOnSuperseded,
            "DependsOnSuperseded check (step 2) fires before same-line check (step 3/4)");
    }

    // ── NO INCUMBENT (FIRST WRITE) ────────────────────────────────────────────

    #[test]
    fn no_incumbent_is_no_conflict() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let inp = input(&candidate, None, &[], false);
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.conflict_type, ConflictType::NoConflict,
            "no incumbent = first write on subject-line = NoConflict");
        assert!(proposal.incumbent.is_none());
    }

    // ── PROPOSAL INTEGRITY ────────────────────────────────────────────────────

    #[test]
    fn proposal_carries_measured_confidence() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: None,
            superseded_claim_refs: &[],
            measured_confidence: 0.73,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            n_gt_1_live_incumbents: false,
        };
        let proposal = reconcile(inp, &cfg());
        assert!((proposal.measured_confidence - 0.73).abs() < f32::EPSILON,
            "measured_confidence must be threaded into the Proposal unchanged");
    }

    #[test]
    fn proposal_carries_oracle_present_flag() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let inp = input(&candidate, None, &[], true);
        let proposal = reconcile(inp, &cfg());
        assert!(proposal.oracle_present, "oracle_present=true must be in the Proposal (A24)");
    }

    #[test]
    fn proposal_carries_cardinality_proposal() {
        let candidate = make_claim("user", "tags", serde_json::json!(["rust"]), vec![]);
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: None,
            superseded_claim_refs: &[],
            measured_confidence: 0.8,
            cardinality_proposal: Cardinality::SetValued,
            oracle_present: false,
            succession_threshold: 0.7,
            n_gt_1_live_incumbents: false,
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(proposal.cardinality_proposal, Cardinality::SetValued);
    }

    // ── DETERMINISM ───────────────────────────────────────────────────────────

    #[test]
    fn reconcile_is_deterministic_same_line_conflict() {
        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        let cfg = cfg();

        let p1 = reconcile(input(&candidate, Some(&incumbent), &[], false), &cfg);
        let p2 = reconcile(input(&candidate, Some(&incumbent), &[], false), &cfg);

        assert_eq!(p1.conflict_type, p2.conflict_type,
            "reconcile() must be deterministic for SameLineConflict");
        assert_eq!(
            format!("{:?}", p1.conflict_type),
            format!("{:?}", p2.conflict_type)
        );
    }

    #[test]
    fn reconcile_is_deterministic_across_all_conflict_types() {
        let cfg = cfg();

        // No conflict (no incumbent)
        let c1 = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let p1a = reconcile(input(&c1, None, &[], false), &cfg);
        let p1b = reconcile(input(&c1, None, &[], false), &cfg);
        assert_eq!(p1a.conflict_type, p1b.conflict_type);

        // Same-line conflict
        let c2 = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let inc = make_belief("user", "city", serde_json::json!("Berlin"));
        let p2a = reconcile(input(&c2, Some(&inc), &[], false), &cfg);
        let p2b = reconcile(input(&c2, Some(&inc), &[], false), &cfg);
        assert_eq!(p2a.conflict_type, p2b.conflict_type);

        // Cross-line conflict
        let c3 = make_claim("user", "country", serde_json::json!("France"), vec![]);
        let inc3 = make_belief("user", "city", serde_json::json!("Berlin"));
        let p3a = reconcile(input(&c3, Some(&inc3), &[], false), &cfg);
        let p3b = reconcile(input(&c3, Some(&inc3), &[], false), &cfg);
        assert_eq!(p3a.conflict_type, p3b.conflict_type);

        // DependsOnSuperseded
        let sup_ref = ClaimRef::new_random();
        let c4 = make_claim("user", "city", serde_json::json!("Paris"), vec![sup_ref.clone()]);
        let inc4 = make_belief("user", "city", serde_json::json!("Berlin"));
        let sup = vec![sup_ref];
        let p4a = reconcile(input(&c4, Some(&inc4), &sup, false), &cfg);
        let p4b = reconcile(input(&c4, Some(&inc4), &sup, false), &cfg);
        assert_eq!(p4a.conflict_type, p4b.conflict_type);
    }

    // ── GATE INTEGRATION — verify Proposal is consumable by adjudicate() ─────

    #[test]
    fn reconciler_proposal_is_consumable_by_gate() {
        use crate::engine::gate::{adjudicate, Route};

        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let incumbent = make_belief("user", "city", serde_json::json!("Berlin"));
        let inp = input(&candidate, Some(&incumbent), &[], false);
        let proposal = reconcile(inp, &cfg());

        // The proposal must be consumable by the gate without panic.
        let decision = adjudicate(&proposal, &cfg());
        // SameLineConflict + External + oracle_absent → HeavyPath/Contested
        assert_eq!(decision.route, Route::HeavyPath);
    }

    #[test]
    fn no_conflict_proposal_routes_cheap_path_in_gate() {
        use crate::engine::gate::{adjudicate, Route};

        let candidate = make_claim("user", "city", serde_json::json!("Paris"), vec![]);
        let inp = input(&candidate, None, &[], false);
        let proposal = reconcile(inp, &cfg());
        let decision = adjudicate(&proposal, &cfg());
        assert_eq!(decision.route, Route::CheapPath);
    }
}
