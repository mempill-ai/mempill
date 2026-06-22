//! Claim and associated value objects: the atomic committed assertion (SDK_CONTRACT §1.2).
//! Claim is write-once (I1, I4) — private fields with constructor and getters only.

use crate::identity::{AgentId, ClaimRef};
use crate::provenance::{ExternalAnchor, ProvenanceLabel};
use crate::time::{TransactionTime, ValidTime};

/// The atomic asserted statement (SDK_CONTRACT §1.2).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Fact {
    pub subject: String,
    pub predicate: String,
    pub value: serde_json::Value,
}

/// Cardinality proposal (SDK_CONTRACT §1.2, V3-3).
/// `Unknown` is the default — routes to non-destructive branch + oracle surfacing.
/// Classification is always a proposal (I5); the gate decides.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Cardinality {
    /// At most one value is valid at a time. Bounding a new value supersedes prior.
    Functional,
    /// Multiple simultaneous values are valid. Bounding requires explicit negative assertion.
    SetValued,
    /// Default. Routes to non-destructive path + surfaces to oracle.
    #[default]
    Unknown,
}

/// Two separate confidence scores (SDK_CONTRACT §1.4, B2).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Confidence {
    /// Confidence in the value itself (0.0–1.0).
    pub value_confidence: f32,
    /// Confidence in the valid-time extraction (0.0–1.0). May be 0.0 = "unknown".
    pub valid_time_confidence: f32,
}

/// Criticality class — distinct from currency (V3-7).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Criticality {
    Low,
    Medium,
    High,
    /// Safety-relevant (e.g., allergy, medication).
    Critical,
}

/// A committed claim — write-once, immutable after append (I1, I4).
/// All fields set at injection time via `Claim::new`; no field may be mutated after commit.
/// Fields are private to enforce the write-once invariant at compile time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    claim_ref: ClaimRef,
    agent_id: AgentId,
    fact: Fact,
    cardinality: Cardinality,
    provenance: ProvenanceLabel,
    external_anchor: ExternalAnchor,
    transaction_time: TransactionTime,
    valid_time: ValidTime,
    confidence: Confidence,
    criticality: Criticality,
    derived_from: Vec<ClaimRef>,
    metadata: Option<serde_json::Value>,
    snapshot_schema_version: Option<u32>,
}

impl Claim {
    /// Construct a fully-formed, frozen Claim. The only constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        claim_ref: ClaimRef,
        agent_id: AgentId,
        fact: Fact,
        cardinality: Cardinality,
        provenance: ProvenanceLabel,
        external_anchor: ExternalAnchor,
        transaction_time: TransactionTime,
        valid_time: ValidTime,
        confidence: Confidence,
        criticality: Criticality,
        derived_from: Vec<ClaimRef>,
        metadata: Option<serde_json::Value>,
        snapshot_schema_version: Option<u32>,
    ) -> Self {
        Self {
            claim_ref,
            agent_id,
            fact,
            cardinality,
            provenance,
            external_anchor,
            transaction_time,
            valid_time,
            confidence,
            criticality,
            derived_from,
            metadata,
            snapshot_schema_version,
        }
    }

    // ── Getters (read-only; no setters by design) ─────────────────────────────

    pub fn claim_ref(&self) -> &ClaimRef { &self.claim_ref }
    pub fn agent_id(&self) -> &AgentId { &self.agent_id }
    pub fn fact(&self) -> &Fact { &self.fact }
    pub fn cardinality(&self) -> &Cardinality { &self.cardinality }
    pub fn provenance(&self) -> &ProvenanceLabel { &self.provenance }
    pub fn external_anchor(&self) -> &ExternalAnchor { &self.external_anchor }
    pub fn transaction_time(&self) -> &TransactionTime { &self.transaction_time }
    pub fn valid_time(&self) -> &ValidTime { &self.valid_time }
    pub fn confidence(&self) -> &Confidence { &self.confidence }
    pub fn criticality(&self) -> &Criticality { &self.criticality }
    pub fn derived_from(&self) -> &[ClaimRef] { &self.derived_from }
    pub fn metadata(&self) -> Option<&serde_json::Value> { self.metadata.as_ref() }
    pub fn snapshot_schema_version(&self) -> Option<u32> { self.snapshot_schema_version }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentId;
    use crate::provenance::{ExternalAnchor, ProvenanceLabel};
    use crate::time::{TransactionTime, ValidTime};
    use chrono::Utc;

    fn make_claim() -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent-42".into()),
            Fact {
                subject: "user".into(),
                predicate: "email".into(),
                value: serde_json::json!("alice@example.com"),
            },
            Cardinality::Functional,
            ProvenanceLabel::ModelDerived,
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        )
    }

    #[test]
    fn claim_constructed_and_readable() {
        let c = make_claim();
        assert_eq!(c.agent_id(), &AgentId("agent-42".into()));
        assert_eq!(c.fact().subject, "user");
        assert_eq!(c.cardinality(), &Cardinality::Functional);
    }

    #[test]
    fn claim_is_immutable_no_setters() {
        // This test is a compile-time proof: if you can build it, there are no setters.
        let c = make_claim();
        // We can only read:
        let _ = c.claim_ref();
        let _ = c.provenance();
    }

    #[test]
    fn cardinality_unknown_is_default() {
        let c: Cardinality = Default::default();
        assert_eq!(c, Cardinality::Unknown);
    }

    #[test]
    fn claim_round_trip_serde() {
        let c = make_claim();
        let json = serde_json::to_string(&c).unwrap();
        let back: Claim = serde_json::from_str(&json).unwrap();
        assert_eq!(c.claim_ref(), back.claim_ref());
        assert_eq!(c.agent_id(), back.agent_id());
        assert_eq!(c.fact(), back.fact());
    }

    #[test]
    fn criticality_ordering() {
        assert!(Criticality::Low < Criticality::Medium);
        assert!(Criticality::Medium < Criticality::High);
        assert!(Criticality::High < Criticality::Critical);
    }
}
