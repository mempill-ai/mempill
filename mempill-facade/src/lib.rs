//! # mempill
//!
//! Temporally-correct memory for AI agents.
//!
//! This crate is a thin facade that re-exports the public API of
//! [`mempill-core`](https://docs.rs/mempill-core) and makes the persistence
//! adapters available behind feature flags so a downstream user only needs:
//!
//! ```toml
//! # Cargo.toml
//! [dependencies]
//! mempill = "0.3"                          # default features = ["sqlite"]
//! # or:
//! mempill = { version = "0.3", features = ["postgres"] }
//! ```
//!
//! ## Quick start (SQLite, default)
//!
//! Most code only needs two calls — [`remember`] and [`recall`] — with sane defaults:
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use mempill::{open_default_in_memory, remember, recall, RememberOptions};
//!
//! let engine = open_default_in_memory()?;
//!
//! // Remember a fact — 3 args + sane defaults. Dates are lenient: "2020",
//! // "2020-03", "2020-03-01", or full RFC3339 all work.
//! remember(&engine, "my-agent", "user", "city", "Berlin",
//!          RememberOptions::default().valid_from("2020")).await?;
//!
//! // Two conflicting facts are NEVER silently overwritten — they surface as Contested.
//! remember(&engine, "my-agent", "acme:ceo", "held_by", "Alice", RememberOptions::default()).await?;
//! remember(&engine, "my-agent", "acme:ceo", "held_by", "Bob",   RememberOptions::default()).await?;
//!
//! // Recall — a flat result; Contested is explicit (can't be mistaken for "no memory").
//! let r = recall(&engine, "my-agent", "acme:ceo", "held_by").await?;
//! if r.is_contested() {
//!     println!("contested: {:?}", r.candidates);
//! } else {
//!     println!("ceo = {:?}", r.as_str());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Need full control — provenance channels, cardinality, criticality, explicit confidence,
//! or derivation lineage? Drop to the full claim API ([`engine::IngestClaimRequest`] /
//! [`engine::QueryMemoryRequest`]); see the type reference. The ergonomic tier is additive — the
//! rigorous core is unchanged.
//!
//! ## Feature flags
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `sqlite` | yes | Enables [`mempill_sqlite`] — embedded SQLite adapter (topology-a) |
//! | `postgres` | no | Enables `mempill_postgres` — shared PostgreSQL adapter (topology-b) |
//!
//! Both features can be enabled simultaneously (e.g., for tests that verify both backends).
//!
//! ## Architecture
//!
//! The dependency direction is one-way:
//!
//! ```text
//! mempill (this facade)
//!   ├── mempill-core   (engine, port traits, use-cases)
//!   ├── mempill-sqlite (feature = "sqlite")
//!   └── mempill-postgres (feature = "postgres")
//! ```
//!
//! The engine core has zero dependency on either adapter crate.

#![warn(missing_docs)]

// ── Tier-1 ergonomic modules ──────────────────────────────────────────────────

pub mod ergonomic;
pub mod date;

// ── Tier-1 surface re-exports (kept at crate root — quickstarts stay valid) ──

pub use ergonomic::{
    // Functions
    remember,
    recall,
    history,
    // Option builder
    RememberOptions,
    // Return types
    RememberReceipt,
    RecallResult,
    ContestCandidate,
    BeliefDetail,
    History,
    HistoryEntry,
    HistoryEntryStatus,
    // Error
    MempillDxError,
    // Seam traits (for advanced users who write generic code over the engine)
    CanIngestClaim,
    CanQueryMemory,
    CanQueryHistory,
    // Tier-2 builder
    IngestClaimRequestExt,
    IngestClaimRequestBuilder,
};

// ── Power-user modules ────────────────────────────────────────────────────────

/// Domain value types shared across the mempill engine.
///
/// Import from here when you need the deep type surface: provenance channels,
/// adjudication request/response, ledger entries, claim edges, validity assertions,
/// and so on. Most consumers only need the ergonomic tier at the crate root.
///
/// # Example
///
/// ```rust
/// use mempill::types::{Disposition, ProvenanceLabel, ExternalKind};
/// ```
pub mod types {
    // Identity
    pub use mempill_types::{AgentId, ClaimRef, SubjectLineRef};
    // Provenance
    pub use mempill_types::{ProvenanceLabel, ExternalKind, ExternalAnchor};
    // Claim value objects
    pub use mempill_types::{Cardinality, Confidence, Criticality, Fact, Claim};
    // Disposition (12-state model)
    pub use mempill_types::{Disposition, WriteOutcome};
    // Belief projection (read-time)
    pub use mempill_types::{Belief, BeliefProjection, BeliefStatus, CurrencySignal, CurrencyState, StalenessFlag, Marker};
    // Time
    pub use mempill_types::{TransactionTime, ValidTime};
    // Ledger
    pub use mempill_types::{LedgerEntry, LedgerEventKind};
    // Validity
    pub use mempill_types::{ValidityAssertion, AssertionKind};
    // Graph edges
    pub use mempill_types::{ClaimEdge, EdgeKind};
    // Oracle adjudication
    pub use mempill_types::{ClaimProposal, AdjudicationRequest, AdjudicationResponse, AdjudicationVerdict, AdjudicationOutcome, OverturnReason};
}

/// Core engine surface for power users and adapter authors.
///
/// Contains the `EngineHandle`, configuration, port traits, NoOp stubs, and use-case
/// request/response DTOs. Most consumers only need the ergonomic tier at the crate root.
///
/// # Example
///
/// ```rust,no_run
/// use mempill::engine::{EngineConfig, NoOpOracle, NoOpVector};
/// ```
pub mod engine {
    // EngineHandle — sole async entry point
    pub use mempill_core::EngineHandle;
    // Configuration
    pub use mempill_core::EngineConfig;
    // Error types
    pub use mempill_core::{MemError, WriteResult, BeliefResult};
    // Port traits
    pub use mempill_core::{PersistencePort, OraclePort, ExtractorPort, EmbeddingPort, VectorPort, PendingAdjudicationPort, PendingAdjudicationRow};
    // NoOp stubs for tests / simple setups
    pub use mempill_core::{NoOpOracle, NoOpVector};
    // Use-case request/response DTOs
    pub use mempill_core::{
        IngestClaimRequest, IngestClaimResponse,
        QueryMemoryRequest, QueryMemoryResponse,
        ReconcileRequest, ReconcileResponse,
        AuditQueryRequest, AuditQueryResponse,
        QueryHistoryRequest, QueryHistoryResponse,
    };
    // Use-case traits
    pub use mempill_core::{IngestClaimUseCase, QueryMemoryUseCase, ReconcileUseCase, AuditUseCase, QueryHistoryUseCase};
}

// ── Flat re-exports of commonly-needed types ──────────────────────────────────
//
// Keep the most-used power-user types at the crate root for ergonomic imports
// without requiring `use mempill::types::*`. Power-user-only types live in
// `mempill::types` and `mempill::engine` modules.

pub use mempill_types::{
    // Identity (needed for RememberOptions::derived_from and builder calls)
    AgentId,
    ClaimRef,
    SubjectLineRef,
    // Provenance (needed to call builder .provenance())
    ProvenanceLabel,
    ExternalKind,
    // Claim value objects (needed for builder .cardinality() / .criticality())
    Cardinality,
    Criticality,
    Confidence,
    Fact,
    // Disposition (returned in RememberReceipt)
    Disposition,
    // Belief status (in RecallResult)
    BeliefStatus,
    CurrencyState,
    // Marker (in RecallResult via BeliefProjection — kept for match arms)
    Marker,
    // Time types (used in Tier-2 bi-temporal examples)
    ValidTime,
    // Date granularity — DISPLAY-ONLY precision hint surfaced in BeliefDetail
    DateGranularity,
};

pub use mempill_core::{
    // EngineHandle (returned by open_default_in_memory / open_default)
    EngineHandle,
    // Error types (used in ? chains)
    MemError,
    // Commonly-used request types (Tier-2 usage)
    IngestClaimRequest,
    QueryMemoryRequest,
};

// ── Adapter re-exports (behind feature flags) ─────────────────────────────────

/// SQLite persistence adapter (`feature = "sqlite"`).
///
/// Use [`sqlite::open_default_in_memory`] or [`sqlite::open_default`] to open an engine.
#[cfg(feature = "sqlite")]
pub mod sqlite {
    pub use mempill_sqlite::{
        DefaultEngine,
        OracleEngine,
        SqlitePersistenceStore,
        SqlitePendingStore,
        SqliteStoreError,
        open_default,
        open_default_in_memory,
        open_with_oracle,
        open_with_oracle_in_memory,
    };
}

/// PostgreSQL persistence adapter (`feature = "postgres"`).
///
/// Use [`postgres::open_postgres`] to open an engine connected to PostgreSQL.
/// Note: NoTls only in v0.3.
#[cfg(feature = "postgres")]
pub mod postgres {
    pub use mempill_postgres::{
        PostgresEngine,
        PostgresPersistenceStore,
        PostgresPendingStore,
        PostgresTxn,
        PostgresStoreError,
        open_postgres,
        open_postgres_with_oracle,
    };
}

// ── Convenience top-level functions ──────────────────────────────────────────

/// Open an in-memory [`sqlite::DefaultEngine`].
///
/// Convenience shortcut for `mempill::sqlite::open_default_in_memory()`.
/// Requires the `sqlite` feature (enabled by default).
///
/// # Errors
/// Returns [`sqlite::SqliteStoreError`] if the connection cannot be opened.
///
/// # Example
///
/// ```rust
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = mempill::open_default_in_memory()?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
pub fn open_default_in_memory() -> Result<sqlite::DefaultEngine, sqlite::SqliteStoreError> {
    mempill_sqlite::open_default_in_memory()
}

/// Open a file-backed [`sqlite::DefaultEngine`] at the given path.
///
/// Convenience shortcut for `mempill::sqlite::open_default(path)`.
/// Requires the `sqlite` feature (enabled by default).
///
/// # Errors
/// Returns [`sqlite::SqliteStoreError`] if the connection cannot be opened or migrations fail.
#[cfg(feature = "sqlite")]
pub fn open_default(path: &str) -> Result<sqlite::DefaultEngine, sqlite::SqliteStoreError> {
    mempill_sqlite::open_default(path)
}
