//! C1 — Ingestion / Write Gateway (TECHNICAL_DESIGN.md §9, I2, I4, DC-1).
//!
//! Responsibilities:
//! - Enforce provenance present (MissingProvenance error if absent, I4, DC-1).
//! - Stamp TransactionTime (engine-assigned; host cannot supply this as truth, I2).
//! - Assign ClaimRef (UUID, immutable once minted, I4).
//! - Stamp ProvenanceLabel immutably (I4): preserve caller-supplied label; RecallReEntry
//!   is set externally by C6 (firewall.rs) BEFORE calling gateway in the full write path —
//!   gateway preserves it without re-deriving.
//! - Enforce ModelDerived default: if `provenance` is `None`, return MissingProvenance;
//!   callers must supply a label explicitly (DTO validation layer handles the default assignment).
//!
//! Gateway does NOT make adjudication decisions — that is C7 (gate.rs).
//! Gateway does NOT persist — that is the application layer + PersistencePort.

use mempill_types::{
    AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, ExternalAnchor,
    Fact, ProvenanceLabel, TransactionTime, ValidTime,
};
use crate::error::MemError;

/// Input to the ingestion gateway — raw caller-supplied data before stamping.
///
/// `provenance` is `Option` to force explicit validation at the engine boundary (DC-1, I4).
/// The DTO layer supplies the caller's intended label; `None` means the caller failed to
/// provide one, which is a hard error.
#[derive(Debug, Clone)]
pub(crate) struct IngestInput {
    pub agent_id: AgentId,
    pub fact: Fact,
    pub cardinality: Cardinality,
    /// Required. Must be `Some`; gateway returns `MemError::MissingProvenance` if `None`.
    pub provenance: Option<ProvenanceLabel>,
    pub external_anchor: ExternalAnchor,
    /// Host-supplied valid-time (fallible, I2). `None` = unknown valid-time.
    pub valid_time: Option<ValidTime>,
    pub confidence: Confidence,
    pub criticality: Criticality,
    pub derived_from: Vec<ClaimRef>,
    pub metadata: Option<serde_json::Value>,
}

/// Output of the ingestion gateway — a fully-stamped, immutable Claim ready for C6/C7.
#[derive(Debug, Clone)]
pub(crate) struct StampedClaim {
    pub claim: Claim,
}

/// Ingest a caller-supplied input, enforcing all C1 gateway invariants.
///
/// # Parameters
/// - `input`   — caller-supplied claim data (provenance required).
/// - `tx_time` — engine-assigned transaction time (I2). Caller cannot override.
///
/// # Errors
/// - `MemError::MissingProvenance` — `input.provenance` was `None`.
///
/// # Invariants enforced
/// - Provenance is required (I4, DC-1): `None` → `MissingProvenance`.
/// - `TransactionTime` is engine-stamped via `tx_time` parameter (I2).
/// - `ClaimRef` is newly minted (immutable identity, I4).
/// - Provenance is preserved immutably after construction via `Claim::new()`.
pub(crate) fn stamp(input: IngestInput, tx_time: TransactionTime) -> Result<StampedClaim, MemError> {
    // I4 / DC-1: provenance is required.
    let provenance = input.provenance.ok_or(MemError::MissingProvenance)?;

    // Assign immutable ClaimRef (minted here, never reassigned).
    let claim_ref = ClaimRef::new_random();

    // Resolve ValidTime: use supplied or fallback to unknown.
    let valid_time = input.valid_time.unwrap_or(ValidTime {
        start: None,
        end: None,
        valid_time_confidence: 0.0,
    });

    // Construct the immutable Claim via the only constructor (I4 — no setters).
    let claim = Claim::new(
        claim_ref,
        input.agent_id,
        input.fact,
        input.cardinality,
        provenance, // immutably stamped; Claim::new takes ownership (I4)
        input.external_anchor,
        tx_time,    // engine-assigned (I2)
        valid_time,
        input.confidence,
        input.criticality,
        input.derived_from,
        input.metadata,
        None, // snapshot_schema_version: None in v0.1
    );

    Ok(StampedClaim { claim })
}

/// Classify the provenance type for routing decisions downstream.
///
/// Used by the write path to determine whether to route to C6 firewall (RecallReEntry),
/// or directly to C7 gate (External, ModelDerived).
///
/// Note: `ProvenanceLabel` is `#[non_exhaustive]`; the wildcard arm handles any future variants.
pub(crate) fn classify_provenance(provenance: &ProvenanceLabel) -> ProvenanceClass {
    match provenance {
        ProvenanceLabel::RecallReEntry => ProvenanceClass::RecallReEntry,
        ProvenanceLabel::External(_) => ProvenanceClass::External,
        ProvenanceLabel::ModelDerived => ProvenanceClass::ModelDerived,
        _ => ProvenanceClass::ModelDerived, // future non-exhaustive variants default to ModelDerived
    }
}

/// Coarse provenance classification for write-path routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProvenanceClass {
    External,
    RecallReEntry,
    ModelDerived,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_types::{ExternalAnchor, ExternalKind};
    use chrono::{TimeZone, Utc};

    fn tx_now() -> TransactionTime {
        TransactionTime(Utc.with_ymd_and_hms(2026, 6, 22, 12, 0, 0).unwrap())
    }

    fn base_input(provenance: Option<ProvenanceLabel>) -> IngestInput {
        IngestInput {
            agent_id: AgentId("agent-test".into()),
            fact: Fact {
                subject: "user".into(),
                predicate: "city".into(),
                value: serde_json::json!("Paris"),
            },
            cardinality: Cardinality::Functional,
            provenance,
            external_anchor: ExternalAnchor {
                nearest_external_anchor: None,
                derivation_depth: 0,
            },
            valid_time: None,
            confidence: Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.0,
            },
            criticality: Criticality::Medium,
            derived_from: vec![],
            metadata: None,
        }
    }

    // ── MISSING PROVENANCE ───────────────────────────────────────────────────────

    #[test]
    fn missing_provenance_returns_error() {
        let result = stamp(base_input(None), tx_now());
        assert!(
            matches!(result, Err(MemError::MissingProvenance)),
            "expected MissingProvenance, got {:?}",
            result
        );
    }

    // ── PROVENANCE PRESERVED IMMUTABLY ───────────────────────────────────────────

    #[test]
    fn external_provenance_is_preserved() {
        let prov = ProvenanceLabel::External(ExternalKind::UserAsserted);
        let result = stamp(base_input(Some(prov.clone())), tx_now()).unwrap();
        assert_eq!(*result.claim.provenance(), prov,
            "provenance must be preserved immutably after stamping");
    }

    #[test]
    fn model_derived_provenance_is_preserved() {
        let prov = ProvenanceLabel::ModelDerived;
        let result = stamp(base_input(Some(prov.clone())), tx_now()).unwrap();
        assert_eq!(*result.claim.provenance(), prov);
    }

    #[test]
    fn recall_reentry_provenance_is_preserved() {
        let prov = ProvenanceLabel::RecallReEntry;
        let result = stamp(base_input(Some(prov.clone())), tx_now()).unwrap();
        assert_eq!(*result.claim.provenance(), prov,
            "RecallReEntry must be preserved — gateway does not re-derive it");
    }

    // ── TX_TIME IS ENGINE-ASSIGNED ───────────────────────────────────────────────

    #[test]
    fn tx_time_is_engine_assigned() {
        let tx = tx_now();
        let result = stamp(
            base_input(Some(ProvenanceLabel::External(ExternalKind::ExternalFirstHand))),
            tx.clone(),
        ).unwrap();
        assert_eq!(*result.claim.transaction_time(), tx,
            "gateway must stamp the engine-supplied tx_time (I2)");
    }

    // ── CLAIM_REF ASSIGNED ───────────────────────────────────────────────────────

    #[test]
    fn claim_ref_is_assigned_as_uuid() {
        let result = stamp(
            base_input(Some(ProvenanceLabel::External(ExternalKind::UserAsserted))),
            tx_now(),
        ).unwrap();
        assert_ne!(
            result.claim.claim_ref().0,
            uuid::Uuid::nil(),
            "ClaimRef must be a freshly minted non-nil UUID"
        );
    }

    #[test]
    fn two_stamps_produce_different_claim_refs() {
        let input_a = base_input(Some(ProvenanceLabel::External(ExternalKind::UserAsserted)));
        let input_b = base_input(Some(ProvenanceLabel::External(ExternalKind::UserAsserted)));
        let a = stamp(input_a, tx_now()).unwrap();
        let b = stamp(input_b, tx_now()).unwrap();
        assert_ne!(a.claim.claim_ref(), b.claim.claim_ref(),
            "each ingestion must mint a distinct ClaimRef");
    }

    // ── VALID_TIME DEFAULTS ──────────────────────────────────────────────────────

    #[test]
    fn absent_valid_time_defaults_to_unknown() {
        let result = stamp(
            base_input(Some(ProvenanceLabel::ModelDerived)),
            tx_now(),
        ).unwrap();
        assert!(result.claim.valid_time().is_unknown(),
            "absent valid_time should default to unknown (None/None)");
    }

    #[test]
    fn supplied_valid_time_is_preserved() {
        let start = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let vt = ValidTime {
            start: Some(start),
            end: None,
            valid_time_confidence: 0.8,
        };
        let mut input = base_input(Some(ProvenanceLabel::External(ExternalKind::UserAsserted)));
        input.valid_time = Some(vt.clone());
        let result = stamp(input, tx_now()).unwrap();
        assert_eq!(result.claim.valid_time().start, vt.start);
        assert_eq!(
            result.claim.valid_time().valid_time_confidence,
            vt.valid_time_confidence
        );
    }

    // ── PROVENANCE CLASSIFICATION ────────────────────────────────────────────────

    #[test]
    fn classify_external_provenance() {
        assert_eq!(
            classify_provenance(&ProvenanceLabel::External(ExternalKind::UserAsserted)),
            ProvenanceClass::External
        );
        assert_eq!(
            classify_provenance(&ProvenanceLabel::External(ExternalKind::ExternalFirstHand)),
            ProvenanceClass::External
        );
    }

    #[test]
    fn classify_recall_reentry_provenance() {
        assert_eq!(
            classify_provenance(&ProvenanceLabel::RecallReEntry),
            ProvenanceClass::RecallReEntry
        );
    }

    #[test]
    fn classify_model_derived_provenance() {
        assert_eq!(
            classify_provenance(&ProvenanceLabel::ModelDerived),
            ProvenanceClass::ModelDerived
        );
    }

    // ── RECALL REENTRY LABEL DETECTION ──────────────────────────────────────────

    #[test]
    fn recall_reentry_correctly_labeled_as_reentry() {
        let prov = ProvenanceLabel::RecallReEntry;
        let result = stamp(base_input(Some(prov)), tx_now()).unwrap();
        assert!(result.claim.provenance().is_recall_reentry(),
            "RecallReEntry provenance must be detected by is_recall_reentry()");
    }

    #[test]
    fn external_not_labeled_as_recall_reentry() {
        let prov = ProvenanceLabel::External(ExternalKind::ExternalFirstHand);
        let result = stamp(base_input(Some(prov)), tx_now()).unwrap();
        assert!(!result.claim.provenance().is_recall_reentry());
    }
}
