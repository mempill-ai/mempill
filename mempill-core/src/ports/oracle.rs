//! OraclePort — pull-based, non-blocking adjudication port.
//!
//! Host-implemented. The engine NEVER blocks waiting for a verdict.
//! Absence of oracle → `Contested`, never silent incumbent-wins.
//! The oracle delivers responses asynchronously via `EngineHandle::submit_adjudication`.

use mempill_types::{AgentId, AdjudicationRequest};

/// The oracle port — pull-based, non-blocking.
///
/// Host-implemented. The engine NEVER blocks waiting for a verdict.
/// When no oracle is registered, conflicting claims surface as `Contested`
/// rather than silently picking the incumbent.
pub trait OraclePort: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;
    /// An opaque handle for correlating the async response back to the engine.
    type Handle: Send + 'static;

    /// Engine requests adjudication. Returns immediately. Handle used for correlation.
    /// Host delivers response asynchronously back into the engine
    /// via `EngineHandle::submit_adjudication`.
    fn request_adjudication(
        &self,
        agent_id: &AgentId,
        request: AdjudicationRequest,
    ) -> Result<Self::Handle, Self::Error>;

    /// Convert an opaque oracle handle to the durable `handle_id` UUID used as the PK
    /// in the `pending_adjudications` table as the durable correlation key.
    ///
    /// This bridges the oracle's opaque handle type to the engine's persistence layer.
    /// Called immediately after `request_adjudication` returns, before the pending row is inserted.
    fn handle_to_uuid(handle: &Self::Handle) -> uuid::Uuid;
}
