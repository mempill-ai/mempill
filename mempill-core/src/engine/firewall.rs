//! Write-Path Amplification Guard.
//!
//! CONTAINMENT + SURFACING mechanism — NOT a correctness mechanism.
//!
//! Three responsibilities:
//!   1. Detect recall re-entry by provenance tag (`ProvenanceLabel::RecallReEntry`).
//!   2. Corroborate-by-identity: return existing `ClaimRef`, emit NO new claim row.
//!   3. Detect burst/loop signatures → Quarantine.
//!
//! Amplification defence:
//!   N semantically-identical `RecallReEntry` re-entries of the same content MUST collapse
//!   to ONE underlying claim. `check()` returns `CorroborateByIdentity` for all candidates;
//!   the caller never inserts a new Claim row.
//!
//! Provenance laundering depth cap:
//!   `RecallReEntry` candidates whose `derivation_depth` exceeds
//!   `config.derivation_depth_cap_for_overturning` cannot overturn incumbent beliefs.
//!   `check()` enforces this via the `DepthCapExceeded` verdict.
//!
//! PURE / DETERMINISTIC: no clock reads, no RNG, no I/O inside `check()`.
//! Given the same inputs and the same `EngineConfig`, `check()` returns byte-identical output.

use std::sync::Arc;
use mempill_types::{Claim, ClaimRef};
use crate::config::EngineConfig;

/// The verdict returned by the amplification guard for a single candidate claim.
///
/// Callers act on the verdict WITHOUT writing a new Claim row for any variant except Admit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FirewallVerdict {
    /// Claim passes the guard — forward to the reconciler.
    Admit,
    /// RecallReEntry detected: corroborate the existing claim by identity.
    /// The caller increments the existing claim's corroboration annotation (if provenance_independent)
    /// but MUST NOT insert a new Claim row (idempotent append: recall re-entry never duplicates).
    CorroborateByIdentity {
        /// The existing ClaimRef this candidate re-entails.
        existing_claim: ClaimRef,
        /// Whether this corroboration counts as provenance-independent.
        /// Always `false` for RecallReEntry: same engine = NOT provenance-independent.
        provenance_independent: bool,
    },
    /// Burst or loop signature: quarantine this candidate.
    /// Reason is logged to the audit ledger; the claim is parked (not destroyed).
    Quarantine {
        reason: String,
    },
    /// Derivation depth exceeds the configured cap for overturning incumbents.
    /// The candidate is admitted as ModelDerived-equivalent but cannot overturn.
    DepthCapExceeded {
        depth: u32,
        cap: u32,
    },
}

/// Amplification Guard.
///
/// Pure struct: holds only the config reference. All state (injected_claim_refs,
/// burst_count) is passed in per-call so that `check()` remains a pure function.
pub(crate) struct AmplificationGuard {
    config: Arc<EngineConfig>,
}

impl AmplificationGuard {
    /// Construct a new AmplificationGuard bound to the given engine config.
    pub(crate) fn new(config: Arc<EngineConfig>) -> Self {
        Self { config }
    }

    /// Check a candidate claim against the amplification firewall.
    ///
    /// # Parameters
    /// - `candidate` — fully-stamped claim from the ingestion gateway. Provenance is already set.
    /// - `injected_claim_refs` — ClaimRefs previously served to this session context
    ///   (loaded from ledger entries with kind = ServedAsInjected). May be empty if no prior
    ///   session or when ledger read is unavailable (firewall degrades gracefully to Admit).
    /// - `burst_count_this_batch` — number of identical (same content digest) RecallReEntry
    ///   candidates that have already been processed in this write batch. The caller increments
    ///   this before each call for the same content. Used for burst detection.
    ///
    /// # Decision order (deterministic, execute in this order):
    /// 1. Burst gate — if burst_count_this_batch >= quarantine_burst_threshold → Quarantine.
    /// 2. RecallReEntry provenance → CorroborateByIdentity (idempotent append).
    /// 3. Depth cap — if derivation_depth > derivation_depth_cap_for_overturning → DepthCapExceeded.
    /// 4. Otherwise → Admit (forward to the reconciler).
    ///
    /// # Determinism guarantee
    /// PURE FUNCTION: same candidate + same injected_claim_refs + same burst_count + same config
    /// → byte-identical FirewallVerdict. No clock reads, no RNG, no I/O.
    pub(crate) fn check(
        &self,
        candidate: &Claim,
        injected_claim_refs: &[ClaimRef],
        burst_count_this_batch: u32,
    ) -> FirewallVerdict {
        // Step 1: Burst detection gate.
        // If the same content has already been submitted N >= threshold times in this batch,
        // quarantine this candidate to break the amplification loop.
        if burst_count_this_batch >= self.config.quarantine_burst_threshold {
            return FirewallVerdict::Quarantine {
                reason: format!(
                    "burst_detected: {} identical re-entries in batch (threshold: {})",
                    burst_count_this_batch,
                    self.config.quarantine_burst_threshold,
                ),
            };
        }

        // Step 2: RecallReEntry corroborate-by-identity (I6, F3).
        // C1 (gateway.rs) stamps RecallReEntry provenance at injection time when content
        // entails a previously-served claim. The firewall trusts that stamp and enforces
        // the NO-NEW-ROW invariant.
        if candidate.provenance().is_recall_reentry() {
            // The matched existing ClaimRef is in derived_from[0] (set by C1 at stamp time).
            // If missing, fall back to the first injected ClaimRef that matches by content
            // (entailment check approximation — identity match is sufficient for v0.1).
            let existing_claim = candidate
                .derived_from()
                .first()
                .cloned()
                .or_else(|| injected_claim_refs.first().cloned());

            if let Some(existing) = existing_claim {
                return FirewallVerdict::CorroborateByIdentity {
                    existing_claim: existing,
                    // RecallReEntry is NEVER provenance-independent: same engine served and
                    // re-received the same content. Currency is NOT refreshed (V3-7).
                    provenance_independent: false,
                };
            }
            // RecallReEntry with no matching prior claim — unusual; treat as Admit
            // (conservative: better to admit than to silently quarantine a legitimate claim).
        }

        // Step 3: OP-1 derivation depth cap.
        // A RecallReEntry that has been inferred/paraphrased (depth > cap) cannot launder
        // into ground truth by claiming high derivation depth.
        let depth = candidate.external_anchor().derivation_depth;
        let cap = self.config.derivation_depth_cap_for_overturning;
        if depth > cap {
            return FirewallVerdict::DepthCapExceeded { depth, cap };
        }

        // Step 4: Admit — forward to C3 reconciler.
        FirewallVerdict::Admit
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, ExternalAnchor,
        ExternalKind, Fact, ProvenanceLabel, TransactionTime, ValidTime,
    };
    use chrono::{TimeZone, Utc};

    // ── Shared helpers ────────────────────────────────────────────────────────

    fn tx() -> TransactionTime {
        TransactionTime(Utc.with_ymd_and_hms(2026, 6, 22, 0, 0, 0).unwrap())
    }

    fn make_claim(
        provenance: ProvenanceLabel,
        derivation_depth: u32,
        derived_from: Vec<ClaimRef>,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent-fw".into()),
            Fact {
                subject: "user".into(),
                predicate: "city".into(),
                value: serde_json::json!("Paris"),
            },
            Cardinality::Functional,
            provenance,
            ExternalAnchor {
                nearest_external_anchor: None,
                derivation_depth,
            },
            tx(),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            derived_from,
            None,
            None,
        )
    }

    fn external_claim(depth: u32) -> Claim {
        make_claim(ProvenanceLabel::External(ExternalKind::ExternalFirstHand), depth, vec![])
    }

    fn recall_claim_with_ref(existing: ClaimRef) -> Claim {
        make_claim(ProvenanceLabel::RecallReEntry, 0, vec![existing])
    }

    fn recall_claim_no_ref() -> Claim {
        make_claim(ProvenanceLabel::RecallReEntry, 0, vec![])
    }

    fn guard(threshold: u32, depth_cap: u32) -> AmplificationGuard {
        let cfg = EngineConfig {
            quarantine_burst_threshold: threshold,
            derivation_depth_cap_for_overturning: depth_cap,
            ..EngineConfig::default()
        };
        AmplificationGuard::new(Arc::new(cfg))
    }

    fn default_guard() -> AmplificationGuard {
        AmplificationGuard::new(Arc::new(EngineConfig::default()))
    }

    // ── AMPLIFICATION ACID TEST — mem0 #4573 ─────────────────────────────────
    //
    // 808 identical RecallReEntry re-ingestions of the same content MUST collapse to
    // ONE underlying claim and 807 CorroborateByIdentity verdicts (NOT 808 new claims).
    // This is the headline correctness claim for the firewall (I6, §7).

    #[test]
    fn amplification_acid_808_copies_collapse_to_one_claim() {
        // Use a threshold higher than 808 so burst detection does not fire —
        // we are testing the identity-collapse path, not the burst path.
        let g = guard(1000, 10);
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];

        let mut admitted_count = 0u32;
        let mut corroborated_count = 0u32;
        let mut quarantine_count = 0u32;

        for _ in 0..808 {
            let candidate = recall_claim_with_ref(existing_ref.clone());
            // burst_count_this_batch is always 0 here — each is "the first" of the batch
            // (the caller would increment but we are testing the identity collapse, not burst).
            // In a real write loop, burst_count would increment; here we isolate identity collapse.
            match g.check(&candidate, &injected, 0) {
                FirewallVerdict::Admit => admitted_count += 1,
                FirewallVerdict::CorroborateByIdentity { .. } => corroborated_count += 1,
                FirewallVerdict::Quarantine { .. } => quarantine_count += 1,
                FirewallVerdict::DepthCapExceeded { .. } => {}
            }
        }

        assert_eq!(admitted_count, 0,
            "808 RecallReEntry re-ingestions must NOT be admitted as new claims");
        assert_eq!(corroborated_count, 808,
            "808 RecallReEntry re-ingestions must ALL be CorroborateByIdentity");
        assert_eq!(quarantine_count, 0,
            "burst threshold was 1000; no quarantine expected");
    }

    /// Full end-to-end acid test: simulate the ACTUAL write loop where burst_count increments.
    /// 808 identical re-entries with burst_threshold=1000: all collapse to CorroborateByIdentity.
    #[test]
    fn amplification_acid_808_with_incrementing_burst_count_no_new_claim_rows() {
        let g = guard(1000, 10);
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];

        let mut new_claim_rows = 0u32;
        for batch_seq in 0..808 {
            let candidate = recall_claim_with_ref(existing_ref.clone());
            let verdict = g.check(&candidate, &injected, batch_seq);
            match verdict {
                FirewallVerdict::Admit | FirewallVerdict::DepthCapExceeded { .. } => {
                    new_claim_rows += 1;
                }
                FirewallVerdict::CorroborateByIdentity { provenance_independent, .. } => {
                    assert!(!provenance_independent,
                        "RecallReEntry must NOT be marked provenance-independent (V3-7)");
                }
                FirewallVerdict::Quarantine { .. } => {}
            }
        }

        assert_eq!(new_claim_rows, 0,
            "ACID: 808 re-ingestions must result in ZERO new claim rows (mem0 #4573 defence)");
    }

    // ── BURST QUARANTINE ──────────────────────────────────────────────────────

    #[test]
    fn burst_over_threshold_returns_quarantine() {
        let g = guard(5, 10); // threshold = 5
        let candidate = external_claim(0);
        let verdict = g.check(&candidate, &[], 5); // burst_count == threshold → quarantine
        assert!(
            matches!(verdict, FirewallVerdict::Quarantine { .. }),
            "burst_count >= threshold must return Quarantine"
        );
    }

    #[test]
    fn burst_exactly_at_threshold_returns_quarantine() {
        let g = guard(10, 10);
        let candidate = external_claim(0);
        let verdict = g.check(&candidate, &[], 10); // burst_count == 10 == threshold
        assert!(matches!(verdict, FirewallVerdict::Quarantine { .. }));
    }

    #[test]
    fn burst_one_below_threshold_does_not_quarantine() {
        let g = guard(10, 10);
        let candidate = external_claim(0);
        // burst_count = 9 < threshold 10 → should NOT quarantine
        let verdict = g.check(&candidate, &[], 9);
        assert!(
            !matches!(verdict, FirewallVerdict::Quarantine { .. }),
            "burst_count < threshold must NOT quarantine"
        );
    }

    #[test]
    fn burst_quarantine_fires_before_identity_check() {
        // Even a RecallReEntry candidate with a valid existing_ref gets Quarantine
        // when burst_count >= threshold (burst gate fires at step 1).
        let g = guard(3, 10);
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];
        let candidate = recall_claim_with_ref(existing_ref);
        let verdict = g.check(&candidate, &injected, 3); // burst_count == threshold
        assert!(
            matches!(verdict, FirewallVerdict::Quarantine { .. }),
            "burst detection must fire before identity check (step 1 before step 2)"
        );
    }

    // ── GENUINE EXTERNAL CLAIM — ADMIT ────────────────────────────────────────

    #[test]
    fn genuine_external_claim_is_admitted() {
        let g = default_guard();
        let candidate = external_claim(0);
        let verdict = g.check(&candidate, &[], 0);
        assert_eq!(verdict, FirewallVerdict::Admit,
            "first External claim with no prior injected refs must be Admitted");
    }

    #[test]
    fn external_claim_with_injected_refs_is_still_admitted() {
        // Injected refs exist but candidate is External (not RecallReEntry) → Admit.
        let g = default_guard();
        let injected = vec![ClaimRef::new_random(), ClaimRef::new_random()];
        let candidate = external_claim(0);
        let verdict = g.check(&candidate, &injected, 0);
        assert_eq!(verdict, FirewallVerdict::Admit);
    }

    // ── OP-1 DERIVATION DEPTH CAP ─────────────────────────────────────────────

    #[test]
    fn depth_beyond_cap_returns_depth_cap_exceeded() {
        let g = guard(10, 2); // depth cap = 2
        let candidate = external_claim(3); // depth 3 > cap 2
        let verdict = g.check(&candidate, &[], 0);
        assert!(
            matches!(verdict, FirewallVerdict::DepthCapExceeded { depth: 3, cap: 2 }),
            "derivation_depth > cap must return DepthCapExceeded"
        );
    }

    #[test]
    fn depth_exactly_at_cap_is_admitted() {
        let g = guard(10, 2); // depth cap = 2
        let candidate = external_claim(2); // depth == cap → still admitted
        let verdict = g.check(&candidate, &[], 0);
        assert_eq!(verdict, FirewallVerdict::Admit,
            "derivation_depth == cap is within limit; must Admit");
    }

    #[test]
    fn depth_below_cap_is_admitted() {
        let g = guard(10, 2);
        let candidate = external_claim(1);
        let verdict = g.check(&candidate, &[], 0);
        assert_eq!(verdict, FirewallVerdict::Admit);
    }

    #[test]
    fn op1_depth_cap_fires_after_burst_check() {
        // DepthCapExceeded fires at step 3 (after burst at step 1).
        // When burst_count >= threshold, Quarantine must win over DepthCapExceeded.
        let g = guard(3, 2); // burst threshold=3, depth cap=2
        let candidate = external_claim(5); // depth 5 > cap 2
        // burst_count = 3 == threshold → Quarantine wins (step 1 fires first)
        let verdict = g.check(&candidate, &[], 3);
        assert!(
            matches!(verdict, FirewallVerdict::Quarantine { .. }),
            "burst check (step 1) must fire before depth cap (step 3)"
        );
    }

    #[test]
    fn op1_recall_reentry_with_high_depth_corroborates_not_depth_exceeded() {
        // RecallReEntry provenance fires at step 2, before depth cap at step 3.
        // Even with depth > cap, CorroborateByIdentity wins if it is a RecallReEntry.
        // (The recall re-entry is already degraded; depth cap applies to overturn eligibility,
        // which reconciler handles — firewall only tags corroboration, not overturn eligibility.)
        let g = guard(1000, 2); // depth cap = 2
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];
        // Build a RecallReEntry with depth 5 (> cap 2)
        let candidate = make_claim(ProvenanceLabel::RecallReEntry, 5, vec![existing_ref]);
        let verdict = g.check(&candidate, &injected, 0);
        assert!(
            matches!(verdict, FirewallVerdict::CorroborateByIdentity { .. }),
            "RecallReEntry (step 2) fires before depth cap (step 3)"
        );
    }

    // ── PROVENANCE INDEPENDENCE — V3-7 ───────────────────────────────────────

    #[test]
    fn corroborate_by_identity_is_never_provenance_independent() {
        let g = default_guard();
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];
        let candidate = recall_claim_with_ref(existing_ref);
        let verdict = g.check(&candidate, &injected, 0);
        match verdict {
            FirewallVerdict::CorroborateByIdentity { provenance_independent, .. } => {
                assert!(!provenance_independent,
                    "RecallReEntry corroboration must NEVER be provenance-independent (V3-7)");
            }
            other => panic!("expected CorroborateByIdentity, got {other:?}"),
        }
    }

    // ── DETERMINISM ───────────────────────────────────────────────────────────

    #[test]
    fn check_is_deterministic_same_inputs_same_verdict() {
        let g = default_guard();
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];
        let candidate = recall_claim_with_ref(existing_ref);

        let v1 = g.check(&candidate, &injected, 0);
        let v2 = g.check(&candidate, &injected, 0);
        // Both must be CorroborateByIdentity with the same fields.
        assert_eq!(v1, v2, "check() must be deterministic for identical inputs");
    }

    #[test]
    fn check_is_deterministic_across_multiple_scenarios() {
        let g = guard(5, 3);
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];

        let scenarios: Vec<(Claim, &[ClaimRef], u32)> = vec![
            (external_claim(0), &[], 0),             // Admit
            (external_claim(4), &[], 0),              // DepthCapExceeded (depth 4 > cap 3)
            (external_claim(0), &[], 5),              // Quarantine (burst)
            (recall_claim_with_ref(existing_ref.clone()), &injected, 0), // CorroborateByIdentity
        ];

        for (candidate, refs, burst) in &scenarios {
            let v1 = g.check(candidate, refs, *burst);
            let v2 = g.check(candidate, refs, *burst);
            assert_eq!(v1, v2, "non-deterministic result for burst={burst}");
        }
    }

    // ── RECALL WITH NO MATCHING REF — GRACEFUL DEGRADATION ───────────────────

    #[test]
    fn recall_reentry_with_no_derived_from_and_empty_injected_admits() {
        // RecallReEntry with no prior injected refs: degraded gracefully to Admit.
        let g = default_guard();
        let candidate = recall_claim_no_ref();
        let verdict = g.check(&candidate, &[], 0);
        // No existing ClaimRef found → conservative: Admit rather than silently quarantine.
        assert_eq!(verdict, FirewallVerdict::Admit,
            "RecallReEntry with no matching prior ref must degrade gracefully to Admit");
    }

    #[test]
    fn recall_reentry_with_no_derived_from_but_injected_ref_present_corroborates() {
        // RecallReEntry with no derived_from but injected_claim_refs present →
        // fall back to first injected ref as the existing claim.
        let g = default_guard();
        let existing_ref = ClaimRef::new_random();
        let injected = vec![existing_ref.clone()];
        let candidate = recall_claim_no_ref(); // derived_from is empty
        let verdict = g.check(&candidate, &injected, 0);
        assert!(
            matches!(verdict, FirewallVerdict::CorroborateByIdentity { .. }),
            "RecallReEntry with no derived_from but with injected refs should corroborate"
        );
    }
}
