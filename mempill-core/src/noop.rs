//! NoOp stub implementations for OraclePort and VectorPort.
//!
//! These stubs satisfy their respective trait bounds with do-nothing / empty behavior.
//! They are provided for:
//!   - v0.1 testing without a real oracle or vector store.
//!   - The `DefaultEngine` type alias which wires NoOpOracle + NoOpVector
//!     as the defaults for host-optional ports.
//!
//! DO NOT use these stubs in production. They are clearly doc-commented as test/default stubs.

use mempill_types::{AgentId, AdjudicationRequest, ClaimRef};
use crate::ports::{OraclePort, VectorPort};

// ── ERROR TYPES ───────────────────────────────────────────────────────────────

/// Infallible error type for NoOp stubs — the stubs never fail.
#[derive(Debug)]
pub enum NoOpError {}

impl std::fmt::Display for NoOpError {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Infallible — this variant is unreachable.
        unreachable!()
    }
}

impl std::error::Error for NoOpError {}

// ── NOOP ORACLE ───────────────────────────────────────────────────────────────

/// A no-op oracle that accepts adjudication requests and immediately returns a unit handle.
///
/// # Test / Default Stub
///
/// - Does NOT surface requests to any external system.
/// - The returned `()` handle cannot be used to correlate an `AdjudicationResponse`.
/// - With this oracle present, `oracle_present = true` in the gate Proposal; therefore
///   conflicting claims route to `QueuedForAdjudication` rather than `Contested`.
///   No verdict ever arrives, leaving them in `QueuedForAdjudication` indefinitely.
/// - Suitable for unit tests that don't care about oracle resolution and for the
///   `DefaultEngine` alias.
#[derive(Debug, Clone)]
pub struct NoOpOracle;

impl OraclePort for NoOpOracle {
    type Error = NoOpError;
    type Handle = ();

    fn request_adjudication(
        &self,
        _agent_id: &AgentId,
        _request: AdjudicationRequest,
    ) -> Result<Self::Handle, Self::Error> {
        // No-op: accept and discard the request.
        Ok(())
    }

    /// NoOpOracle never correlates responses, so return a fresh UUID.
    /// The pending row will be persisted but never resolved (no verdict arrives).
    fn handle_to_uuid(_handle: &Self::Handle) -> uuid::Uuid {
        uuid::Uuid::new_v4()
    }
}

// ── NOOP VECTOR ───────────────────────────────────────────────────────────────

/// A no-op vector store that discards all embeddings and returns empty search results.
///
/// # Test / Default Stub
///
/// - `store_embedding` silently discards the vector (no-op).
/// - `search` always returns an empty `Vec<ClaimRef>`.
/// - Engine operates in structural-only mode when this stub is used (no fuzzy candidate coverage).
/// - Matches v0.1 intent: VectorPort is a compile-time seam only; sqlite-vec integration is v0.2.
#[derive(Debug, Clone)]
pub struct NoOpVector;

impl VectorPort for NoOpVector {
    type Error = NoOpError;

    fn store_embedding(
        &self,
        _agent_id: &AgentId,
        _claim_ref: &ClaimRef,
        _vector: &[f32],
        _embedding_model_id: &str,
    ) -> Result<(), Self::Error> {
        // No-op: discard the embedding.
        Ok(())
    }

    fn search(
        &self,
        _agent_id: &AgentId,
        _query_vector: &[f32],
        _k: usize,
        _embedding_model_id: &str,
    ) -> Result<Vec<ClaimRef>, Self::Error> {
        // No-op: always empty result set.
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_types::{
        AgentId, AdjudicationRequest, Belief, Claim, ClaimRef, Cardinality, Criticality,
        Confidence, CurrencySignal, CurrencyState, ExternalAnchor, ExternalKind,
        Fact, OverturnReason, ProvenanceLabel, SubjectLineRef, TransactionTime, ValidTime,
    };

    fn make_agent() -> AgentId {
        AgentId("test-agent".into())
    }

    fn make_belief() -> Belief {
        Belief {
            claim_ref: ClaimRef::new_random(),
            fact: Fact {
                subject: "alice".into(),
                predicate: "age".into(),
                value: serde_json::json!(30),
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            transaction_time: TransactionTime(chrono::Utc::now()),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            currency_signal: CurrencySignal {
                last_refreshed_at: TransactionTime(chrono::Utc::now()),
                state: CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: Criticality::Low,
        }
    }

    fn make_challenger(agent: &AgentId) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact {
                subject: "alice".into(),
                predicate: "age".into(),
                value: serde_json::json!(31),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(chrono::Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        )
    }

    #[test]
    fn noop_oracle_implements_oracle_port_and_returns_ok() {
        let oracle = NoOpOracle;
        let agent = make_agent();
        let request = AdjudicationRequest {
            subject_line: SubjectLineRef {
                agent_id: agent.clone(),
                subject: "alice".into(),
                predicate: "age".into(),
            },
            incumbent: make_belief(),
            challenger: make_challenger(&agent),
            criticality: Criticality::Low,
            reason: OverturnReason::ExternalContradiction,
        };

        let result = oracle.request_adjudication(&agent, request);
        assert!(result.is_ok());
        let _handle: () = result.unwrap();
    }

    #[test]
    fn noop_vector_store_embedding_is_noop() {
        let vector = NoOpVector;
        let agent = make_agent();
        let claim_ref = ClaimRef::new_random();
        let embedding = vec![0.1f32, 0.2, 0.3];
        let result = vector.store_embedding(&agent, &claim_ref, &embedding, "text-embedding-3-small");
        assert!(result.is_ok());
    }

    #[test]
    fn noop_vector_search_returns_empty() {
        let vector = NoOpVector;
        let agent = make_agent();
        let query = vec![0.1f32, 0.2, 0.3];
        let result = vector.search(&agent, &query, 10, "text-embedding-3-small");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn noop_oracle_is_clone_and_debug() {
        let oracle = NoOpOracle;
        let _cloned = oracle.clone();
        let s = format!("{oracle:?}");
        assert!(s.contains("NoOpOracle"));
    }

    #[test]
    fn noop_vector_is_clone_and_debug() {
        let vector = NoOpVector;
        let _cloned = vector.clone();
        let s = format!("{vector:?}");
        assert!(s.contains("NoOpVector"));
    }

    #[test]
    fn noop_oracle_satisfies_trait_bounds() {
        fn assert_oracle_bounds<T: OraclePort + Send + Sync + 'static>() {}
        assert_oracle_bounds::<NoOpOracle>();
    }

    #[test]
    fn noop_vector_satisfies_trait_bounds() {
        fn assert_vector_bounds<T: VectorPort + Send + Sync + 'static>() {}
        assert_vector_bounds::<NoOpVector>();
    }

    /// NoOpOracle::handle_to_uuid returns a non-nil UUID for the unit handle.
    /// Two calls must return distinct UUIDs (because each call generates a fresh UUID v4).
    #[test]
    fn noop_oracle_handle_to_uuid_returns_non_nil_uuid() {
        let uuid1 = NoOpOracle::handle_to_uuid(&());
        let uuid2 = NoOpOracle::handle_to_uuid(&());
        assert!(!uuid1.is_nil(), "handle_to_uuid must return a non-nil UUID");
        assert_ne!(uuid1, uuid2, "successive handle_to_uuid calls must return distinct UUIDs");
    }
}
