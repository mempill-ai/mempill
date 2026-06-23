//! Python exception hierarchy for mempill.
//!
//! Exception tree (all subclass MempillError, which subclasses Exception):
//!   MempillError
//!     ├─ ValidationError  — structural/domain rejections
//!     ├─ NotFoundError    — entity-lookup failures
//!     ├─ ConflictError    — concurrency / write-lock contention
//!     ├─ StorageError     — persistence-layer failures
//!     ├─ ConfigError      — calibration/config param failures
//!     └─ InternalError    — invariant violations (bugs)

use pyo3::prelude::*;
use mempill_core::error::MemError;

// ── Exception hierarchy ───────────────────────────────────────────────────────

pyo3::create_exception!(mempill._mempill, MempillError, pyo3::exceptions::PyException,
    "Base exception for all mempill errors.");

pyo3::create_exception!(mempill._mempill, ValidationError, MempillError,
    "Raised when a write is rejected due to structural/domain invariant violations.");

pyo3::create_exception!(mempill._mempill, NotFoundError, MempillError,
    "Raised when a requested entity (claim, agent, adjudication handle) does not exist.");

pyo3::create_exception!(mempill._mempill, ConflictError, MempillError,
    "Raised when a write-lock contention conflict is detected.");

pyo3::create_exception!(mempill._mempill, StorageError, MempillError,
    "Raised when the persistence layer encounters an error.");

pyo3::create_exception!(mempill._mempill, ConfigError, MempillError,
    "Raised when a calibration or configuration parameter is invalid.");

pyo3::create_exception!(mempill._mempill, InternalError, MempillError,
    "Raised when an internal engine invariant is violated (indicates a bug).");

// ── MemError → PyErr conversion ───────────────────────────────────────────────
// Cannot impl From<MemError> for PyErr directly (orphan rule).
// Use a free function instead; call via `mem_err_to_pyerr(e)` or `?` with `.map_err`.
//
// NO wildcard arm — every variant is matched explicitly so that adding a new
// MemError variant causes a compile error, forcing explicit mapping.

pub fn mem_err_to_pyerr(e: MemError) -> PyErr {
    let msg = e.to_string();
    match e {
        // ValidationError group (ARCHITECTURE.md §4 table)
        MemError::MissingProvenance => ValidationError::new_err(msg),
        MemError::WriteAuthorityViolation { .. } => ValidationError::new_err(msg),
        MemError::MalformedFact { .. } => ValidationError::new_err(msg),
        MemError::IncoherentTemporalWindow { .. } => ValidationError::new_err(msg),

        // NotFoundError group
        MemError::ClaimNotFound { .. } => NotFoundError::new_err(msg),
        MemError::UnknownAgentId { .. } => NotFoundError::new_err(msg),
        MemError::AdjudicationHandleNotFound { .. } => NotFoundError::new_err(msg),

        // ConflictError group
        MemError::WriteLockContention { .. } => ConflictError::new_err(msg),

        // StorageError group
        MemError::Persistence { .. } => StorageError::new_err(msg),
        MemError::PragmaInitFailed { .. } => StorageError::new_err(msg),
        // PendingStore is a storage-layer failure (W3 pending_adjudications write path).
        MemError::PendingStore { .. } => StorageError::new_err(msg),

        // ConfigError group
        MemError::ConfigurationError { .. } => ConfigError::new_err(msg),
        MemError::OracleError { .. } => ConfigError::new_err(msg),

        // InternalError group
        MemError::SpawnBlocking { .. } => InternalError::new_err(msg),
        MemError::AtomicCommitViolation { .. } => InternalError::new_err(msg),
        MemError::MonotonicityViolation { .. } => InternalError::new_err(msg),
        MemError::BeliefCacheInconsistency => InternalError::new_err(msg),
    }
}

/// Register all exception types on the module.
pub fn register_exceptions(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("MempillError", m.py().get_type::<MempillError>())?;
    m.add("ValidationError", m.py().get_type::<ValidationError>())?;
    m.add("NotFoundError", m.py().get_type::<NotFoundError>())?;
    m.add("ConflictError", m.py().get_type::<ConflictError>())?;
    m.add("StorageError", m.py().get_type::<StorageError>())?;
    m.add("ConfigError", m.py().get_type::<ConfigError>())?;
    m.add("InternalError", m.py().get_type::<InternalError>())?;
    Ok(())
}
