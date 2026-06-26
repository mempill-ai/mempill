//! Application layer — public use-cases and DTOs.
//!
//! All items here are `pub` — this is the stable public API surface consumed by bindings.
//! Engine internals in `engine/` remain `pub(crate)`.

pub mod audit;
pub mod dto;
pub mod ingest_claim;
pub mod query_history;
pub mod query_memory;
pub mod reconcile;
pub mod submit_adjudication;
pub mod sweep_adjudications;

pub use audit::AuditUseCase;
pub use dto::{
    AuditQueryRequest, AuditQueryResponse, HistoryEntry, IngestClaimRequest, IngestClaimResponse,
    QueryHistoryRequest, QueryHistoryResponse, QueryMemoryRequest, QueryMemoryResponse,
    ReconcileRequest, ReconcileResponse,
};
pub use ingest_claim::IngestClaimUseCase;
pub use query_history::QueryHistoryUseCase;
pub use query_memory::QueryMemoryUseCase;
pub use reconcile::ReconcileUseCase;
pub use submit_adjudication::SubmitAdjudicationUseCase;
