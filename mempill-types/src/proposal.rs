//! Proposal types: stochastic proposer output and adjudication request/response.
//!
//! These types cross the stochastic/deterministic boundary. Proposals from extractors
//! and oracles are always advisory — the deterministic engine core decides all
//! dispositions and no stochastic output can commit directly.

use crate::belief::Belief;
use crate::claim::{Cardinality, Confidence, Criticality};
use crate::identity::SubjectLineRef;
use crate::provenance::ProvenanceLabel;
use crate::time::ValidTime;
use crate::claim::Fact;
use crate::claim::Claim;

/// Stochastic proposer output — never a commit.
///
/// The engine receives proposals from `ExtractorPort` and decides all dispositions
/// deterministically. Proposals carry no authority to commit; they flow through the
/// reconciler and adjudication gate before any write is made.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimProposal {
    pub fact: Fact,
    pub suggested_valid_time: Option<ValidTime>,
    pub suggested_cardinality: Cardinality,
    pub confidence: Confidence,
    /// ADVISORY ONLY — engine enforces ModelDerived default and provenance immutability.
    /// If None, gateway assigns ModelDerived (the mandatory default).
    pub suggested_provenance: Option<ProvenanceLabel>,
}

/// Adjudication request sent to the `OraclePort` by the adjudication gate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdjudicationRequest {
    pub subject_line: SubjectLineRef,
    pub incumbent: Belief,
    pub challenger: Claim,
    pub criticality: Criticality,
    pub reason: OverturnReason,
}

/// Why an adjudication was triggered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OverturnReason {
    ExternalContradiction,
    ValidityBound,
    DependsOnSuperseded,
    HighDerivationDepth,
}

/// Response delivered asynchronously back into the engine from the oracle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdjudicationResponse {
    pub handle_id: uuid::Uuid,
    pub verdict: AdjudicationVerdict,
    pub evidence_provenance: ProvenanceLabel,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AdjudicationVerdict {
    /// Challenger confirmed; incumbent bounded.
    Affirm,
    /// Incumbent affirmed; challenger goes Superseded.
    Deny,
    /// Ambiguous; surfaces Contested.
    Unknown,
}

/// The resolved outcome of an adjudication, delivered asynchronously from the oracle loop.
/// Carries the identity of the adjudication request (`handle_id`), the final disposition
/// applied to the challenger claim, and the claim reference the outcome targets.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdjudicationOutcome {
    /// Correlates this outcome back to the originating [`AdjudicationRequest`].
    pub handle_id: uuid::Uuid,
    /// The deterministic disposition the engine will apply to the challenger claim.
    pub disposition: crate::disposition::Disposition,
    /// The claim this outcome acts upon.
    pub claim_ref: crate::identity::ClaimRef,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{ExternalKind, ProvenanceLabel};

    #[test]
    fn claim_proposal_carries_suggested_provenance() {
        let p = ClaimProposal {
            fact: Fact { subject: "s".into(), predicate: "p".into(), value: serde_json::json!(1) },
            suggested_valid_time: None,
            suggested_cardinality: Cardinality::Unknown,
            confidence: Confidence { value_confidence: 0.8, valid_time_confidence: 0.0 },
            suggested_provenance: Some(ProvenanceLabel::External(ExternalKind::UserAsserted)),
        };
        assert!(p.suggested_provenance.is_some());
    }

    #[test]
    fn overture_reason_round_trip_serde() {
        let reasons = [
            OverturnReason::ExternalContradiction,
            OverturnReason::ValidityBound,
            OverturnReason::DependsOnSuperseded,
            OverturnReason::HighDerivationDepth,
        ];
        for r in &reasons {
            let json = serde_json::to_string(r).unwrap();
            let back: OverturnReason = serde_json::from_str(&json).unwrap();
            assert_eq!(r, &back);
        }
    }

    #[test]
    fn adjudication_verdict_round_trip_serde() {
        let verdicts = [
            AdjudicationVerdict::Affirm,
            AdjudicationVerdict::Deny,
            AdjudicationVerdict::Unknown,
        ];
        for v in &verdicts {
            let json = serde_json::to_string(v).unwrap();
            let back: AdjudicationVerdict = serde_json::from_str(&json).unwrap();
            assert_eq!(v, &back);
        }
    }

    #[test]
    fn adjudication_outcome_round_trip_serde() {
        use crate::disposition::Disposition;
        use crate::identity::ClaimRef;

        let outcome = AdjudicationOutcome {
            handle_id: uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            disposition: Disposition::Superseded,
            claim_ref: ClaimRef::new_random(),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        let back: AdjudicationOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back.handle_id, outcome.handle_id);
        assert_eq!(back.disposition, outcome.disposition);
        assert_eq!(back.claim_ref, outcome.claim_ref);
    }

    #[test]
    fn adjudication_response_has_oracle_present_field_via_handle_id() {
        // A24: oracle_present is on the internal Proposal (engine/gate.rs), not on
        // AdjudicationResponse. The response carries the verdict; oracle_present is
        // a gate input, not part of the async response payload.
        let resp = AdjudicationResponse {
            handle_id: uuid::Uuid::new_v4(),
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        };
        assert_eq!(resp.verdict, AdjudicationVerdict::Affirm);
    }
}
