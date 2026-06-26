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
use mempill_core::{IngestClaimRequest, MemError, QueryMemoryRequest};
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

// ── RememberOptions (consuming builder) ──────────────────────────────────────

/// Optional overrides for [`remember`]. All fields default to sane values.
///
/// ```rust
/// use mempill::RememberOptions;
/// use mempill::Criticality;
///
/// let opts = RememberOptions::new()
///     .valid_from("2025-01-01")
///     .confidence(0.85)
///     .criticality(Criticality::High);
/// ```
#[derive(Default, Clone, Debug)]
pub struct RememberOptions {
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    /// Value confidence 0.0–1.0. Default: 1.0.
    /// Also drives `valid_time_confidence` when dates are supplied (set once, no duplication).
    pub confidence: Option<f32>,
    pub criticality: Option<Criticality>,
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

/// A candidate value surfaced when the belief is `Contested` or `Conflict`.
#[derive(Debug, Clone)]
pub struct ContestCandidate {
    pub value: serde_json::Value,
    pub claim_ref: ClaimRef,
    pub valid_from: Option<DateTime<Utc>>,
}

/// Flat read result from [`recall`].
///
/// Use the accessor methods rather than matching on `status` directly.
///
/// ```rust,no_run
/// # use mempill::RecallResult;
/// # fn example(r: RecallResult) {
/// if r.is_contested() {
///     for c in &r.candidates { println!("candidate: {:?}", c.value); }
/// } else if r.is_empty() {
///     println!("no memory");
/// } else {
///     println!("{:?}", r.as_str());
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

    let req = IngestClaimRequest::builder(agent_id, subject, predicate, value_json)
        .then_if(opts.valid_from, |b, s| b.valid_from(s))
        .then_if(opts.valid_until, |b, s| b.valid_until(s))
        .then_if(opts.confidence, |b, c| b.confidence(c))
        .then_if(opts.criticality, |b, c| b.criticality(c))
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

    let (value, candidates) = match &bp.status {
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
                })
                .collect();
            (None, cands)
        }
        BeliefStatus::NoBelief | BeliefStatus::TimingUncertain => {
            let value = bp.primary.as_ref().map(|b| b.fact.value.clone());
            (value, vec![])
        }
        BeliefStatus::Resolved => {
            let value = bp.primary.as_ref().map(|b| b.fact.value.clone());
            (value, vec![])
        }
    };

    let currency = bp.currency.clone();
    let is_stale = bp.staleness.is_stale;

    Ok(RecallResult { value, status: bp.status, candidates, currency, is_stale })
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
                ContestCandidate {
                    value: serde_json::json!("Alice"),
                    claim_ref: ClaimRef::new_random(),
                    valid_from: None,
                },
                ContestCandidate {
                    value: serde_json::json!("Bob"),
                    claim_ref: ClaimRef::new_random(),
                    valid_from: None,
                },
            ],
            currency: CurrencyState::Fresh,
            is_stale: false,
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
}

