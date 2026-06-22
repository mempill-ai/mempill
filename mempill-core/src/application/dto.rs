//! Public DTOs — the stable API surface consumed by all bindings (§4a).
//!
//! Domain types from `mempill-types` are referenced here but raw internal engine types
//! never cross this boundary; callers only see these structs.

use mempill_types::{
    AgentId, BeliefProjection, Cardinality, ClaimRef, Confidence, Criticality, Disposition,
    LedgerEntry, ProvenanceLabel, ValidTime,
};

// ── INGEST CLAIM ──────────────────────────────────────────────────────────────

/// Public write request. Maps to domain Claim at the application boundary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestClaimRequest {
    pub agent_id: AgentId,
    pub subject: String,
    pub predicate: String,
    pub value: serde_json::Value,
    /// Required; no default imposed here — gateway enforces ModelDerived default for model output.
    pub provenance: ProvenanceLabel,
    /// Caller proposal; gated by C7.
    pub cardinality: Cardinality,
    /// None = unknown; fallback to tx_time ordering.
    pub valid_time: Option<ValidTime>,
    pub confidence: Confidence,
    pub criticality: Criticality,
    /// Lineage for ModelDerived claims.
    pub derived_from: Vec<ClaimRef>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestClaimResponse {
    pub claim_ref: ClaimRef,
    pub disposition: Disposition,
    /// Populated when disposition is Contested or PendingConflict.
    pub contested_with: Vec<ClaimRef>,
}

// ── QUERY MEMORY ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryMemoryRequest {
    pub agent_id: AgentId,
    pub subject: String,
    pub predicate: String,
    /// Optional: query as of a specific transaction time (bi-temporal as-of query).
    pub as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryMemoryResponse {
    /// Canonical fold result; never stored (I3).
    pub belief: BeliefProjection,
}

// ── RECONCILE ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcileRequest {
    pub agent_id: AgentId,
    /// Subject lines to reconcile. Empty = reconcile all subject lines for agent_id.
    pub subject_lines: Vec<(String, String)>, // (subject, predicate) pairs
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcileResponse {
    /// Per-claim disposition outcomes from the reconciliation pass.
    pub outcomes: Vec<(ClaimRef, Disposition)>,
    /// Number of subject lines that required oracle escalation.
    pub oracle_escalations: u32,
}

// ── AUDIT QUERY ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditQueryRequest {
    pub agent_id: AgentId,
    /// None = load full ledger for agent_id.
    pub claim_ref: Option<ClaimRef>,
    pub from_tx_time: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditQueryResponse {
    pub entries: Vec<LedgerEntry>,
}
