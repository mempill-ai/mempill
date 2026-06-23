//! Provenance types: structural invariant on every write (SDK_CONTRACT §2, I4, DC-1).

/// The structural invariant of the write surface (SDK_CONTRACT §2, I4, DC-1).
/// Required, typed field on every write. Assigned at injection time. Immutable.
/// Model-emitted content defaults to `ModelDerived` — callers cannot override this default
/// by supplying a more prestigious label; the gateway enforces the default (see C1 gateway.rs).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "kind")]
#[non_exhaustive]
pub enum ProvenanceLabel {
    /// First-hand external evidence — the ONLY cheap-path-eligible channel.
    External(ExternalKind),
    /// Content the engine itself previously served, re-entering the write path (X2 loop).
    /// Caught by C6 (firewall.rs). Corroborates by identity; never becomes ground truth.
    RecallReEntry,
    /// Model-emitted / inferred content. The mandatory default for model output.
    /// Committed down-weighted (Inferred disposition); ineligible to overturn until anchored.
    ModelDerived,
}

/// Sub-channel for External provenance (SDK_CONTRACT §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ExternalKind {
    /// A first-hand human assertion (user as oracle).
    UserAsserted,
    /// First-hand external evidence (tool result, system-of-record, sensor).
    ExternalFirstHand,
}

impl ProvenanceLabel {
    /// Returns true iff this label is eligible for the cheap (non-conflicting commit) path.
    /// Only External(*) qualifies. RecallReEntry and ModelDerived never qualify.
    pub fn is_cheap_path_eligible(&self) -> bool {
        matches!(self, Self::External(_))
    }

    /// Returns true iff this is a RecallReEntry — must be caught by the Amplification Guard.
    pub fn is_recall_reentry(&self) -> bool {
        matches!(self, Self::RecallReEntry)
    }
}

/// Distance from the nearest first-hand external anchor (SDK_CONTRACT §2.2, OP-1).
/// Derivation depth 0 = the claim IS a first-hand external claim.
/// Chains with depth > OP-3.derivation_depth_cap are ineligible for currency boosts/overturning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAnchor {
    /// ClaimRef of the nearest first-hand external claim in the lineage, if known.
    pub nearest_external_anchor: Option<crate::identity::ClaimRef>,
    /// Number of inference hops from that anchor. 0 = this claim is first-hand external.
    pub derivation_depth: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_user_asserted_is_cheap_path_eligible() {
        let p = ProvenanceLabel::External(ExternalKind::UserAsserted);
        assert!(p.is_cheap_path_eligible());
        assert!(!p.is_recall_reentry());
    }

    #[test]
    fn external_first_hand_is_cheap_path_eligible() {
        let p = ProvenanceLabel::External(ExternalKind::ExternalFirstHand);
        assert!(p.is_cheap_path_eligible());
        assert!(!p.is_recall_reentry());
    }

    #[test]
    fn recall_reentry_is_not_cheap_path_eligible() {
        let p = ProvenanceLabel::RecallReEntry;
        assert!(!p.is_cheap_path_eligible());
        assert!(p.is_recall_reentry());
    }

    #[test]
    fn model_derived_is_neither() {
        let p = ProvenanceLabel::ModelDerived;
        assert!(!p.is_cheap_path_eligible());
        assert!(!p.is_recall_reentry());
    }

    #[test]
    fn provenance_label_round_trip_serde() {
        let labels = [
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
            ProvenanceLabel::RecallReEntry,
            ProvenanceLabel::ModelDerived,
        ];
        for label in &labels {
            let json = serde_json::to_string(label).unwrap();
            let back: ProvenanceLabel = serde_json::from_str(&json).unwrap();
            assert_eq!(label, &back);
        }
    }

    #[test]
    fn provenance_label_python_friendly_json_shapes() {
        // External(UserAsserted) → adjacently-tagged: {"type":"External","kind":"UserAsserted"}
        let ext_ua = ProvenanceLabel::External(ExternalKind::UserAsserted);
        let json = serde_json::to_string(&ext_ua).unwrap();
        assert_eq!(json, r#"{"type":"External","kind":"UserAsserted"}"#);
        let back: ProvenanceLabel = serde_json::from_str(&json).unwrap();
        assert_eq!(ext_ua, back);

        // External(ExternalFirstHand) → {"type":"External","kind":"ExternalFirstHand"}
        let ext_fh = ProvenanceLabel::External(ExternalKind::ExternalFirstHand);
        let json = serde_json::to_string(&ext_fh).unwrap();
        assert_eq!(json, r#"{"type":"External","kind":"ExternalFirstHand"}"#);
        let back: ProvenanceLabel = serde_json::from_str(&json).unwrap();
        assert_eq!(ext_fh, back);

        // RecallReEntry → {"type":"RecallReEntry"} (unit variant — no "kind" key)
        let rre = ProvenanceLabel::RecallReEntry;
        let json = serde_json::to_string(&rre).unwrap();
        assert_eq!(json, r#"{"type":"RecallReEntry"}"#);
        let back: ProvenanceLabel = serde_json::from_str(&json).unwrap();
        assert_eq!(rre, back);

        // ModelDerived → {"type":"ModelDerived"}
        let md = ProvenanceLabel::ModelDerived;
        let json = serde_json::to_string(&md).unwrap();
        assert_eq!(json, r#"{"type":"ModelDerived"}"#);
        let back: ProvenanceLabel = serde_json::from_str(&json).unwrap();
        assert_eq!(md, back);
    }

    #[test]
    fn external_anchor_round_trip_serde() {
        let anchor = ExternalAnchor {
            nearest_external_anchor: Some(crate::identity::ClaimRef::new_random()),
            derivation_depth: 2,
        };
        let json = serde_json::to_string(&anchor).unwrap();
        let back: ExternalAnchor = serde_json::from_str(&json).unwrap();
        assert_eq!(anchor, back);
    }
}
