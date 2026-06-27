//! MemError — top-level error type for the mempill engine.
//!
//! Every invariant violation surfaces as a typed variant — never silently swallowed.
//! Uses `thiserror` for ergonomic `Display` + `Error` implementations.

use thiserror::Error;
use mempill_types::{AgentId, ClaimRef};

/// Top-level error type for the mempill engine.
/// Every invariant violation surfaces as a typed variant here — never silently swallowed.
#[derive(Debug, Error)]
pub enum MemError {
    // ── STRUCTURAL WRITE REJECTIONS (Disposition::Rejected) ──────────────────
    /// Provenance label is absent on the write request.
    #[error("Missing or untyped provenance on write: claim cannot be committed without a provenance label")]
    MissingProvenance,

    /// The caller does not hold write authority for the specified `agent_id`.
    #[error("Caller does not hold write authority for agent_id {agent_id:?}")]
    WriteAuthorityViolation {
        /// The agent ID that the caller attempted to write on behalf of.
        agent_id: AgentId,
    },

    /// The fact payload is structurally invalid (empty subject, invalid JSON, etc.).
    #[error("Malformed fact: {reason}")]
    MalformedFact {
        /// Human-readable description of the malformation.
        reason: String,
    },

    /// The supplied `agent_id` is not recognised by the engine.
    #[error("Unknown or invalid agent_id: {agent_id:?}")]
    UnknownAgentId {
        /// The unrecognised agent ID.
        agent_id: AgentId,
    },

    /// The referenced claim does not exist in the store.
    #[error("Claim not found: {claim_ref:?}")]
    ClaimNotFound {
        /// The claim reference that was not found.
        claim_ref: ClaimRef,
    },

    // ── CONCURRENCY ───────────────────────────────────────────────────────────
    /// Single-writer-per-agent-id invariant violated: the write lock is already held.
    #[error("Write lock for agent_id {agent_id:?} is already held (single-writer-per-agent-id violation)")]
    WriteLockContention {
        /// The agent ID whose write lock is already held.
        agent_id: AgentId,
    },

    // ── ASYNC / SPAWN_BLOCKING BRIDGE ─────────────────────────────────────────
    /// Returned when a `tokio::task::spawn_blocking` call fails to join at the EngineHandle async boundary.
    #[error("spawn_blocking task failed: {reason}")]
    SpawnBlocking {
        /// Description of the join error.
        reason: String,
    },

    // ── INVARIANT VIOLATIONS (bugs — should never occur in correct impl) ──────
    /// Partial write detected — atomic commit unit invariant violated.
    #[error("Atomic commit unit violated: partial write detected for agent_id {agent_id:?}")]
    AtomicCommitViolation {
        /// The agent ID for which the partial write was detected.
        agent_id: AgentId,
    },

    /// Belief changed between reads without an intervening write — fixed-history monotonicity violated.
    #[error(
        "Fixed-history monotonicity violated: belief changed between reads without an intervening write \
         for agent_id {agent_id:?}"
    )]
    MonotonicityViolation {
        /// The agent ID for which monotonicity was violated.
        agent_id: AgentId,
    },

    /// Materialized belief cache disagrees with the canonical fold result.
    #[error(
        "Belief cache inconsistency: materialized belief cache disagrees with canonical fold \
         (cache must be subordinate)"
    )]
    BeliefCacheInconsistency,

    // ── TEMPORAL COHERENCE ────────────────────────────────────────────────────
    /// `valid_time_start` is after `valid_time_end`.
    #[error(
        "Temporal coherence failure: valid_time_start ({start}) is after valid_time_end ({end})"
    )]
    IncoherentTemporalWindow {
        /// The valid-time start (RFC3339).
        start: String,
        /// The valid-time end (RFC3339).
        end: String,
    },

    // ── PERSISTENCE ───────────────────────────────────────────────────────────
    /// Underlying persistence adapter returned an error.
    #[error("Persistence error: {source}")]
    Persistence {
        /// The wrapped persistence error.
        #[from]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// SQLite PRAGMA initialization failed (WAL, FULL sync, foreign keys).
    #[error("SQLite PRAGMA initialization failed: {reason}")]
    PragmaInitFailed {
        /// Description of the PRAGMA failure.
        reason: String,
    },

    // ── ORACLE PORT ───────────────────────────────────────────────────────────
    /// Oracle port returned an error during `request_adjudication` or another oracle call site.
    /// Use `OracleError { reason: e.to_string() }` — string-reason convention is consistent
    /// across all non-persistence, non-internal error variants.
    #[error("Oracle port error: {reason}")]
    OracleError {
        /// Description of the oracle error.
        reason: String,
    },

    /// Pending-adjudication store error from insert_pending or mark_resolved.
    #[error("Pending-adjudication store error: {source}")]
    PendingStore {
        /// The wrapped pending-store error.
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// The adjudication handle is unknown, expired, or has already been resolved.
    #[error("Adjudication handle not found: {handle_id}")]
    AdjudicationHandleNotFound {
        /// The handle UUID that was not found.
        handle_id: uuid::Uuid,
    },

    // ── CONFIGURATION ─────────────────────────────────────────────────────────
    /// An engine calibration parameter has an invalid value.
    #[error("Engine calibration parameter invalid: {param} = {value}: {reason}")]
    ConfigurationError {
        /// The parameter name.
        param: String,
        /// The invalid value.
        value: String,
        /// Why the value is invalid.
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
        let s = format!("{e:?}");
        assert!(s.contains("MissingProvenance"));
    }
}
