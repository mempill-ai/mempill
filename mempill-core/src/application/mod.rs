//! Application layer — public use-cases and DTOs (§4a, A27, A28, A29).
//!
//! All items here are `pub` — this is the stable public API surface consumed by bindings.
//! Engine internals in `engine/` remain `pub(crate)`.

pub mod audit;
pub mod dto;
pub mod ingest_claim;
pub mod query_memory;
pub mod reconcile;

pub use audit::AuditUseCase;
pub use dto::{
    AuditQueryRequest, AuditQueryResponse, IngestClaimRequest, IngestClaimResponse,
    QueryMemoryRequest, QueryMemoryResponse, ReconcileRequest, ReconcileResponse,
};
pub use ingest_claim::IngestClaimUseCase;
pub use query_memory::QueryMemoryUseCase;
pub use reconcile::ReconcileUseCase;
