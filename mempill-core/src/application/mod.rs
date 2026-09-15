//! Application layer — public use-cases and DTOs.
//!
//! All items here are `pub` — this is the stable public API surface consumed by bindings.
//! Engine internals in `engine/` remain `pub(crate)`.

pub mod assert_validity;
pub mod audit;
pub mod dto;
pub mod ingest_claim;
pub mod query_history;
pub mod query_memory;
pub mod query_subject;
pub mod reconcile;
pub mod submit_adjudication;
pub mod sweep_adjudications;

pub use assert_validity::AssertValidityUseCase;
pub use audit::AuditUseCase;
pub use dto::{
    AssertValidityRequest, AssertValidityResponse, AuditQueryRequest, AuditQueryResponse,
    HistoryEntry, IngestClaimRequest, IngestClaimResponse, LiveClaimResolution,
    QueryHistoryRequest, QueryHistoryResponse, QueryMemoryRequest, QueryMemoryResponse,
    QuerySubjectRequest, QuerySubjectResponse, SubjectFactEntry,
    ReconcileRequest, ReconcileResponse, ValidityAssertionInput,
};
pub use ingest_claim::IngestClaimUseCase;
pub use query_history::QueryHistoryUseCase;
pub use query_memory::QueryMemoryUseCase;
pub use query_subject::QuerySubjectUseCase;
pub use reconcile::ReconcileUseCase;
pub use submit_adjudication::SubmitAdjudicationUseCase;
