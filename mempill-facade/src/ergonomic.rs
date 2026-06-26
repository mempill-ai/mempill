//! # Tier-1 ergonomic API — `remember` + `recall`
//!
//! The simplest possible entry point for new users. Zero internal-type imports required.
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use mempill::{open_default_in_memory, remember, recall, RememberOptions};
//! let engine = open_default_in_memory()?;
//! remember(&engine, "agent", "user", "city", "Berlin", RememberOptions::new()).await?;
//! let r = recall(&engine, "agent", "user", "city").await?;
//! println!("{:?}", r.as_str()); // Some("Berlin")
//! # Ok(())
//! # }
//! ```

use chrono::DateTime;
use chrono::Utc;
use mempill_core::{IngestClaimRequest, MemError, QueryHistoryRequest, QueryMemoryRequest};
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, ClaimRef, Confidence, Criticality, Disposition,
    ExternalKind, ProvenanceLabel, ValidTime,
};

use crate::date::parse_lenient_date;

// ── Error type ────────────────────────────────────────────────────────────────

/// Actionable error type for the Tier-1 / Tier-2 ergonomic surface.
///
/// Translates cryptic engine errors (e.g. "premature end of input") into
/// human-readable messages with hints for resolution.
#[derive(Debug, thiserror::Error)]
pub enum MempillDxError {
    /// The supplied date string could not be parsed.
    /// Use YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339.
    #[error("Unparsable date {input:?}: {hint}")]
    UnparsableDate { input: String, hint: &'static str },

    /// `valid_from` must precede `valid_until`.
    #[error("Incoherent date range: start={start} end={end}. {hint}")]
    IncoherentDates { start: String, end: String, hint: &'static str },

    /// Engine-level error (pass-through with original message).
    #[error("Engine error: {0}")]
    Engine(#[from] MemError),
}

// ── Facade traits (object-safe thin seam over EngineHandle) ──────────────────

/// Object-safe async ingest seam. Implemented for `EngineHandle<P,O,V>` via blanket impl.
///
/// Not intended for direct use — call `remember()` instead.
#[async_trait::async_trait]
pub trait CanIngestClaim: Send + Sync {
    async fn ingest_ergo(
        &self,
        req: IngestClaimRequest,
    ) -> Result<mempill_core::IngestClaimResponse, MemError>;
}

/// Object-safe async query seam. Implemented for `EngineHandle<P,O,V>` via blanket impl.
///
/// Not intended for direct use — call `recall()` instead.
#[async_trait::async_trait]
pub trait CanQueryMemory: Send + Sync {
    async fn query_ergo(
        &self,
        req: QueryMemoryRequest,
    ) -> Result<mempill_core::QueryMemoryResponse, MemError>;
}

#[async_trait::async_trait]
impl<P, O, V> CanIngestClaim for mempill_core::EngineHandle<P, O, V>
where
    P: mempill_core::PersistencePort + Send + Sync + 'static,
    O: mempill_core::OraclePort + Send + Sync + 'static,
    V: mempill_core::VectorPort + Send + Sync + 'static,
{
    async fn ingest_ergo(
        &self,
        req: IngestClaimRequest,
    ) -> Result<mempill_core::IngestClaimResponse, MemError> {
        self.ingest_claim(req).await
    }
}

#[async_trait::async_trait]
impl<P, O, V> CanQueryMemory for mempill_core::EngineHandle<P, O, V>
where
    P: mempill_core::PersistencePort + Send + Sync + 'static,
    O: mempill_core::OraclePort + Send + Sync + 'static,
    V: mempill_core::VectorPort + Send + Sync + 'static,
{
    async fn query_ergo(
        &self,
        req: QueryMemoryRequest,
    ) -> Result<mempill_core::QueryMemoryResponse, MemError> {
        self.query_memory(req).await
    }
}

/// Object-safe async history-query seam. Implemented for `EngineHandle<P,O,V>` via blanket impl.
///
/// Not intended for direct use — call `history()` instead.
#[async_trait::async_trait]
pub trait CanQueryHistory: Send + Sync {
    async fn query_history_ergo(
        &self,
        req: QueryHistoryRequest,
    ) -> Result<mempill_core::QueryHistoryResponse, MemError>;
}

#[async_trait::async_trait]
impl<P, O, V> CanQueryHistory for mempill_core::EngineHandle<P, O, V>
where
    P: mempill_core::PersistencePort + Send + Sync + 'static,
    O: mempill_core::OraclePort + Send + Sync + 'static,
    V: mempill_core::VectorPort + Send + Sync + 'static,
{
    async fn query_history_ergo(
        &self,
        req: QueryHistoryRequest,
    ) -> Result<mempill_core::QueryHistoryResponse, MemError> {
        self.query_history(req).await
    }
}

// ── History return type ───────────────────────────────────────────────────────

/// Re-export core's `HistoryEntry` so callers only need `use mempill::HistoryEntry`.
pub use mempill_core::HistoryEntry;

/// Re-export core's `HistoryEntryStatus` so callers can match on `Current`/`Superseded`.
pub use mempill_types::HistoryEntryStatus;

/// Ordered timeline of all claims for a (subject, predicate) subject-line.
///
/// Returned by [`history`]. Entries are ordered oldest-first by the canonical ordering
/// key (valid_time_start when confidence ≥ threshold, else tx_time), exactly matching
/// the sort order of `recall` / `query_memory`.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use mempill::{open_default_in_memory, remember, history, recall, RememberOptions};
///
/// let engine = open_default_in_memory()?;
/// let agent = "my-agent";
///
/// remember(&engine, agent, "city_fact", "name", "Berlin",
///          RememberOptions::new().valid_from("2020-01-01").valid_until("2025-01-01")).await?;
/// remember(&engine, agent, "city_fact", "name", "Munich",
///          RememberOptions::new().valid_from("2025-01-01")).await?;
///
/// let h = history(&engine, agent, "city_fact", "name").await?;
/// assert_eq!(h.entries.len(), 2);
/// assert_eq!(h.current().and_then(|e| e.value.as_str()), Some("Munich"));
/// # Ok(())
/// # }
/// ```
#[must_use]
#[derive(Debug, Clone)]
pub struct History {
    /// All claims for the subject-line, ordered by canonical ordering key (oldest first).
    pub entries: Vec<HistoryEntry>,
}

impl History {
    /// Returns the single `Current` entry, if any.
    ///
    /// The current entry is exactly the claim that [`recall`] would return as primary.
    /// Returns `None` when the subject-line is empty or all claims have been superseded.
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.entries.iter().find(|e| e.status == HistoryEntryStatus::Current)
    }

    /// Returns `true` when the subject-line has no history at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── RememberOptions (consuming builder) ──────────────────────────────────────

/// Optional overrides for [`remember`]. All fields default to sane values.
///
/// ```rust
/// use mempill::{RememberOptions, Criticality};
///
/// let opts = RememberOptions::new()
///     .valid_from("2025-01-01")
///     .confidence(0.85)
///     .criticality(Criticality::High);
///
/// // Lineage (RecallReEntry / model-derived chains):
/// use mempill::ClaimRef;
/// let parent_ref = ClaimRef::new_random();
/// let opts2 = RememberOptions::new().derived_from(vec![parent_ref]);
/// ```
#[derive(Default, Clone, Debug)]
pub struct RememberOptions {
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    /// Value confidence 0.0–1.0. Default: 1.0.
    /// Also drives `valid_time_confidence` when dates are supplied (set once, no duplication).
    pub confidence: Option<f32>,
    pub criticality: Option<Criticality>,
    /// Upstream claims this fact was derived from. Default: empty.
    /// Forwarded directly into `IngestClaimRequest::derived_from`.
    pub derived_from: Vec<ClaimRef>,
}

impl RememberOptions {
    /// Create a `RememberOptions` with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the start of the valid-time window (lenient: YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339).
    pub fn valid_from(mut self, s: impl Into<String>) -> Self {
        self.valid_from = Some(s.into());
        self
    }

    /// Set the end of the valid-time window. `None` = open-ended.
    pub fn valid_until(mut self, s: impl Into<String>) -> Self {
        self.valid_until = Some(s.into());
        self
    }

    /// Value confidence (0.0–1.0). Default: 1.0.
    pub fn confidence(mut self, c: f32) -> Self {
        self.confidence = Some(c);
        self
    }

    /// Criticality class. Default: `Criticality::Medium`.
    pub fn criticality(mut self, c: Criticality) -> Self {
        self.criticality = Some(c);
        self
    }

    /// Upstream claim refs that this fact was derived from.
    ///
    /// Used to express lineage for RecallReEntry and model-derived chains.
    /// Forwarded verbatim into `IngestClaimRequest::derived_from`.
    pub fn derived_from(mut self, refs: Vec<ClaimRef>) -> Self {
        self.derived_from = refs;
        self
    }
}

// ── Return types ──────────────────────────────────────────────────────────────

/// Receipt returned by [`remember`].
#[derive(Debug, Clone)]
pub struct RememberReceipt {
    pub claim_ref: ClaimRef,
    pub disposition: Disposition,
    /// Non-empty only when the write produced a Contested or Conflict disposition.
    pub contested_with: Vec<ClaimRef>,
}

/// Rich detail for a single belief (primary or candidate).
///
/// Available via [`RecallResult::primary`] (the resolved belief) and via
/// [`ContestCandidate::detail`] (each contested candidate). Gives the caller
/// everything needed to build a view without navigating the deep belief path.
#[derive(Debug, Clone)]
pub struct BeliefDetail {
    /// The claim reference (UUID) that backs this belief.
    pub claim_ref: ClaimRef,
    /// The asserted value.
    pub value: serde_json::Value,
    /// Start of the valid-time window, or `None` if open / unknown.
    pub valid_from: Option<DateTime<Utc>>,
    /// End of the valid-time window, or `None` if open-ended.
    pub valid_until: Option<DateTime<Utc>>,
    /// Value confidence (0.0–1.0).
    pub value_confidence: f32,
    /// Human-readable provenance label (e.g. `"External/UserAsserted"`, `"RecallReEntry"`, `"ModelDerived"`).
    pub provenance: String,
    /// Number of independent corroborating sources recorded by the engine.
    pub corroboration_count: u32,
}

/// A candidate value surfaced when the belief is `Contested` or `Conflict`.
#[derive(Debug, Clone)]
pub struct ContestCandidate {
    pub value: serde_json::Value,
    pub claim_ref: ClaimRef,
    pub valid_from: Option<DateTime<Utc>>,
    /// Full detail for this candidate — same fields as the primary [`BeliefDetail`].
    pub detail: BeliefDetail,
}

/// Flat read result from [`recall`].
///
/// Use the accessor methods rather than matching on `status` directly.
///
/// ```rust,no_run
/// # use mempill::RecallResult;
/// # fn example(r: RecallResult) {
/// if r.is_contested() {
///     for c in &r.candidates {
///         println!("candidate: {:?} conf={}", c.value, c.detail.value_confidence);
///     }
/// } else if r.is_empty() {
///     println!("no memory");
/// } else {
///     println!("{:?}", r.as_str());
///     if let Some(p) = &r.primary {
///         println!("provenance={} corroboration={}", p.provenance, p.corroboration_count);
///     }
/// }
/// # }
/// ```
#[must_use]
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// The resolved value, or `None` when `Contested`, `NoBelief`, or `TimingUncertain`.
    /// Never rely on `value.is_none()` to detect Contested — use `is_contested()`.
    pub value: Option<serde_json::Value>,
    /// Semantic status — always check before using `value`.
    pub status: BeliefStatus,
    /// Populated when `status` is `Contested` or `Conflict` — both candidates surfaced.
    pub candidates: Vec<ContestCandidate>,
    /// Computed currency / decay state.
    pub currency: mempill_types::CurrencyState,
    /// True when the claim is past the currency decay threshold.
    pub is_stale: bool,
    /// Full detail for the resolved primary belief.
    ///
    /// `None` when status is `NoBelief`. For `Contested`/`Conflict`, the primary
    /// belief is not resolved — read `candidates` instead and use `candidate.detail`.
    pub primary: Option<BeliefDetail>,
}

impl RecallResult {
    /// The value as a `&str`, or `None`. Convenience for the 95% case.
    pub fn as_str(&self) -> Option<&str> {
        self.value.as_ref().and_then(|v| v.as_str())
    }

    /// True when the status is `Contested` or `Conflict`.
    ///
    /// When this returns `true`, `value` is `None` — use `candidates` to read both values.
    pub fn is_contested(&self) -> bool {
        matches!(self.status, BeliefStatus::Contested | BeliefStatus::Conflict)
    }

    /// True when the engine has no live claim for this subject+predicate.
    pub fn is_empty(&self) -> bool {
        matches!(self.status, BeliefStatus::NoBelief)
    }
}

// ── Tier-2 builder for IngestClaimRequest ────────────────────────────────────

/// Tier-2 builder for [`IngestClaimRequest`].
///
/// Applies the same defaults as [`remember`] but exposes all optional overrides
/// including `cardinality` and `provenance`. Uses lenient date parsing.
///
/// ```rust,no_run
/// use mempill::{IngestClaimRequest, IngestClaimRequestExt, Cardinality, Criticality};
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let req = IngestClaimRequest::builder("agent", "user", "city", serde_json::json!("Berlin"))
///     .valid_from("2025")
///     .confidence(0.9)
///     .cardinality(Cardinality::Functional)
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct IngestClaimRequestBuilder {
    agent_id: AgentId,
    subject: String,
    predicate: String,
    value: serde_json::Value,
    valid_from: Option<String>,
    valid_until: Option<String>,
    confidence: Option<f32>,
    cardinality: Option<Cardinality>,
    provenance: Option<ProvenanceLabel>,
    criticality: Option<Criticality>,
    derived_from: Vec<ClaimRef>,
}

impl IngestClaimRequestBuilder {
    pub fn valid_from(mut self, s: impl Into<String>) -> Self {
        self.valid_from = Some(s.into());
        self
    }

    pub fn valid_until(mut self, s: impl Into<String>) -> Self {
        self.valid_until = Some(s.into());
        self
    }

    pub fn confidence(mut self, c: f32) -> Self {
        self.confidence = Some(c);
        self
    }

    pub fn cardinality(mut self, c: Cardinality) -> Self {
        self.cardinality = Some(c);
        self
    }

    pub fn provenance(mut self, p: ProvenanceLabel) -> Self {
        self.provenance = Some(p);
        self
    }

    pub fn criticality(mut self, c: Criticality) -> Self {
        self.criticality = Some(c);
        self
    }

    pub fn derived_from(mut self, refs: Vec<ClaimRef>) -> Self {
        self.derived_from = refs;
        self
    }

    /// Finalise the request, applying defaults and normalising dates.
    ///
    /// # Errors
    /// Returns `MempillDxError::UnparsableDate` if any date string is invalid.
    /// Returns `MempillDxError::IncoherentDates` if `valid_from` >= `valid_until`.
    pub fn build(self) -> Result<IngestClaimRequest, MempillDxError> {
        let value_confidence = self.confidence.unwrap_or(1.0);
        let has_dates = self.valid_from.is_some() || self.valid_until.is_some();
        let vtc = if has_dates { value_confidence } else { 0.0 };

        let start = self.valid_from.as_deref().map(parse_lenient_date).transpose()?;
        let end = self.valid_until.as_deref().map(parse_lenient_date).transpose()?;

        if let (Some(s), Some(e)) = (start, end) {
            if s >= e {
                return Err(MempillDxError::IncoherentDates {
                    start: s.to_rfc3339(),
                    end: e.to_rfc3339(),
                    hint: "valid_from must precede valid_until",
                });
            }
        }

        let valid_time = if has_dates {
            Some(ValidTime { start, end, valid_time_confidence: vtc })
        } else {
            None
        };

        Ok(IngestClaimRequest {
            agent_id: self.agent_id,
            subject: self.subject,
            predicate: self.predicate,
            value: self.value,
            provenance: self.provenance.unwrap_or(ProvenanceLabel::External(ExternalKind::UserAsserted)),
            cardinality: self.cardinality.unwrap_or(Cardinality::Functional),
            valid_time,
            confidence: Confidence { value_confidence, valid_time_confidence: vtc },
            criticality: self.criticality.unwrap_or(Criticality::Medium),
            derived_from: self.derived_from,
        })
    }
}

/// Extension trait that adds `.builder()` to `IngestClaimRequest`.
pub trait IngestClaimRequestExt {
    /// Create a Tier-2 builder with required fields and sane defaults.
    fn builder(
        agent_id: impl Into<String>,
        subject: impl Into<String>,
        predicate: impl Into<String>,
        value: serde_json::Value,
    ) -> IngestClaimRequestBuilder;
}

impl IngestClaimRequestExt for IngestClaimRequest {
    fn builder(
        agent_id: impl Into<String>,
        subject: impl Into<String>,
        predicate: impl Into<String>,
        value: serde_json::Value,
    ) -> IngestClaimRequestBuilder {
        IngestClaimRequestBuilder {
            agent_id: AgentId(agent_id.into()),
            subject: subject.into(),
            predicate: predicate.into(),
            value,
            valid_from: None,
            valid_until: None,
            confidence: None,
            cardinality: None,
            provenance: None,
            criticality: None,
            derived_from: vec![],
        }
    }
}

// ── Tier-1 functions ──────────────────────────────────────────────────────────

/// Remember a fact about a subject. The minimal write path.
///
/// # Defaults (when not overridden in `opts`)
///
/// | Field | Default |
/// |-------|---------|
/// | provenance | `External(UserAsserted)` |
/// | cardinality | `Functional` |
/// | value_confidence | `1.0` |
/// | valid_time_confidence | `0.0` (no dates) / `confidence` (dates supplied) |
/// | valid_time | `None` (open / unknown) |
/// | criticality | `Medium` |
/// | derived_from | `[]` |
///
/// # Errors
/// - `MempillDxError::UnparsableDate` — if a date string is malformed
/// - `MempillDxError::IncoherentDates` — if `valid_from >= valid_until`
/// - `MempillDxError::Engine(_)` — engine-level rejection (e.g. write lock contention)
pub async fn remember(
    engine: &impl CanIngestClaim,
    agent_id: impl Into<String>,
    subject: impl Into<String>,
    predicate: impl Into<String>,
    value: impl serde::Serialize,
    opts: RememberOptions,
) -> Result<RememberReceipt, MempillDxError> {
    let value_json = serde_json::to_value(value)
        .map_err(|e| MempillDxError::Engine(MemError::MalformedFact { reason: e.to_string() }))?;

    let derived = opts.derived_from.clone();
    let req = IngestClaimRequest::builder(agent_id, subject, predicate, value_json)
        .then_if(opts.valid_from, |b, s| b.valid_from(s))
        .then_if(opts.valid_until, |b, s| b.valid_until(s))
        .then_if(opts.confidence, |b, c| b.confidence(c))
        .then_if(opts.criticality, |b, c| b.criticality(c))
        .derived_from(derived)
        .build()?;

    let resp = engine.ingest_ergo(req).await?;

    Ok(RememberReceipt {
        claim_ref: resp.claim_ref,
        disposition: resp.disposition,
        contested_with: resp.contested_with,
    })
}

/// Recall the current belief for a subject+predicate.
///
/// Returns a flat [`RecallResult`] — no 4-level path required.
///
/// # Errors
/// - `MempillDxError::Engine(_)` — persistence failure
pub async fn recall(
    engine: &impl CanQueryMemory,
    agent_id: impl Into<String>,
    subject: impl Into<String>,
    predicate: impl Into<String>,
) -> Result<RecallResult, MempillDxError> {
    let req = QueryMemoryRequest {
        agent_id: AgentId(agent_id.into()),
        subject: subject.into(),
        predicate: predicate.into(),
        as_of_tx_time: None,
    };

    let resp = engine.query_ergo(req).await?;
    let bp = resp.belief;

    // ── Helper: build a BeliefDetail from a raw Belief ───────────────────────
    let make_detail = |b: &mempill_types::Belief| -> BeliefDetail {
        BeliefDetail {
            claim_ref: b.claim_ref.clone(),
            value: b.fact.value.clone(),
            valid_from: b.valid_time.start,
            valid_until: b.valid_time.end,
            value_confidence: b.confidence.value_confidence,
            provenance: provenance_label_str(&b.provenance),
            corroboration_count: b.currency_signal.corroboration_count,
        }
    };

    let (value, candidates, primary) = match &bp.status {
        BeliefStatus::Contested | BeliefStatus::Conflict => {
            // Structurally impossible to misread as NoBelief:
            // value = None, candidates populated with all surfaced beliefs.
            let cands = std::iter::once(bp.primary.as_ref())
                .flatten()
                .chain(bp.alternatives.iter())
                .map(|b| ContestCandidate {
                    value: b.fact.value.clone(),
                    claim_ref: b.claim_ref.clone(),
                    valid_from: b.valid_time.start,
                    detail: make_detail(b),
                })
                .collect();
            (None, cands, None)
        }
        BeliefStatus::NoBelief => {
            (None, vec![], None)
        }
        BeliefStatus::TimingUncertain | BeliefStatus::Resolved => {
            let value = bp.primary.as_ref().map(|b| b.fact.value.clone());
            let detail = bp.primary.as_ref().map(|b| make_detail(b));
            (value, vec![], detail)
        }
    };

    let currency = bp.currency.clone();
    let is_stale = bp.staleness.is_stale;

    Ok(RecallResult { value, status: bp.status, candidates, currency, is_stale, primary })
}

/// Retrieve the full ordered history timeline for a subject+predicate.
///
/// Returns a [`History`] containing all claims ever written to the (subject, predicate)
/// subject-line, ordered oldest-first. Each entry is tagged [`HistoryEntryStatus::Current`]
/// or [`HistoryEntryStatus::Superseded`] using the same canonical fold as [`recall`], so
/// `history().current()` is guaranteed to agree with `recall().primary`.
///
/// # Errors
/// - `MempillDxError::Engine(_)` — persistence failure
pub async fn history(
    engine: &impl CanQueryHistory,
    agent_id: impl Into<String>,
    subject: impl Into<String>,
    predicate: impl Into<String>,
) -> Result<History, MempillDxError> {
    let req = QueryHistoryRequest {
        agent_id: AgentId(agent_id.into()),
        subject: subject.into(),
        predicate: predicate.into(),
    };

    let resp = engine.query_history_ergo(req).await?;

    Ok(History { entries: resp.entries })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn provenance_label_str(p: &ProvenanceLabel) -> String {
    match p {
        ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted) => {
            "External/UserAsserted".to_owned()
        }
        ProvenanceLabel::External(mempill_types::ExternalKind::ExternalFirstHand) => {
            "External/ExternalFirstHand".to_owned()
        }
        ProvenanceLabel::RecallReEntry => "RecallReEntry".to_owned(),
        ProvenanceLabel::ModelDerived => "ModelDerived".to_owned(),
        // ProvenanceLabel is #[non_exhaustive] — forward-compatible fallback.
        _ => format!("{p:?}"),
    }
}

// ── Builder helper (avoids Option::map chaining) ──────────────────────────────

trait BuilderExt: Sized {
    fn then_if<T, F>(self, opt: Option<T>, f: F) -> Self
    where
        F: FnOnce(Self, T) -> Self;
}

impl BuilderExt for IngestClaimRequestBuilder {
    fn then_if<T, F>(self, opt: Option<T>, f: F) -> Self
    where
        F: FnOnce(Self, T) -> Self,
    {
        match opt {
            Some(v) => f(self, v),
            None => self,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use mempill_types::{CurrencyState};

    // ── Defaults test ─────────────────────────────────────────────────────────

    #[test]
    fn builder_defaults_applied() {
        let req = IngestClaimRequest::builder("agent", "user", "city", serde_json::json!("Berlin"))
            .build()
            .unwrap();
        assert_eq!(req.provenance, ProvenanceLabel::External(ExternalKind::UserAsserted));
        assert_eq!(req.cardinality, Cardinality::Functional);
        assert_eq!(req.confidence.value_confidence, 1.0);
        assert_eq!(req.confidence.valid_time_confidence, 0.0); // no dates
        assert!(req.valid_time.is_none());
        assert_eq!(req.criticality, Criticality::Medium);
        assert!(req.derived_from.is_empty());
    }

    #[test]
    fn builder_confidence_drives_valid_time_confidence_when_dates_supplied() {
        let req = IngestClaimRequest::builder("agent", "user", "city", serde_json::json!("X"))
            .valid_from("2025-01-01")
            .confidence(0.8)
            .build()
            .unwrap();
        assert_eq!(req.confidence.value_confidence, 0.8);
        assert_eq!(req.confidence.valid_time_confidence, 0.8); // mirrors value_confidence
        let vt = req.valid_time.unwrap();
        assert_eq!(vt.valid_time_confidence, 0.8);
    }

    #[test]
    fn builder_incoherent_dates_rejected() {
        let err = IngestClaimRequest::builder("a", "s", "p", serde_json::json!(1))
            .valid_from("2025-06-01")
            .valid_until("2025-01-01")
            .build()
            .unwrap_err();
        assert!(matches!(err, MempillDxError::IncoherentDates { .. }));
    }

    // ── Lenient date forms ────────────────────────────────────────────────────

    #[test]
    fn builder_lenient_year_only() {
        let req = IngestClaimRequest::builder("a", "s", "p", serde_json::json!(1))
            .valid_from("2026")
            .build()
            .unwrap();
        let vt = req.valid_time.unwrap();
        assert_eq!(vt.start.unwrap().year(), 2026);
    }

    #[test]
    fn builder_bad_date_gives_clear_error() {
        let err = IngestClaimRequest::builder("a", "s", "p", serde_json::json!(1))
            .valid_from("not-a-date")
            .build()
            .unwrap_err();
        match err {
            MempillDxError::UnparsableDate { input, hint } => {
                assert_eq!(input, "not-a-date");
                assert!(hint.contains("YYYY"));
                assert!(!hint.contains("premature end of input"));
            }
            other => panic!("expected UnparsableDate, got {:?}", other),
        }
    }

    // ── RecallResult helper methods ───────────────────────────────────────────

    fn make_recall_result(status: BeliefStatus, value: Option<serde_json::Value>) -> RecallResult {
        RecallResult {
            value,
            status,
            candidates: vec![],
            currency: CurrencyState::Fresh,
            is_stale: false,
            primary: None,
        }
    }

    fn make_belief_detail(value: serde_json::Value) -> BeliefDetail {
        BeliefDetail {
            claim_ref: ClaimRef::new_random(),
            value,
            valid_from: None,
            valid_until: None,
            value_confidence: 1.0,
            provenance: "External/UserAsserted".to_owned(),
            corroboration_count: 0,
        }
    }

    fn make_contest_candidate(value: serde_json::Value) -> ContestCandidate {
        let detail = make_belief_detail(value.clone());
        ContestCandidate {
            value,
            claim_ref: detail.claim_ref.clone(),
            valid_from: None,
            detail,
        }
    }

    #[test]
    fn recall_result_as_str_resolved() {
        let r = make_recall_result(BeliefStatus::Resolved, Some(serde_json::json!("Berlin")));
        assert_eq!(r.as_str(), Some("Berlin"));
        assert!(!r.is_contested());
        assert!(!r.is_empty());
    }

    #[test]
    fn recall_result_contested_is_none_value() {
        let r = RecallResult {
            value: None, // structurally enforced
            status: BeliefStatus::Contested,
            candidates: vec![
                make_contest_candidate(serde_json::json!("Alice")),
                make_contest_candidate(serde_json::json!("Bob")),
            ],
            currency: CurrencyState::Fresh,
            is_stale: false,
            primary: None,
        };
        assert!(r.is_contested());
        assert!(r.value.is_none(), "Contested must have value=None");
        assert_eq!(r.candidates.len(), 2);
        assert!(!r.is_empty(), "Contested must NOT be is_empty()");
    }

    #[test]
    fn recall_result_no_belief_is_empty() {
        let r = make_recall_result(BeliefStatus::NoBelief, None);
        assert!(r.is_empty());
        assert!(!r.is_contested());
    }

    #[test]
    fn recall_result_conflict_is_contested() {
        let r = make_recall_result(BeliefStatus::Conflict, None);
        assert!(r.is_contested());
    }

    #[test]
    fn remember_options_derived_from_builder() {
        let ref1 = ClaimRef::new_random();
        let ref2 = ClaimRef::new_random();
        let opts = RememberOptions::new().derived_from(vec![ref1.clone(), ref2.clone()]);
        assert_eq!(opts.derived_from.len(), 2);
        assert_eq!(opts.derived_from[0], ref1);
    }

    #[test]
    fn remember_options_derived_from_default_empty() {
        let opts = RememberOptions::new();
        assert!(opts.derived_from.is_empty());
    }

    #[test]
    fn belief_detail_fields_accessible() {
        let detail = make_belief_detail(serde_json::json!("Berlin"));
        assert_eq!(detail.value, serde_json::json!("Berlin"));
        assert_eq!(detail.value_confidence, 1.0);
        assert_eq!(detail.provenance, "External/UserAsserted");
        assert_eq!(detail.corroboration_count, 0);
        assert!(detail.valid_from.is_none());
        assert!(detail.valid_until.is_none());
    }

    #[test]
    fn contest_candidate_detail_field_accessible() {
        let c = make_contest_candidate(serde_json::json!("Alice"));
        assert_eq!(c.detail.value, serde_json::json!("Alice"));
        assert_eq!(c.detail.provenance, "External/UserAsserted");
    }

    #[test]
    fn recall_result_primary_field_present_on_resolved() {
        let detail = make_belief_detail(serde_json::json!("Berlin"));
        let r = RecallResult {
            value: Some(serde_json::json!("Berlin")),
            status: BeliefStatus::Resolved,
            candidates: vec![],
            currency: CurrencyState::Fresh,
            is_stale: false,
            primary: Some(detail),
        };
        let p = r.primary.as_ref().unwrap();
        assert_eq!(p.value, serde_json::json!("Berlin"));
        assert_eq!(p.provenance, "External/UserAsserted");
    }

    // ── Round-trip via SQLite (integration-ish, requires tokio runtime) ───────

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn remember_recall_round_trip() {
        let engine = crate::open_default_in_memory().unwrap();
        let receipt = remember(
            &engine,
            "test-agent",
            "user",
            "city",
            serde_json::json!("Berlin"),
            RememberOptions::new(),
        )
        .await
        .unwrap();
        assert!(!format!("{:?}", receipt.disposition).is_empty());

        let result = recall(&engine, "test-agent", "user", "city").await.unwrap();
        assert!(!result.is_empty());
        assert!(!result.is_contested());
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn recall_contested_value_is_none_candidates_populated() {
        let engine = crate::open_default_in_memory().unwrap();
        remember(&engine, "agent", "acme", "ceo", serde_json::json!("Alice"), RememberOptions::new())
            .await
            .unwrap();
        remember(&engine, "agent", "acme", "ceo", serde_json::json!("Bob"), RememberOptions::new())
            .await
            .unwrap();

        let r = recall(&engine, "agent", "acme", "ceo").await.unwrap();
        assert!(r.is_contested(), "expected Contested, got {:?}", r.status);
        assert!(r.value.is_none(), "Contested must have value=None");
        assert_eq!(r.candidates.len(), 2, "both Alice and Bob must surface");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn remember_with_valid_from_defaults_correct() {
        let engine = crate::open_default_in_memory().unwrap();
        let opts = RememberOptions::new().valid_from("2025-01-01").confidence(0.9);
        remember(&engine, "agent", "user", "city", "Munich", opts).await.unwrap();
        let r = recall(&engine, "agent", "user", "city").await.unwrap();
        assert!(!r.is_empty());
    }

    // ── Gap 1: derived_from forwarded via remember() ──────────────────────────

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn remember_derived_from_forwarded() {
        let engine = crate::open_default_in_memory().unwrap();
        // First, remember a "source" fact whose claim_ref we will reference.
        let source = remember(
            &engine,
            "agent",
            "user",
            "city",
            "Berlin",
            RememberOptions::new(),
        )
        .await
        .unwrap();

        // Now remember a derived fact, referencing the source claim_ref.
        let derived = remember(
            &engine,
            "agent",
            "user",
            "city_note",
            "Capital of Germany",
            RememberOptions::new().derived_from(vec![source.claim_ref.clone()]),
        )
        .await
        .unwrap();

        // The engine accepted the request (claim_ref issued means it didn't reject it).
        assert!(!format!("{:?}", derived.claim_ref).is_empty());
        // Verify we can still recall the derived fact.
        let r = recall(&engine, "agent", "user", "city_note").await.unwrap();
        assert!(!r.is_empty());
    }

    // ── Gap 2: RecallResult.primary exposes rich fields ───────────────────────

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn recall_primary_exposes_rich_fields() {
        let engine = crate::open_default_in_memory().unwrap();
        let opts = RememberOptions::new()
            .valid_from("2025-01-01")
            .valid_until("2026-01-01")
            .confidence(0.9);
        let receipt = remember(&engine, "agent", "user", "city", "Berlin", opts).await.unwrap();

        let r = recall(&engine, "agent", "user", "city").await.unwrap();
        let p = r.primary.as_ref().expect("primary must be set for a non-empty resolved belief");

        assert_eq!(p.claim_ref, receipt.claim_ref, "claim_ref must match the stored claim");
        assert_eq!(p.value, serde_json::json!("Berlin"));
        assert!(p.valid_from.is_some(), "valid_from must be populated");
        assert!(p.valid_until.is_some(), "valid_until must be populated");
        assert!((p.value_confidence - 0.9).abs() < 1e-4, "value_confidence must be 0.9");
        assert!(p.provenance.contains("UserAsserted"), "provenance must contain UserAsserted");
        // corroboration_count is 0 for a single fresh write (no independent corroboration).
        assert_eq!(p.corroboration_count, 0);
    }

    // ── history() integration tests ───────────────────────────────────────────

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn history_succession_ordered_with_correct_statuses() {
        let engine = crate::open_default_in_memory().unwrap();

        // First fact: Berlin valid [2020, 2025)
        remember(
            &engine,
            "agent",
            "user",
            "city",
            "Berlin",
            RememberOptions::new().valid_from("2020-01-01").valid_until("2025-01-01"),
        )
        .await
        .unwrap();

        // Superseding fact: Munich valid [2025, ∞) — this is what recall() returns now
        remember(
            &engine,
            "agent",
            "user",
            "city",
            "Munich",
            RememberOptions::new().valid_from("2025-01-01"),
        )
        .await
        .unwrap();

        let h = history(&engine, "agent", "user", "city").await.unwrap();

        assert_eq!(h.entries.len(), 2, "two claims → two history entries");
        assert!(!h.is_empty());

        // Oldest-first order
        assert_eq!(
            h.entries[0].value,
            serde_json::json!("Berlin"),
            "Berlin must be first (oldest)"
        );
        assert_eq!(
            h.entries[1].value,
            serde_json::json!("Munich"),
            "Munich must be second (newer)"
        );

        // Status correctness
        assert_eq!(
            h.entries[0].status,
            mempill_types::HistoryEntryStatus::Superseded,
            "Berlin must be Superseded"
        );
        assert_eq!(
            h.entries[1].status,
            mempill_types::HistoryEntryStatus::Current,
            "Munich must be Current"
        );

        // history().current() agrees with recall().primary value
        let current = h.current().expect("must have a Current entry");
        assert_eq!(current.value, serde_json::json!("Munich"));

        let r = recall(&engine, "agent", "user", "city").await.unwrap();
        let primary_value = r.primary.as_ref().map(|p| &p.value);
        assert_eq!(
            primary_value,
            Some(&serde_json::json!("Munich")),
            "history().current() value must match recall().primary value"
        );
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn history_empty_for_unknown_predicate() {
        let engine = crate::open_default_in_memory().unwrap();
        let h = history(&engine, "agent", "nobody", "nothing").await.unwrap();
        assert!(h.is_empty());
        assert!(h.current().is_none());
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn recall_contested_candidates_expose_rich_fields() {
        let engine = crate::open_default_in_memory().unwrap();
        remember(&engine, "agent", "acme", "ceo", serde_json::json!("Alice"), RememberOptions::new())
            .await
            .unwrap();
        remember(&engine, "agent", "acme", "ceo", serde_json::json!("Bob"), RememberOptions::new())
            .await
            .unwrap();

        let r = recall(&engine, "agent", "acme", "ceo").await.unwrap();
        assert!(r.is_contested());
        assert!(r.primary.is_none(), "Contested belief must not have primary set");
        assert_eq!(r.candidates.len(), 2);

        for c in &r.candidates {
            // Each candidate must have a valid claim_ref and provenance.
            assert!(!format!("{:?}", c.detail.claim_ref).is_empty());
            assert!(!c.detail.provenance.is_empty());
            // Values must match the ones we stored.
            assert!(
                c.value == serde_json::json!("Alice") || c.value == serde_json::json!("Bob"),
                "unexpected candidate value: {:?}", c.value
            );
            // detail.value mirrors the top-level value.
            assert_eq!(c.detail.value, c.value);
        }
    }
}

