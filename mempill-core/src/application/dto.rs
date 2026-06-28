//! Public DTOs — the stable API surface consumed by all bindings.
//!
//! Domain types from `mempill-types` are referenced here but raw internal engine types
//! never cross this boundary; callers only see these structs.

use mempill_types::{
    AgentId, BeliefProjection, Cardinality, ClaimRef, Confidence, Criticality, Disposition,
    HistoryEntryStatus, LedgerEntry, ProvenanceLabel, ValidTime,
};

// ── INGEST CLAIM ──────────────────────────────────────────────────────────────

/// Public write request. Maps to domain Claim at the application boundary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestClaimRequest {
    /// The agent performing the write.
    pub agent_id: AgentId,
    /// Opaque key — the entity the claim is about (e.g. `"acme:ceo"`). mempill does
    /// **not** perform entity resolution; you must use the same `subject` on write and
    /// read. Adopt a canonical key convention and apply it consistently across both paths.
    pub subject: String,
    /// Opaque key — the property being asserted (e.g. `"held_by"`). Like [`Self::subject`],
    /// it is matched verbatim; the engine cannot reconcile differently-keyed facts for you.
    pub predicate: String,
    /// The JSON value being asserted.
    pub value: serde_json::Value,
    /// Required; no default imposed here — gateway enforces ModelDerived default for model output.
    pub provenance: ProvenanceLabel,
    /// Caller-supplied cardinality hint; the adjudication gate may override or contest it.
    pub cardinality: Cardinality,
    /// None = unknown; fallback to tx_time ordering.
    pub valid_time: Option<ValidTime>,
    /// Confidence in the value and valid-time assertion (0.0–1.0 each).
    pub confidence: Confidence,
    /// Criticality class for this claim.
    pub criticality: Criticality,
    /// Lineage for ModelDerived claims.
    pub derived_from: Vec<ClaimRef>,
}

/// Response from a successful claim ingest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IngestClaimResponse {
    /// Stable UUID reference to the committed claim.
    pub claim_ref: ClaimRef,
    /// The engine's disposition for this write.
    pub disposition: Disposition,
    /// Populated when disposition is Contested or PendingConflict.
    pub contested_with: Vec<ClaimRef>,
}

// ── QUERY MEMORY ──────────────────────────────────────────────────────────────

/// Request to retrieve the current belief for a (subject, predicate) subject-line.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryMemoryRequest {
    /// The agent whose memory is queried.
    pub agent_id: AgentId,
    /// The subject of the query.
    pub subject: String,
    /// The predicate of the query.
    pub predicate: String,
    /// Optional: query as of a specific transaction time (bi-temporal as-of query).
    ///
    /// When set, only claims whose transaction time is at or before this instant are
    /// considered (the transaction-time axis). Controls assertion visibility as well.
    pub as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional: select the belief valid at this specific valid-time instant (valid-time axis).
    ///
    /// When set, after the transaction-time visibility filter is applied, the fold
    /// narrows the result to the single claim whose valid-time window contains this
    /// instant (D2 independence rule: tx-time filter first, then valid-time selection).
    ///
    /// When `None`, the existing backward-compatible behaviour is preserved: the
    /// `as_of_tx_time` (or `now`) is used as the valid-time selection instant.
    #[serde(default)]
    pub valid_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Response from a memory query — the canonical belief projection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryMemoryResponse {
    /// Canonical fold result; computed at read time, never persisted.
    pub belief: BeliefProjection,
}

// ── RECONCILE ─────────────────────────────────────────────────────────────────

/// Request to reconcile one or more subject lines.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcileRequest {
    /// The agent whose subject lines are reconciled.
    pub agent_id: AgentId,
    /// Subject lines to reconcile. Empty = reconcile all subject lines for agent_id.
    pub subject_lines: Vec<(String, String)>, // (subject, predicate) pairs
}

/// Response from a reconciliation pass.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcileResponse {
    /// Per-claim disposition outcomes from the reconciliation pass.
    pub outcomes: Vec<(ClaimRef, Disposition)>,
    /// Number of subject lines that required oracle escalation.
    pub oracle_escalations: u32,
}

// ── QUERY HISTORY ────────────────────────────────────────────────────────────

/// Request to retrieve the full history timeline for a (subject, predicate) subject-line.
///
/// Returns all claims ever written to the line, ordered by the canonical ordering key
/// (valid_time_start when confidence ≥ threshold, else tx_time). Each entry is tagged
/// `Current` or `Superseded` based on the same canonical fold that powers `query_memory`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryHistoryRequest {
    /// The agent whose history is queried.
    pub agent_id: AgentId,
    /// The subject of the history query.
    pub subject: String,
    /// The predicate of the history query.
    pub predicate: String,
}

/// One slot in the history timeline for a subject-line.
///
/// `status` is derived from `is_live` in the canonical fold — the `Current` entry is
/// exactly the claim that `recall` / `query_memory` would return as primary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    /// Stable reference to the underlying claim (UUID).
    pub claim_ref: ClaimRef,
    /// The asserted value for this claim.
    pub value: serde_json::Value,
    /// Start of the valid-time window, or `None` if unknown.
    pub valid_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Effective end of the slot: equals the successor's canonical ordering key,
    /// or `None` for the open-ended current slot.
    pub valid_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether this claim is the live belief or has been superseded.
    pub status: HistoryEntryStatus,
    /// Human-readable provenance label (e.g. `"External/UserAsserted"`).
    pub provenance: String,
    /// Confidence in the claim's value (0.0–1.0).
    pub value_confidence: f32,
}

/// Response from `query_history` — the full ordered timeline for a subject-line.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueryHistoryResponse {
    /// All claims for the subject-line, ordered by canonical ordering key (oldest first).
    pub entries: Vec<HistoryEntry>,
}

impl QueryHistoryResponse {
    /// Convenience: returns the single `Current` entry, if any.
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.entries.iter().find(|e| e.status == HistoryEntryStatus::Current)
    }
}

// ── AUDIT QUERY ───────────────────────────────────────────────────────────────

/// Request to query the audit ledger.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditQueryRequest {
    /// The agent whose audit ledger is queried.
    pub agent_id: AgentId,
    /// None = load full ledger for agent_id.
    pub claim_ref: Option<ClaimRef>,
    /// Filter to entries recorded at or after this transaction time.
    pub from_tx_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Maximum number of entries to return.
    pub limit: usize,
}

/// Response from an audit ledger query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditQueryResponse {
    /// The matching audit ledger entries.
    pub entries: Vec<LedgerEntry>,
}
