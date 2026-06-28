//! Granularity-aware display-string enrichment for query_memory responses.
//!
//! After the core engine returns a `QueryMemoryResponse`, this module adds
//! two consumer-friendly, pre-rendered display strings to every `BeliefSlot`
//! inside the response dict:
//!
//!   - `valid_from_display`  — start of the valid-time window at its recorded precision.
//!   - `valid_until_display` — end of the valid-time window at its recorded precision.
//!
//! The strings are computed by the canonical Rust helper
//! [`mempill_types::time::format_valid_time_endpoint`] using the `start` /
//! `end` timestamps and `start_granularity` / `end_granularity` fields of
//! `ValidTime`.  Callers receive honest precision:
//!
//!   - Month granularity   → `"2020-03"`   (no fabricated day component)
//!   - Year granularity    → `"2020"`
//!   - Day / Instant       → `"2020-03-15"`
//!   - None granularity    → `"YYYY-MM-DD"` fallback
//!   - Absent endpoint     → field is absent from the dict (not `null`)
//!
//! The enrichment is injected into the response **before** the caller receives
//! the Python dict, so Python/MCP code never needs to import `DateGranularity`
//! or call format helpers — they just read `valid_from_display`.

use mempill_core::application::dto::QueryMemoryResponse;
use mempill_types::time::format_valid_time_endpoint;
use mempill_types::{
    Belief, BeliefStatus, Criticality, CurrencyState, Marker, StalenessFlag,
};

/// A `QueryMemoryResponse` augmented with per-endpoint display strings.
///
/// Produced by [`enrich_query_memory`] and serialised to Python via
/// `pythonize`. The shape is identical to `QueryMemoryResponse` except that
/// each [`EnrichedBelief`] inside `belief` carries two additional optional
/// fields: `valid_from_display` and `valid_until_display`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnrichedQueryMemoryResponse {
    /// Enriched belief projection.
    pub belief: EnrichedBeliefProjection,
}

/// Belief projection with display strings on each slot.
///
/// All fields from `BeliefProjection` are replicated here (except `primary`
/// and `alternatives` which are replaced with their enriched counterparts).
/// This avoids `#[serde(flatten)]` conflicts when both the original and the
/// enriched versions carry the same field names.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnrichedBeliefProjection {
    /// Forwarded from `BeliefProjection::status`.
    pub status: BeliefStatus,
    /// Primary slot with display strings (`null` when original primary is `None`).
    ///
    /// Serialised as `null` (not absent) to preserve backward-compatibility with
    /// existing Python tests that check `belief["primary"] is None`.
    pub primary: Option<EnrichedBelief>,
    /// Alternatives with display strings (mirrors `BeliefProjection::alternatives`).
    pub alternatives: Vec<EnrichedBelief>,
    /// Forwarded from `BeliefProjection::currency`.
    pub currency: CurrencyState,
    /// Forwarded from `BeliefProjection::criticality`.
    pub criticality: Criticality,
    /// Forwarded from `BeliefProjection::staleness`.
    pub staleness: StalenessFlag,
    /// Forwarded from `BeliefProjection::markers`.
    pub markers: Vec<Marker>,
}

/// A single belief candidate with all core fields plus honest display strings.
///
/// All fields from the raw `Belief` are included verbatim via `#[serde(flatten)]`.
/// This preserves the existing dict shape — Python code that reads
/// `belief["primary"]["valid_time"]["start"]` continues to work unchanged;
/// the new `valid_from_display` and `valid_until_display` fields are additive.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnrichedBelief {
    /// All fields from the raw `Belief` (claim_ref, fact, provenance, valid_time,
    /// transaction_time, confidence, currency_signal, criticality).
    #[serde(flatten)]
    pub core: Belief,
    /// Start of the valid-time window rendered at its recorded precision.
    ///
    /// `None` / absent when the start endpoint is unknown (open).
    ///
    /// | `start_granularity` | Example output |
    /// |---------------------|----------------|
    /// | `"year"`            | `"2020"`       |
    /// | `"month"`           | `"2020-03"`    |
    /// | `"day"` or `"instant"` | `"2020-03-15"` |
    /// | absent / legacy     | `"2020-03-15"` (fallback) |
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from_display: Option<String>,
    /// End of the valid-time window rendered at its recorded precision.
    ///
    /// `None` / absent when the end endpoint is unknown or open-ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until_display: Option<String>,
}

// ── Conversion helpers ────────────────────────────────────────────────────────

fn enrich_belief(b: Belief) -> EnrichedBelief {
    let valid_from_display =
        format_valid_time_endpoint(b.valid_time.start, b.valid_time.start_granularity);
    let valid_until_display =
        format_valid_time_endpoint(b.valid_time.end, b.valid_time.end_granularity);
    EnrichedBelief { core: b, valid_from_display, valid_until_display }
}

/// Convert a raw `QueryMemoryResponse` into an enriched form that includes
/// `valid_from_display` and `valid_until_display` on every belief slot.
///
/// The `belief.primary` and all `belief.alternatives` are enriched.
/// Other `BeliefProjection` fields (`status`, `currency`, `criticality`,
/// `staleness`, `markers`) are forwarded unchanged.
pub fn enrich_query_memory(resp: QueryMemoryResponse) -> EnrichedQueryMemoryResponse {
    let primary = resp.belief.primary.map(enrich_belief);
    let alternatives = resp.belief.alternatives.into_iter().map(enrich_belief).collect();

    EnrichedQueryMemoryResponse {
        belief: EnrichedBeliefProjection {
            status: resp.belief.status,
            primary,
            alternatives,
            currency: resp.belief.currency,
            criticality: resp.belief.criticality,
            staleness: resp.belief.staleness,
            markers: resp.belief.markers,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mempill_core::application::dto::QueryMemoryResponse;
    use mempill_types::{
        Belief, BeliefProjection, BeliefStatus, ClaimRef, Confidence, Criticality,
        CurrencySignal, CurrencyState, DateGranularity, ExternalKind, Fact, ProvenanceLabel,
        StalenessFlag, TransactionTime, ValidTime,
    };
    use chrono::{TimeZone, Utc};

    fn make_belief(
        start: Option<chrono::DateTime<Utc>>,
        start_gran: Option<DateGranularity>,
        end: Option<chrono::DateTime<Utc>>,
        end_gran: Option<DateGranularity>,
    ) -> Belief {
        Belief {
            claim_ref: ClaimRef::new_random(),
            fact: Fact {
                subject: "s".into(),
                predicate: "p".into(),
                value: serde_json::json!("v"),
            },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            valid_time: ValidTime {
                start,
                end,
                valid_time_confidence: 0.9,
                start_granularity: start_gran,
                end_granularity: end_gran,
            },
            transaction_time: TransactionTime::now(),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            currency_signal: CurrencySignal {
                last_refreshed_at: TransactionTime::now(),
                state: CurrencyState::Fresh,
                corroboration_count: 1,
            },
            criticality: Criticality::Low,
        }
    }

    fn make_response(belief: Belief) -> QueryMemoryResponse {
        QueryMemoryResponse {
            belief: BeliefProjection {
                status: BeliefStatus::Resolved,
                primary: Some(belief),
                alternatives: vec![],
                currency: CurrencyState::Fresh,
                criticality: Criticality::Low,
                staleness: StalenessFlag { is_stale: false, reason: None },
                markers: vec![],
            },
        }
    }

    /// Month granularity → display = "YYYY-MM" (no day component).
    #[test]
    fn month_granularity_display_no_day() {
        let dt = Utc.with_ymd_and_hms(2020, 3, 1, 0, 0, 0).unwrap();
        let resp = make_response(make_belief(Some(dt), Some(DateGranularity::Month), None, None));
        let enriched = enrich_query_memory(resp);
        let display = enriched.belief.primary.as_ref().unwrap().valid_from_display.as_deref();
        assert_eq!(display, Some("2020-03"), "Month granularity must render as YYYY-MM");
        let s = display.unwrap();
        assert_eq!(s.matches('-').count(), 1, "Month display must have exactly one dash");
    }

    /// Year granularity → display = "YYYY".
    #[test]
    fn year_granularity_display() {
        let dt = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let resp = make_response(make_belief(Some(dt), Some(DateGranularity::Year), None, None));
        let enriched = enrich_query_memory(resp);
        let display = enriched.belief.primary.as_ref().unwrap().valid_from_display.as_deref();
        assert_eq!(display, Some("2020"), "Year granularity must render as YYYY");
        assert!(!display.unwrap().contains('-'), "Year display must not contain dashes");
    }

    /// Day granularity → display = "YYYY-MM-DD".
    #[test]
    fn day_granularity_display() {
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
        let resp = make_response(make_belief(Some(dt), Some(DateGranularity::Day), None, None));
        let enriched = enrich_query_memory(resp);
        let display = enriched.belief.primary.as_ref().unwrap().valid_from_display.as_deref();
        assert_eq!(display, Some("2020-03-15"));
    }

    /// None granularity (legacy row) → display falls back to "YYYY-MM-DD".
    #[test]
    fn none_granularity_falls_back_to_day_form() {
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
        let resp = make_response(make_belief(Some(dt), None, None, None));
        let enriched = enrich_query_memory(resp);
        let display = enriched.belief.primary.as_ref().unwrap().valid_from_display.as_deref();
        assert_eq!(display, Some("2020-03-15"), "None granularity falls back to day form");
    }

    /// Absent start → valid_from_display is None (open/unknown endpoint).
    #[test]
    fn absent_start_display_is_none() {
        let resp = make_response(make_belief(None, None, None, None));
        let enriched = enrich_query_memory(resp);
        assert!(
            enriched.belief.primary.as_ref().unwrap().valid_from_display.is_none(),
            "Absent start endpoint must produce None valid_from_display"
        );
    }

    /// Both start (Month) and end (Year) enriched together.
    #[test]
    fn both_endpoints_enriched() {
        let start = Utc.with_ymd_and_hms(2020, 3, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let resp = make_response(make_belief(
            Some(start), Some(DateGranularity::Month),
            Some(end),  Some(DateGranularity::Year),
        ));
        let enriched = enrich_query_memory(resp);
        let p = enriched.belief.primary.as_ref().unwrap();
        assert_eq!(p.valid_from_display.as_deref(), Some("2020-03"));
        assert_eq!(p.valid_until_display.as_deref(), Some("2023"));
    }

    /// Serialised JSON must contain valid_from_display but not valid_until_display when absent.
    #[test]
    fn serialised_json_contains_display_when_present() {
        let dt = Utc.with_ymd_and_hms(2020, 3, 1, 0, 0, 0).unwrap();
        let resp = make_response(make_belief(Some(dt), Some(DateGranularity::Month), None, None));
        let enriched = enrich_query_memory(resp);
        let json = serde_json::to_string(&enriched).unwrap();
        assert!(json.contains("valid_from_display"), "JSON must contain valid_from_display");
        assert!(json.contains("2020-03"), "JSON must contain the rendered month string");
        assert!(!json.contains("valid_until_display"), "Absent end must not appear in JSON");
    }

    /// `BeliefStatus` from the core projection is forwarded unchanged.
    #[test]
    fn belief_status_forwarded_correctly() {
        let resp = make_response(make_belief(None, None, None, None));
        let enriched = enrich_query_memory(resp);
        assert_eq!(enriched.belief.status, BeliefStatus::Resolved);
    }
}
