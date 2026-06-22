//! MemError — top-level error type for the mempill engine (TECHNICAL_DESIGN.md §11).
//!
//! Every invariant violation surfaces as a typed variant — never silently swallowed.
//! Uses `thiserror` for ergonomic Display + Error implementations.

use thiserror::Error;
use mempill_types::{AgentId, ClaimRef};

/// Top-level error type for the mempill engine.
/// Every invariant violation surfaces as a typed variant here — never silently swallowed.
#[derive(Debug, Error)]
pub enum MemError {
    // ── STRUCTURAL WRITE REJECTIONS (Disposition::Rejected) ──────────────────
    #[error("Missing or untyped provenance on write (DC-1, I4): claim cannot be committed")]
    MissingProvenance,

    #[error("Caller does not hold write authority for agent_id {agent_id:?} (DC-2, I9)")]
    WriteAuthorityViolation { agent_id: AgentId },

    #[error("Malformed fact: {reason}")]
    MalformedFact { reason: String },

    #[error("Unknown or invalid agent_id: {agent_id:?}")]
    UnknownAgentId { agent_id: AgentId },

    #[error("Claim not found: {claim_ref:?}")]
    ClaimNotFound { claim_ref: ClaimRef },

    // ── CONCURRENCY ───────────────────────────────────────────────────────────
    #[error("Write lock for agent_id {agent_id:?} is already held (single-writer violation, DC-2)")]
    WriteLockContention { agent_id: AgentId },

    // ── ASYNC / SPAWN_BLOCKING BRIDGE ─────────────────────────────────────────
    /// Returned when a `tokio::task::spawn_blocking` call fails to join (W7 EngineHandle).
    #[error("spawn_blocking task failed: {reason}")]
    SpawnBlocking { reason: String },

    // ── INVARIANT VIOLATIONS (bugs — should never occur in correct impl) ──────
    #[error("I9 atomic commit unit violated: partial write detected for agent_id {agent_id:?}")]
    AtomicCommitViolation { agent_id: AgentId },

    #[error(
        "I10 monotonicity violated: belief changed between reads without an intervening write \
         for agent_id {agent_id:?}"
    )]
    MonotonicityViolation { agent_id: AgentId },

    #[error(
        "I3 violated: materialized belief cache disagrees with canonical fold \
         (cache must be subordinate)"
    )]
    BeliefCacheInconsistency,

    // ── TEMPORAL COHERENCE ────────────────────────────────────────────────────
    #[error(
        "Temporal coherence failure: valid_time_start ({start}) is after valid_time_end ({end})"
    )]
    IncoherentTemporalWindow { start: String, end: String },

    // ── PERSISTENCE ───────────────────────────────────────────────────────────
    #[error("Persistence error: {source}")]
    Persistence {
        #[from]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error("SQLite PRAGMA initialization failed: {reason}")]
    PragmaInitFailed { reason: String },

    // ── ORACLE PORT ───────────────────────────────────────────────────────────
    #[error("Oracle port error: {reason}")]
    OracleError { reason: String },

    #[error("Adjudication handle not found: {handle_id}")]
    AdjudicationHandleNotFound { handle_id: uuid::Uuid },

    // ── CONFIGURATION ─────────────────────────────────────────────────────────
    #[error("OP-3 calibration parameter invalid: {param} = {value}: {reason}")]
    ConfigurationError {
        param: String,
        value: String,
        reason: String,
    },
}

/// Write surface result — returned synchronously.
/// For heavy-path ops, disposition = QueuedForAdjudication;
/// final state arrives asynchronously via the oracle callback.
pub type WriteResult = Result<mempill_types::WriteOutcome, MemError>;

/// Belief projection result — returned from query_memory.
pub type BeliefResult = Result<mempill_types::BeliefProjection, MemError>;

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_types::AgentId;

    #[test]
    fn mem_error_missing_provenance_display() {
        let e = MemError::MissingProvenance;
        let s = e.to_string();
        assert!(s.contains("provenance"));
    }

    #[test]
    fn mem_error_malformed_fact_carries_reason() {
        let e = MemError::MalformedFact { reason: "empty subject".into() };
        assert!(e.to_string().contains("empty subject"));
    }

    #[test]
    fn mem_error_spawn_blocking_present_and_displays() {
        let e = MemError::SpawnBlocking { reason: "task panicked".into() };
        let s = e.to_string();
        assert!(s.contains("spawn_blocking"));
        assert!(s.contains("task panicked"));
    }

    #[test]
    fn mem_error_write_authority_violation_displays_agent_id() {
        let e = MemError::WriteAuthorityViolation {
            agent_id: AgentId("agent-42".into()),
        };
        assert!(e.to_string().contains("agent-42"));
    }

    #[test]
    fn mem_error_claim_not_found_displays_claim_ref() {
        let id = uuid::Uuid::new_v4();
        let e = MemError::ClaimNotFound {
            claim_ref: mempill_types::ClaimRef(id),
        };
        assert!(e.to_string().contains(&id.to_string()));
    }

    #[test]
    fn mem_error_atomic_commit_violation_displays_agent_id() {
        let e = MemError::AtomicCommitViolation {
            agent_id: AgentId("agent-99".into()),
        };
        assert!(e.to_string().contains("agent-99"));
    }

    #[test]
    fn mem_error_incoherent_temporal_window_displays_times() {
        let e = MemError::IncoherentTemporalWindow {
            start: "2025-01-02T00:00:00Z".into(),
            end: "2025-01-01T00:00:00Z".into(),
        };
        let s = e.to_string();
        assert!(s.contains("2025-01-02"));
        assert!(s.contains("2025-01-01"));
    }

    #[test]
    fn mem_error_oracle_error_carries_reason() {
        let e = MemError::OracleError { reason: "timeout".into() };
        assert!(e.to_string().contains("timeout"));
    }

    #[test]
    fn mem_error_configuration_error_displays_all_fields() {
        let e = MemError::ConfigurationError {
            param: "valid_time_confidence_threshold".into(),
            value: "-0.1".into(),
            reason: "must be in [0.0, 1.0]".into(),
        };
        let s = e.to_string();
        assert!(s.contains("valid_time_confidence_threshold"));
        assert!(s.contains("-0.1"));
        assert!(s.contains("must be in [0.0, 1.0]"));
    }

    #[test]
    fn mem_error_adjudication_handle_not_found() {
        let id = uuid::Uuid::new_v4();
        let e = MemError::AdjudicationHandleNotFound { handle_id: id };
        assert!(e.to_string().contains(&id.to_string()));
    }

    #[test]
    fn mem_error_pragma_init_failed() {
        let e = MemError::PragmaInitFailed { reason: "WAL failed".into() };
        assert!(e.to_string().contains("WAL failed"));
    }

    #[test]
    fn mem_error_is_debug() {
        let e = MemError::MissingProvenance;
        let s = format!("{:?}", e);
        assert!(s.contains("MissingProvenance"));
    }
}
