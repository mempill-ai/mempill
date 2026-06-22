//! EmbeddingPort and VectorPort — fuzzy candidate coverage and vector persistence seam.
//!
//! EmbeddingPort (F8, A10): BYO-embedding port for fuzzy candidate lookup.
//! Engine functional in structural-only mode when this port is absent.
//!
//! VectorPort: UNIMPLEMENTED IN v0.1 — this is a compile-time seam only.
//! The sqlite-vec integration will implement this in v0.2.

use mempill_types::{AgentId, ClaimRef, SubjectLineRef};

/// BYO-embedding port for fuzzy candidate coverage (secondary tier, F8, A10).
/// Engine functional in structural-only mode when this port is absent.
pub trait EmbeddingPort: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn embed(&self, text: &str) -> Result<Vec<f32>, Self::Error>;

    fn select_candidates(
        &self,
        agent_id: &AgentId,
        query_vector: &[f32],
        k: usize,
    ) -> Result<Vec<SubjectLineRef>, Self::Error>;
}

/// Vector persistence seam — SEPARATE from PersistencePort (DB_REQUIREMENTS.md §3, A10).
///
/// # v0.1 Status — UNIMPLEMENTED SEAM
///
/// This trait is defined as a compile-time seam only. No implementation exists in v0.1.
/// The sqlite-vec integration will implement this in v0.2. Callers that need structural-only
/// mode pass `None::<Arc<NoOpVector>>` (or equivalent) at construction.
pub trait VectorPort: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Store embedding for a claim.
    /// `embedding_model_id` is required for model-swap safety (CONSTRAINTS.md §D, A10).
    fn store_embedding(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
        vector: &[f32],
        embedding_model_id: &str,
    ) -> Result<(), Self::Error>;

    fn search(
        &self,
        agent_id: &AgentId,
        query_vector: &[f32],
        k: usize,
        embedding_model_id: &str,
    ) -> Result<Vec<ClaimRef>, Self::Error>;
}
