#![allow(missing_docs)]
//! Contradiction Detector / Reconciler.
//!
//! Classifies contradictions and builds gate Proposals.
//! Never commits — all disposition decisions are made by the adjudication gate.
//!
//! # Separation of concerns
//! - Reconciler PROPOSES: determines `ConflictType` and assembles a `Proposal` for the gate.
//! - Gate ADJUDICATES: receives the `Proposal` and applies the deterministic routing logic.
//!
//! # ConflictType classification
//! - `NoConflict`          — no existing belief on this subject-line (first write).
//! - `SameLineConflict`    — same (subject, predicate), different value.
//! - `CrossLineConflict`   — mutual exclusion or entailment violation across predicates.
//! - `DependsOnSuperseded` — candidate's derived_from lineage includes a superseded claim.
//!
//! # Determinism
//! The `ConflictType` classification is deterministic given fixed inputs.
//! The `measured_confidence` field may be populated by a stochastic LLM call at a higher
//! layer; this module accepts it as a parameter and records it unchanged.
//!
//! Replay-audit compliance: given the same `StampedClaim` + same incumbent + same config, the
//! `Proposal` produced is byte-identical. The only stochastic input is `measured_confidence`,
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
///   this set to detect `DependsOnSuperseded`.
///
/// `measured_confidence` — confidence score from the stochastic extractor, recorded to the ledger.
///   Populated at a higher layer; the reconciler records it into the Proposal without re-sampling.
///
/// `oracle_present` — whether the `OraclePort` has a registered listener.
///   Passed in from the engine wrapper; the reconciler threads it into the Proposal for the gate.
///
/// `succession_threshold` — the `valid_time_confidence_threshold` from EngineConfig; used to
///   determine whether the candidate + incumbent form a trusted temporal succession.
///
/// `all_live_claims` — the RAW-live claims on this subject-line (`FoldResult.all_claims`
///   filtered `is_live`), INCLUDING the candidate itself when the candidate is already one of
///   them (e.g. the `reconcile()` use-case iterates candidates drawn from this same set).
///   `classify_conflict` filters the candidate out by `claim_ref` internally — callers never
///   need to pre-filter. Succession is only granted when the candidate is a trusted,
///   non-overlapping window against EVERY OTHER raw-live claim here, not just one incumbent —
///   this is the single fix for the silent-chain-overlap defect (a challenger overlapping a
///   non-current member of an existing succession chain must be `SameLineConflict`, never a
///   cheap-pathed `Succession`).
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
    /// Raw-live claims on this subject-line (may include the candidate; self is filtered
    /// internally). See field doc above.
    pub all_live_claims: &'a [Claim],
}

/// Detect the conflict type between a candidate claim and the incumbent belief on the same
/// subject-line, then build the gate Proposal.
///
/// # Conflict classification (deterministic given inputs):
/// 1. No incumbent → `NoConflict`.
/// 2. Candidate's derived_from intersects superseded_claim_refs → `DependsOnSuperseded`.
/// 3. Same (subject, predicate) as incumbent, different value → `SameLineConflict`.
///    3a. If candidate + incumbent form a trusted temporal succession (non-overlapping windows, confidence >= threshold, N=1 live incumbent) → `Succession` instead.
/// 4. Cross-predicate mutual exclusion (subject matches, predicate differs but facts are
///    mutually exclusive by declaration) → `CrossLineConflict`.
///    In v0.1: detected via the MutualExclusion edge kind; without edges we classify as
///    `SameLineConflict` if same-predicate or `NoConflict` if different-predicate
///    (cross-line edges via MutualExclusion edge kind are a future feature; see RECOMMENDATIONS).
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
/// 3. Same (subject, predicate) + same JSON value AND the candidate's window does not
///    overlap any OTHER live claim (`all_live_claims`) with a DIFFERENT value → NoConflict
///    (idempotent re-statement). Identical value overlapping a differently-valued OLDER live
///    claim falls through to step 4 instead (TASK-33-W5-LIB-R1).
/// 4. Same (subject, predicate) + different value:
///    4a. Candidate forms a trusted, pairwise-non-overlapping succession against EVERY OTHER
///        raw-live claim on the subject-line (N-wide, not just one incumbent) → Succession.
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
        //
        // TASK-33-W5-LIB-R1 (review blocker 1, step-3 refinement): the identical-value
        // shortcut below is a legitimate re-assertion ONLY when the candidate's window does
        // not overlap any OTHER live claim (`all_live_claims`) whose value differs from the
        // candidate's. Re-affirming the CURRENT incumbent's value while silently overlapping
        // an OLDER, different-valued succession member (e.g. candidate "Paris" [2021,∞)
        // overlapping A="Berlin" [2020,2022) even though the current incumbent B is also
        // "Paris") must still be routed through the N-wide overlap check (step 4) — identical
        // value against the CURRENT incumbent is not evidence about what was true during a
        // window that belonged to a DIFFERENT, earlier claim. Overlapping the incumbent
        // itself (same value) never disqualifies the shortcut.
        //
        // Precondition: `.start.is_some()` gates the check on BOTH sides (TASK-33-W5-LIB-R2
        // nit). A claim with no stated `valid_time.start` has no closed-form window to test
        // via `windows_non_overlapping` (open-start windows are only ever narrowed/ordered by
        // `bound_at`, never compared here) — such a claim is simply excluded from the
        // differently-valued-overlap scan rather than being treated as trivially overlapping
        // or non-overlapping. Same guard is used on `other` for the identical reason.
        let candidate_ref = input.candidate.claim_ref();
        let overlaps_different_value = input.candidate.valid_time().start.is_some()
            && input.all_live_claims.iter().any(|other| {
                other.claim_ref() != candidate_ref
                    && &other.fact().value != cand_value
                    && other.valid_time().start.is_some()
                    && !valid_time_helpers::windows_non_overlapping(input.candidate, other)
            });

        if cand_value == incumb_value && !overlaps_different_value {
            // Step 3: identical value, no overlap with a differently-valued claim —
            // idempotent re-statement, not a contradiction.
            ConflictType::NoConflict
        } else {
            // Step 4: different value (or identical value overlapping a differently-valued
            // claim) — check for trusted temporal succession.
            //
            // N-wide check (fixes the silent chain-overlap defect): the candidate must form a
            // trusted succession against EVERY raw-live claim on the subject-line, not just the
            // single "current" incumbent. Uses the SAME `is_trusted_succession` primitive the
            // fold itself uses for narrowing — one algorithm, shared by ingest, reconcile, and
            // history (I8 single source of truth). Self (candidate) is filtered out of
            // `all_live_claims` by claim_ref before the group is assembled.
            let threshold = input.succession_threshold;
            let candidate_ref = input.candidate.claim_ref();
            let mut group: Vec<&Claim> = Vec::with_capacity(input.all_live_claims.len() + 1);
            group.push(input.candidate);
            for c in input.all_live_claims {
                if c.claim_ref() != candidate_ref {
                    group.push(c);
                }
            }
            let is_succession = valid_time_helpers::is_trusted_succession(&group, threshold);

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
        // In v0.1, cross-line mutual exclusion edges are not implemented.
        // We conservatively classify as CrossLineConflict only when subjects match but
        // predicates differ AND the subjects are identical (same entity, different attribute).
        // This is a structural cross-line relationship — the gate handles the adjudication.
        //
        // IMPORTANT: this is a CONSERVATIVE classification. Without MutualExclusion edges,
        // we detect structural cross-line relationships by subject-match/predicate-diff.
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
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None}
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
        // Candidates built via `make_claim` always carry `no_vt()` (untrusted valid-time), so an
        // empty `all_live_claims` is safe here: `is_trusted_succession` fails on the candidate's
        // own trust check regardless of group membership, matching these tests' SameLineConflict
        // expectations. Tests exercising the N-wide succession check build ReconcilerInput directly.
        ReconcilerInput {
            candidate,
            incumbent,
            superseded_claim_refs: superseded,
            measured_confidence: 0.85,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: oracle,
            succession_threshold: 0.7,
            all_live_claims: &[],
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
            all_live_claims: &[],
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
            all_live_claims: &[],
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

    // ── W1b — valid-time-aware succession reconciler ──────────────────────────

    /// Helper: build a Belief (incumbent) with trusted valid-time window.
    fn make_belief_with_vt(
        subject: &str,
        predicate: &str,
        value: serde_json::Value,
        vt_start: Option<chrono::DateTime<chrono::Utc>>,
        vt_end: Option<chrono::DateTime<chrono::Utc>>,
        vt_confidence: f32,
    ) -> Belief {
        Belief {
            claim_ref: ClaimRef::new_random(),
            fact: Fact {
                subject: subject.into(),
                predicate: predicate.into(),
                value,
            },
            provenance: mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted),
            valid_time: ValidTime { start: vt_start, end: vt_end, valid_time_confidence: vt_confidence, start_granularity: None, end_granularity: None },
            transaction_time: tx_past(),
            confidence: mempill_types::Confidence { value_confidence: 0.8, valid_time_confidence: vt_confidence },
            currency_signal: mempill_types::CurrencySignal {
                last_refreshed_at: tx_past(),
                state: mempill_types::CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: mempill_types::Criticality::Medium,
        }
    }

    /// Helper: build a candidate Claim with trusted valid-time window.
    fn make_claim_with_vt(
        subject: &str,
        predicate: &str,
        value: serde_json::Value,
        vt_start: Option<chrono::DateTime<chrono::Utc>>,
        vt_end: Option<chrono::DateTime<chrono::Utc>>,
        vt_confidence: f32,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            mempill_types::AgentId("agent-rc".into()),
            Fact { subject: subject.into(), predicate: predicate.into(), value },
            Cardinality::Functional,
            mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::ExternalFirstHand),
            mempill_types::ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            tx(),
            ValidTime { start: vt_start, end: vt_end, valid_time_confidence: vt_confidence, start_granularity: None, end_granularity: None },
            mempill_types::Confidence { value_confidence: 0.9, valid_time_confidence: vt_confidence },
            mempill_types::Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn dt(year: i32, month: u32, day: u32) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    /// Non-overlapping, high-confidence windows → Succession (NOT SameLineConflict).
    #[test]
    fn w1b_non_overlapping_confident_windows_is_succession() {
        // Incumbent: [Jan, Mar) — City=Berlin
        // Candidate: [Mar, ∞)  — City=Paris
        // Both windows trusted (0.9 >= 0.7 threshold) and non-overlapping.
        let candidate = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 3, 1)), None, 0.9,
        );
        let incumbent = make_belief_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9,
        );
        let incumbent_claim = make_claim_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9,
        );
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: Some(&incumbent),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: std::slice::from_ref(&incumbent_claim),
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::Succession,
            "non-overlapping trusted windows must classify as Succession (not SameLineConflict)"
        );
    }

    /// Overlapping high-confidence windows → SameLineConflict (not Succession).
    #[test]
    fn w1b_overlapping_confident_windows_is_same_line_conflict() {
        // Incumbent: [Jan, Apr) — City=Berlin
        // Candidate: [Mar, ∞)  — City=Paris (overlap: Mar in both windows)
        let candidate = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 3, 1)), None, 0.9,
        );
        let incumbent = make_belief_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 4, 1)), 0.9, // ends Apr → overlaps [Mar, ∞)
        );
        let incumbent_claim = make_claim_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 4, 1)), 0.9,
        );
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: Some(&incumbent),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: std::slice::from_ref(&incumbent_claim),
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::SameLineConflict,
            "overlapping windows must remain SameLineConflict"
        );
    }

    /// Low valid-time confidence → SameLineConflict (even if windows don't overlap).
    #[test]
    fn w1b_low_confidence_windows_is_same_line_conflict() {
        // Candidate window: low confidence (0.3) → not trusted → not Succession.
        let candidate = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 3, 1)), None, 0.3, // below threshold
        );
        let incumbent = make_belief_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9, // incumbent trusted
        );
        let incumbent_claim = make_claim_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9,
        );
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: Some(&incumbent),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: std::slice::from_ref(&incumbent_claim),
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::SameLineConflict,
            "low valid_time_confidence → not trusted → SameLineConflict (not Succession)"
        );
    }

    /// Succession routes to CheapPath via the gate (NOT HeavyPath / Contested).
    #[test]
    fn w1b_succession_routes_cheap_path_in_gate() {
        use crate::engine::gate::{adjudicate, Route};

        let candidate = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 3, 1)), None, 0.9,
        );
        let incumbent = make_belief_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9,
        );
        let incumbent_claim = make_claim_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 3, 1)), 0.9,
        );
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: Some(&incumbent),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: std::slice::from_ref(&incumbent_claim),
        };
        let proposal = reconcile(inp, &cfg());
        let decision = adjudicate(&proposal, &cfg());
        assert_eq!(
            decision.route, Route::CheapPath,
            "Succession must route CheapPath (not HeavyPath/Contested)"
        );
    }

    // ── N-wide succession check: silent chain-overlap regression (DIAG_silent_succession) ──

    /// A challenger overlapping a NON-current member of an existing trusted succession chain
    /// must be SameLineConflict against the full raw-live set, never a cheap-pathed Succession.
    ///
    /// Chain: Linda [2024-09-23, 2026-01-24) -> John [2026-01-24, ∞) — a clean trusted succession.
    /// Challenger: Joan [2024-09-01, 2025-11-01) — overlaps Linda's window, even though it is
    /// non-overlapping against John (the "current" member alone). The old 2-claim reconciler
    /// only ever compared against ONE incumbent and silently cheap-pathed this as Succession.
    #[test]
    fn n_wide_succession_challenger_overlaps_noncurrent_chain_member_is_same_line_conflict() {
        let joan = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("Joan"),
            Some(dt(2024, 9, 1)), Some(dt(2025, 11, 1)), 0.9,
        );
        let linda = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("Linda"),
            Some(dt(2024, 9, 23)), Some(dt(2026, 1, 24)), 0.9,
        );
        let john = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("John"),
            Some(dt(2026, 1, 24)), None, 0.9,
        );
        // "incumbent" (legacy field) is whichever the caller treats as the fold's first live
        // claim — content doesn't affect the succession check, only step 1's None-check.
        let incumbent_belief = make_belief_with_vt(
            "acme", "ceo", serde_json::json!("Linda"),
            Some(dt(2024, 9, 23)), Some(dt(2026, 1, 24)), 0.9,
        );
        let all_live = vec![linda.clone(), john.clone()];
        let inp = ReconcilerInput {
            candidate: &joan,
            incumbent: Some(&incumbent_belief),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: &all_live,
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::SameLineConflict,
            "Joan overlaps Linda (non-current chain member) → must be SameLineConflict, \
             never a silently cheap-pathed Succession (I7)"
        );
    }

    /// Same chain shape, but the challenger is non-overlapping against EVERY member —
    /// must still classify as Succession (N-wide check does not over-fire).
    #[test]
    fn n_wide_succession_challenger_non_overlapping_all_members_is_succession() {
        let linda = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("Linda"),
            Some(dt(2024, 9, 23)), Some(dt(2026, 1, 24)), 0.9,
        );
        let john = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("John"),
            Some(dt(2026, 1, 24)), Some(dt(2027, 1, 1)), 0.9,
        );
        let sam = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("Sam"),
            Some(dt(2027, 1, 1)), None, 0.9,
        );
        let incumbent_belief = make_belief_with_vt(
            "acme", "ceo", serde_json::json!("Linda"),
            Some(dt(2024, 9, 23)), Some(dt(2026, 1, 24)), 0.9,
        );
        let all_live = vec![linda.clone(), john.clone()];
        let inp = ReconcilerInput {
            candidate: &sam,
            incumbent: Some(&incumbent_belief),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: &all_live,
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::Succession,
            "Sam is non-overlapping against every raw-live chain member → Succession"
        );
    }

    /// Zero-width (point) challenger window overlapping a non-current chain member must also
    /// be SameLineConflict — `windows_non_overlapping`'s half-open arithmetic already handles
    /// degenerate `start == end` windows correctly; this is a regression test at N>1.
    #[test]
    fn n_wide_succession_point_claim_challenger_overlaps_noncurrent_member_is_same_line_conflict() {
        let point_claim = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("Joan"),
            Some(dt(2025, 1, 1)), Some(dt(2025, 1, 1)), 0.9, // start == end: zero-width window
        );
        let linda = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("Linda"),
            Some(dt(2024, 9, 23)), Some(dt(2026, 1, 24)), 0.9,
        );
        let john = make_claim_with_vt(
            "acme", "ceo", serde_json::json!("John"),
            Some(dt(2026, 1, 24)), None, 0.9,
        );
        let incumbent_belief = make_belief_with_vt(
            "acme", "ceo", serde_json::json!("Linda"),
            Some(dt(2024, 9, 23)), Some(dt(2026, 1, 24)), 0.9,
        );
        let all_live = vec![linda.clone(), john.clone()];
        let inp = ReconcilerInput {
            candidate: &point_claim,
            incumbent: Some(&incumbent_belief),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: &all_live,
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::SameLineConflict,
            "zero-width challenger window inside Linda's window must still be SameLineConflict"
        );
    }

    // ── Step 3 refinement: identical-value shortcut must not wave through an overlap with
    // an OLDER, differently-valued live claim (TASK-33-W5-LIB-R1 review blocker 1) ─────────

    /// Candidate value matches the CURRENT incumbent B ("Paris"), but the candidate's window
    /// overlaps A ("Berlin"), an OLDER, differently-valued member of `all_live_claims`. The
    /// step-3 identical-value shortcut must NOT wave this through as NoConflict — it must fall
    /// through to the N-wide check (step 4), which correctly classifies it SameLineConflict
    /// because it overlaps A.
    #[test]
    fn step3_identical_value_overlapping_older_different_value_claim_is_same_line_conflict() {
        let a = make_claim_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 2, 1)), 0.9,
        );
        let b = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 2, 1)), None, 0.9,
        );
        let incumbent_b = make_belief_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 2, 1)), None, 0.9,
        );
        // Candidate: same value as B ("Paris"), but window starts inside A's window.
        let candidate = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 1, 15)), None, 0.9,
        );
        let all_live = vec![a.clone(), b.clone()];
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: Some(&incumbent_b),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: &all_live,
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::SameLineConflict,
            "identical value to the CURRENT incumbent must not skip the N-wide check when the \
             window overlaps an OLDER, differently-valued live claim (silent-commit regression)"
        );
    }

    /// Negative control: candidate value matches incumbent B ("Paris") and its window overlaps
    /// ONLY B (not A) — a legitimate re-assertion. Step 3's shortcut must still fire.
    #[test]
    fn step3_identical_value_no_overlap_with_different_value_claim_is_no_conflict() {
        let a = make_claim_with_vt(
            "user", "city", serde_json::json!("Berlin"),
            Some(dt(2024, 1, 1)), Some(dt(2024, 2, 1)), 0.9,
        );
        let b = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 2, 1)), None, 0.9,
        );
        let incumbent_b = make_belief_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 2, 1)), None, 0.9,
        );
        // Candidate: same value as B, window starts AFTER A's window closes — overlaps only B.
        let candidate = make_claim_with_vt(
            "user", "city", serde_json::json!("Paris"),
            Some(dt(2024, 3, 1)), None, 0.9,
        );
        let all_live = vec![a.clone(), b.clone()];
        let inp = ReconcilerInput {
            candidate: &candidate,
            incumbent: Some(&incumbent_b),
            superseded_claim_refs: &[],
            measured_confidence: 0.9,
            cardinality_proposal: Cardinality::Functional,
            oracle_present: false,
            succession_threshold: 0.7,
            all_live_claims: &all_live,
        };
        let proposal = reconcile(inp, &cfg());
        assert_eq!(
            proposal.conflict_type, ConflictType::NoConflict,
            "identical value overlapping ONLY the current incumbent's own window remains a \
             legitimate idempotent re-assertion"
        );
    }
}
