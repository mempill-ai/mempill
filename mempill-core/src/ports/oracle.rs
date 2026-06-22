//! OraclePort — pull-based, non-blocking adjudication port (SDK_CONTRACT §5, DC-4, A15).
//!
//! Host-implemented. Engine NEVER blocks waiting for a verdict.
//! Absence of oracle → Contested, never silent incumbent-wins (V3-5).

use mempill_types::{AgentId, AdjudicationRequest};

/// The oracle port — pull-based, non-blocking (SDK_CONTRACT §5, DC-4, A15).
/// Host-implemented. Engine NEVER blocks waiting for a verdict.
/// Absence of oracle → Contested, never silent incumbent-wins (V3-5).
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
}
