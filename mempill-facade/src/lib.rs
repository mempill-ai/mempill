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
//! mempill = "0.2"                          # default features = ["sqlite"]
//! # or:
//! mempill = { version = "0.2", features = ["postgres"] }
//! ```
//!
//! ## Quick start (SQLite, default)
//!
//! Most code only needs two calls — [`remember`] and [`recall`] — with sane defaults:
//!
//! ```text
//! // Cargo.toml
//! // [dependencies]
//! // mempill = "0.2"
//! // tokio   = { version = "1", features = ["rt-multi-thread", "macros"] }
//!
//! use mempill::{open_default_in_memory, remember, recall, RememberOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let engine = open_default_in_memory()?;
//!
//!     // Remember a fact — 3 args + sane defaults. Dates are lenient: "2020",
//!     // "2020-03", "2020-03-01", or full RFC3339 all work.
//!     remember(&engine, "my-agent", "user", "city", "Berlin",
//!              RememberOptions::default().valid_from("2020")).await?;
//!
//!     // Two conflicting facts are NEVER silently overwritten — they surface as Contested.
//!     remember(&engine, "my-agent", "acme:ceo", "held_by", "Alice", RememberOptions::default()).await?;
//!     remember(&engine, "my-agent", "acme:ceo", "held_by", "Bob",   RememberOptions::default()).await?;
//!
//!     // Recall — a flat result; Contested is explicit (can't be mistaken for "no memory").
//!     let r = recall(&engine, "my-agent", "acme:ceo", "held_by").await?;
//!     if r.is_contested() {
//!         println!("contested: {:?}", r.candidates);
//!     } else {
//!         println!("ceo = {:?}", r.as_str());
//!     }
//!     Ok(())
//! }
//! ```
//!
//! Need full control — provenance channels, cardinality, criticality, explicit confidence,
//! or derivation lineage? Drop to the full claim API ([`IngestClaimRequest`] /
//! [`QueryMemoryRequest`]); see the type reference. The ergonomic tier is additive — the
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

// ── Tier-1 ergonomic modules ──────────────────────────────────────────────────

pub mod ergonomic;
pub mod date;

// ── Tier-1 surface re-exports ─────────────────────────────────────────────────

pub use ergonomic::{
    // Functions
    remember,
    recall,
    // Option builder
    RememberOptions,
    // Return types
    RememberReceipt,
    RecallResult,
    ContestCandidate,
    // Error
    MempillDxError,
    // Seam traits (for advanced users who write generic code over the engine)
    CanIngestClaim,
    CanQueryMemory,
    // Tier-2 builder
    IngestClaimRequestExt,
    IngestClaimRequestBuilder,
};

// ── Domain-type re-exports (mempill-types) ───────────────────────────────────
//
// Re-export the complete set of domain types a consumer needs so they can write
// `use mempill::AgentId` without adding `mempill-types` to their own Cargo.toml.

pub use mempill_types::{
    // Identity
    AgentId,
    ClaimRef,
    SubjectLineRef,
    // Provenance
    ProvenanceLabel,
    ExternalKind,
    ExternalAnchor,
    // Claim value objects
    Cardinality,
    Confidence,
    Criticality,
    Fact,
    Claim,
    // Disposition (12-state model)
    Disposition,
    WriteOutcome,
    // Belief projection (read-time)
    Belief,
    BeliefProjection,
    BeliefStatus,
    CurrencySignal,
    CurrencyState,
    StalenessFlag,
    Marker,
    // Time
    TransactionTime,
    ValidTime,
    // Ledger
    LedgerEntry,
    LedgerEventKind,
    // Validity
    ValidityAssertion,
    AssertionKind,
    // Graph edges
    ClaimEdge,
    EdgeKind,
    // Oracle adjudication
    ClaimProposal,
    AdjudicationRequest,
    AdjudicationResponse,
    AdjudicationVerdict,
    AdjudicationOutcome,
    OverturnReason,
};

// ── Core re-exports ───────────────────────────────────────────────────────────

/// Re-exports of the complete mempill-core public API.
pub use mempill_core::{
    // EngineHandle — sole async entry point
    EngineHandle,
    ErasedPendingStore,
    ErasedPendingStoreAdapter,
    // Configuration
    EngineConfig,
    // Error types
    MemError,
    WriteResult,
    BeliefResult,
    // Port traits
    PersistencePort,
    OraclePort,
    ExtractorPort,
    EmbeddingPort,
    VectorPort,
    PendingAdjudicationPort,
    PendingAdjudicationRow,
    Txn,
    // NoOp stubs for tests / simple setups
    NoOpOracle,
    NoOpVector,
    // Use-case request/response DTOs
    IngestClaimRequest,
    IngestClaimResponse,
    QueryMemoryRequest,
    QueryMemoryResponse,
    ReconcileRequest,
    ReconcileResponse,
    AuditQueryRequest,
    AuditQueryResponse,
    // Use-case traits
    IngestClaimUseCase,
    QueryMemoryUseCase,
    ReconcileUseCase,
    AuditUseCase,
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
        OraclePort,
        open_default,
        open_default_in_memory,
        open_with_oracle,
        open_with_oracle_in_memory,
    };
}

/// PostgreSQL persistence adapter (`feature = "postgres"`).
///
/// Use [`postgres::open_postgres`] to open an engine connected to PostgreSQL.
/// Note: NoTls only in v0.2 — TLS is planned for v0.3.1.
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
