//! Public DTOs — the stable API surface consumed by all bindings.
//!
//! Domain types from `mempill-types` are referenced here but raw internal engine types
//! never cross this boundary; callers only see these structs.

use mempill_types::{
    AgentId, AssertionKind, BeliefProjection, Cardinality, ClaimRef, Confidence, Criticality,
    DateGranularity, Disposition, HistoryEntryStatus, LedgerEntry, ProvenanceLabel, ValidTime,
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

// ── ASSERT VALIDITY (SDK_CONTRACT.md §3.1) ──────────────────────────────────

/// The validity assertion the host wants to apply to `target` — bound (close) or reopen.
///
/// Deliberately distinct from `mempill_types::AssertionKind`: this is the *request* shape
/// (the host supplies `at`; `Reopen` carries no timestamp — the engine stamps `now`),
/// while `AssertionKind` is the persisted, engine-stamped record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ValidityAssertionInput {
    /// Close `target`'s valid-time window as of `at`. Rejected if `at` precedes the
    /// claim's own `valid_time.start` (`IncoherentTemporalWindow`).
    ///
    /// Adjacently-tagged JSON shape (Python-friendly, mirrors `ProvenanceLabel`):
    /// `{"type": "Bound", "value": {"at": "2026-01-01T00:00:00Z"}}`.
    Bound {
        /// The UTC instant at which `target` stops being valid.
        at: chrono::DateTime<chrono::Utc>,
        /// The display precision `at` was supplied at (e.g. `Month` for `"2024-09"`), if
        /// known. `None` for callers that only have a bare instant (raw `assert_validity`
        /// JSON) — honest absence, not a fabricated `Instant`. `#[serde(default)]` so
        /// pre-existing callers that never sent this field keep deserializing (TASK-33-W5-LIB-R2).
        #[serde(default)]
        at_granularity: Option<DateGranularity>,
    },
    /// Reverse the most recent active Bound on `target`, reopening its valid-time window.
    ///
    /// JSON shape: `{"type": "Reopen"}` (unit variant — no `"value"` key).
    Reopen,
}

/// Request for `assert_validity` — the host-facing, oracle-free path to
/// `Superseded`/`Invalidated`/`Reinstated` (SDK_CONTRACT.md §3.1, I11).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssertValidityRequest {
    /// The agent that owns `target` and is submitting this assertion.
    pub agent_id: AgentId,
    /// The claim whose valid-time window is bounded or reopened. Must exist and belong
    /// to `agent_id` — cross-agent targets are rejected as `ClaimNotFound` (I1 scoped lookup).
    pub target: ClaimRef,
    /// Bound or reopen.
    pub assertion: ValidityAssertionInput,
    /// Required. Only `External(*)` (first-hand) is eligible — mirrors the rule that only
    /// first-hand external evidence may overturn a belief. Any other channel is rejected
    /// with `InsufficientProvenanceForOverturn`.
    pub provenance: ProvenanceLabel,
    /// Confidence in this validity assertion.
    pub confidence: Confidence,
}

/// Response from `assert_validity`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AssertValidityResponse {
    /// Echo of `target` — the claim this assertion was applied to.
    pub claim_ref: ClaimRef,
    /// Stable UUID of the appended (or, for a no-op, the pre-existing) validity assertion.
    /// `None` only for a Reopen no-op where no active Bound existed to reverse.
    pub assertion_ref: Option<uuid::Uuid>,
    /// The engine-stamped assertion actually in effect after this call (echoes the
    /// existing Bound on a same-instant no-op, rather than a synthetic re-derivation).
    pub kind: AssertionKind,
    /// The UTC instant the assertion takes effect (`bound_at` for Bound, the reopen
    /// instant for Reopen). `None` for a Reopen no-op (nothing was reopened).
    pub effective_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `Superseded` for Bound, `Reinstated` for Reopen.
    pub disposition: Disposition,
    /// `true` when no new write was made: an idempotent repeat of an identical Bound, a
    /// Reopen with no active Bound to reverse, or a Bound whose `at` is at/after the
    /// claim's own `valid_time.end` (has zero effect on the derived window — see
    /// `assert_validity.rs` gate 5). In the last case `effective_at` honestly reports
    /// `min(at, own_end)` (always `own_end`), not the raw requested `at`.
    pub no_op: bool,
}

/// Outcome of resolving a (subject, predicate) subject-line to the single claim `end_fact`
/// should bound. Computed by the SAME canonical fold `recall`/`query_memory` use — never a
/// heuristic re-derivation (I8 single source of truth).
#[derive(Debug, Clone, PartialEq)]
pub enum LiveClaimResolution {
    /// No live claim on the line.
    Empty,
    /// Exactly one live claim — safe to bound unambiguously.
    Single(ClaimRef),
    /// More than one live claim (Contested, or co-existing SetValued members). `end_fact`
    /// refuses to guess; the count is surfaced in `AmbiguousLineForClose`.
    Ambiguous(usize),
}

// ── QUERY HISTORY ────────────────────────────────────────────────────────────

/// Request to retrieve the full history timeline for a (subject, predicate) subject-line.
///
/// Returns all claims ever written to the line, ordered by the canonical ordering key
/// (valid_time_start when confidence ≥ threshold, else tx_time). Each entry is tagged
/// `Current`, `Superseded`, `Contested`, or `Ended` based on the same canonical fold that
/// powers `query_memory` — see `mempill_types::HistoryEntryStatus` for the full contract, and
/// `query_history.rs` module docs for the single-source-of-truth design (I8).
///
/// AUDIT VIEW — DESIGN DECISION: `query_history` always loads with `as_of_tx_time = None`
/// (every claim ever ingested, regardless of transaction time) and has no `valid_at`
/// parameter. `query_memory`'s bi-temporal `as_of_tx_time`/`valid_at` narrowing is
/// intentionally out of scope for this audit timeline — it exists to show the full claim
/// history, not a point-in-time snapshot.
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
/// `status` is derived from the same canonical fold `query_memory` uses (`is_live`,
/// `has_conflict`, and the narrowed live-selection) — see `mempill_types::HistoryEntryStatus`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    /// Stable reference to the underlying claim (UUID).
    pub claim_ref: ClaimRef,
    /// The asserted value for this claim.
    pub value: serde_json::Value,
    /// Start of the valid-time window, or `None` if unknown.
    pub valid_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Effective end of this entry's valid-time window.
    ///
    /// Derivation rule (see `truth_engine::compute_history_windows`, the single engine-layer
    /// source of truth shared with `query_memory`'s succession selection):
    ///   - This claim's OWN `valid_time.end` is honoured whenever present — it is narrowed
    ///     towards an earlier successor ordering key via `min()`, but NEVER discarded outright
    ///     in favor of a LATER successor key (the historical bug this design fixes).
    ///   - When this claim and its (skip-duplicate) successor are both trusted and their
    ///     windows genuinely OVERLAP, `valid_until` is this claim's own end (not narrowed by
    ///     the overlapping successor) and the entry is flagged `Contested` — never a silent
    ///     fabricated narrowing.
    ///   - When this claim's own end is unknown (`None`) and overlap cannot be determined
    ///     (confidence/start missing on either side), the legacy fallback applies: the
    ///     successor's canonical ordering key closes the window.
    ///   - The last entry (no strictly-later successor) uses its own end, `None` if open-ended.
    pub valid_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Precision of the `valid_from` date, taken verbatim from this claim's own
    /// `ValidTime::start_granularity`. `None` when the start is absent or predates
    /// granularity tracking (legacy row).
    ///
    /// DISPLAY-ONLY — never used for ordering, matching, or fold selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from_granularity: Option<DateGranularity>,
    /// Precision of the `valid_until` date.
    ///
    /// The granularity of whichever timestamp actually produced `valid_until` (see that
    /// field's docs for the full derivation rule):
    ///   - this claim's own `end_granularity`, when its own end was used (the common case now
    ///     that own-end is honoured whenever present);
    ///   - the BOUND's own `bound_at_granularity`, when `valid_until` is bound-derived (an
    ///     active `Bound` narrowed the window). Its source depends on who wrote the bound:
    ///     `end_fact`/`assert_validity` carry the caller's date precision as given (e.g.
    ///     `"2024-09"` → `Month`); an adjudication `Affirm` stamps the winner's
    ///     `valid_time.start` precision; a `Deny` stamps the claim's transaction time and so
    ///     has no precision (`None`);
    ///   - the successor's `start_granularity`, only when the successor's ordering key was
    ///     used AND was itself sourced from `valid_time.start` (not a transaction-time fallback);
    ///   - the successor's `start_granularity` ALSO when a bound-derived `valid_until` carries
    ///     no granularity of its own (legacy assertion, or the `Deny` case above) and its
    ///     instant coincides exactly with that successor key. This fallback is restricted to
    ///     bound-derived ends: a claim's OWN `end_granularity == None` means the writer meant
    ///     instant precision and is never overridden;
    ///   - `None` when the winning value came from a transaction-time fallback (a machine
    ///     timestamp has no user-supplied date precision) or is absent (open-ended / overlap).
    ///
    /// DISPLAY-ONLY — never used for ordering, matching, or fold selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until_granularity: Option<DateGranularity>,
    /// Current / Superseded / Contested / Ended — see `mempill_types::HistoryEntryStatus`.
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

// ── QUERY SUBJECT ─────────────────────────────────────────────────────────────

/// Request to retrieve the resolved belief for every predicate stored under a subject.
///
/// Returns one [`SubjectFactEntry`] per distinct predicate that has at least one claim
/// satisfying the `as_of_tx_time` cutoff.  Each entry is the same fold result that
/// `query_memory` would produce for that `(subject, predicate)` pair under the same
/// `valid_at` / `as_of_tx_time` — the existing fold/disposition logic is reused, not
/// reimplemented.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuerySubjectRequest {
    /// The agent whose memory is queried.
    pub agent_id: AgentId,
    /// The subject for which all predicate beliefs are returned.
    pub subject: String,
    /// Optional: query as of a specific valid-time instant (valid-time axis).
    ///
    /// When set, the fold selects the claim whose valid-time window contains this instant.
    /// When `None`, the backward-compatible behaviour is used (as_of / now drives both axes).
    #[serde(default)]
    pub valid_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional: query as of a specific transaction time (bi-temporal tx-time axis).
    ///
    /// When set, only claims whose `tx_time <= as_of_tx_time` are visible.
    /// When `None`, the full current view is used.
    #[serde(default)]
    pub as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>,
}

/// One per-predicate entry in a [`QuerySubjectResponse`].
///
/// Mirrors the shape that `query_memory` + `enrich_query_memory` would produce for a
/// single `(subject, predicate)` pair.  The field names match the API contract exactly
/// so Python callers can read them as dict keys without additional mapping.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubjectFactEntry {
    /// The predicate this entry describes.
    pub predicate: String,
    /// The resolved value string, or `None` when the status is `NoBelief`.
    pub value: Option<String>,
    /// Resolved belief status: `"Resolved"`, `"Contested"`, `"NoBelief"`, or `"TimingUncertain"`.
    pub status: String,
    /// Start of the valid-time window rendered at recorded precision (e.g. `"2020-03"`).
    /// `None` when the start endpoint is unknown.
    pub valid_from_display: Option<String>,
    /// End of the valid-time window rendered at recorded precision.
    /// `None` when the end endpoint is unknown / open-ended.
    pub valid_until_display: Option<String>,
    /// Human-readable provenance label, or `"none"` when there is no primary belief.
    pub provenance: String,
    /// Stable UUID reference to the primary claim, or `None` when there is no primary.
    pub claim_ref: Option<String>,
    /// Value confidence of the primary claim (0.0–1.0), or `None` when absent.
    pub conf: Option<f32>,
}

/// Response from a `query_subject` call — one entry per distinct predicate.
///
/// Entries are sorted by `predicate` (lexicographic) for stable, deterministic output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuerySubjectResponse {
    /// Per-predicate fold results, sorted by predicate.
    pub entries: Vec<SubjectFactEntry>,
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
