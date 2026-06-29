//! Temporal types: bi-temporal model support.

/// Granularity of a valid-time date that was extracted from a partial date string.
///
/// When a host supplies a date like `"2024"` or `"2024-05"`, the engine normalises it to a
/// `DateTime<Utc>` start-of-period but records the original precision here so that callers can
/// render dates honestly (e.g. display `"2024"` instead of `"2024-01-01T00:00:00Z"`).
///
/// Additive field — existing `ValidTime` values without this field deserialise with `None`
/// (see `#[serde(default)]` on [`ValidTime::start_granularity`] and [`ValidTime::end_granularity`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateGranularity {
    /// The date was given as a four-digit year, e.g. `"2024"`.
    Year,
    /// The date was given as year-month, e.g. `"2024-05"`.
    Month,
    /// The date was given as a full calendar date, e.g. `"2024-05-15"`.
    Day,
    /// The date was given as a full instant (date + time), e.g. an RFC-3339 string.
    Instant,
}

/// Transaction-time stamp: machine-assigned, monotone, reliable. Engine-assigned; host cannot supply this as truth.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TransactionTime(pub chrono::DateTime<chrono::Utc>);

impl TransactionTime {
    /// Stamp the current UTC instant.
    pub fn now() -> Self {
        Self(chrono::Utc::now())
    }
}

/// Valid-time interval — fallible and host-extracted (confidence-tagged).
/// When start/end are None, belief ordering falls back to TransactionTime.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ValidTime {
    /// Start of the valid-time window (`None` = unknown / open-ended).
    pub start: Option<chrono::DateTime<chrono::Utc>>,
    /// End of the valid-time window (`None` = unknown / open-ended).
    pub end: Option<chrono::DateTime<chrono::Utc>>,
    /// Confidence in the valid-time extraction itself (mirrors Confidence.valid_time_confidence).
    pub valid_time_confidence: f32,
    /// Optional precision hint for the `start` field when it was derived from a partial date
    /// string (e.g. `"2024"` → `Year`, `"2024-05"` → `Month`).  `None` means the start was
    /// either absent or already a full instant.
    ///
    /// Additive field: old serialised `ValidTime` values without this key deserialise to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_granularity: Option<DateGranularity>,
    /// Optional precision hint for the `end` field when it was derived from a partial date string.
    /// `None` means the end was either absent (open-ended) or already a full instant.
    ///
    /// Additive field: old serialised `ValidTime` values without this key deserialise to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_granularity: Option<DateGranularity>,
}

/// Convert a [`DateGranularity`] to its canonical TEXT representation used in both
/// the SQLite and Postgres persistence adapters.
///
/// The strings are identical to the serde snake_case serialisation so that TEXT stored in
/// the database matches JSON round-trips: `"year"`, `"month"`, `"day"`, `"instant"`.
///
/// Both adapters MUST use this function (not hand-rolled `match` arms) to guarantee
/// cross-adapter encoding consistency.
pub fn date_granularity_to_str(g: DateGranularity) -> &'static str {
    match g {
        DateGranularity::Year => "year",
        DateGranularity::Month => "month",
        DateGranularity::Day => "day",
        DateGranularity::Instant => "instant",
    }
}

/// Parse the TEXT column value back to [`DateGranularity`].
///
/// Returns `None` for `NULL` (old rows without granularity) and errors on unknown strings.
/// Both adapters MUST use this function for decode symmetry with [`date_granularity_to_str`].
pub fn str_to_date_granularity(s: &str) -> Option<DateGranularity> {
    match s {
        "year" => Some(DateGranularity::Year),
        "month" => Some(DateGranularity::Month),
        "day" => Some(DateGranularity::Day),
        "instant" => Some(DateGranularity::Instant),
        _ => None,
    }
}

/// Parse a lenient date string into a UTC `DateTime` start-of-period and its [`DateGranularity`].
///
/// Accepted formats (in order):
/// - `"YYYY"`         → 1 January of that year, midnight UTC  → [`DateGranularity::Year`]
/// - `"YYYY-MM"`      → 1st of that month, midnight UTC       → [`DateGranularity::Month`]
/// - `"YYYY-MM-DD"`   → that calendar day, midnight UTC       → [`DateGranularity::Day`]
/// - Any RFC-3339 / ISO-8601 string with time component       → [`DateGranularity::Instant`]
///
/// Returns `None` if the input does not match any recognised format.
///
/// # Examples
/// ```
/// use mempill_types::time::parse_valid_time_date;
/// use mempill_types::time::DateGranularity;
/// let (dt, gran) = parse_valid_time_date("2024").unwrap();
/// assert_eq!(gran, DateGranularity::Year);
/// assert_eq!(dt.to_rfc3339(), "2024-01-01T00:00:00+00:00");
/// ```
pub fn parse_valid_time_date(
    input: &str,
) -> Option<(chrono::DateTime<chrono::Utc>, DateGranularity)> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};

    let s = input.trim();

    // Try RFC-3339 / full instant first (most specific).
    if let Ok(dt) = s.parse::<chrono::DateTime<chrono::Utc>>() {
        return Some((dt, DateGranularity::Instant));
    }

    // YYYY-MM-DD
    if s.len() == 10 && s.chars().nth(4) == Some('-') && s.chars().nth(7) == Some('-') {
        if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            let ndt = NaiveDateTime::new(nd, chrono::NaiveTime::from_hms_opt(0, 0, 0)?);
            return Some((Utc.from_utc_datetime(&ndt), DateGranularity::Day));
        }
    }

    // YYYY-MM
    if s.len() == 7 && s.chars().nth(4) == Some('-') {
        let padded = format!("{s}-01");
        if let Ok(nd) = NaiveDate::parse_from_str(&padded, "%Y-%m-%d") {
            let ndt = NaiveDateTime::new(nd, chrono::NaiveTime::from_hms_opt(0, 0, 0)?);
            return Some((Utc.from_utc_datetime(&ndt), DateGranularity::Month));
        }
    }

    // YYYY
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(year) = s.parse::<i32>() {
            let nd = NaiveDate::from_ymd_opt(year, 1, 1)?;
            let ndt = NaiveDateTime::new(nd, chrono::NaiveTime::from_hms_opt(0, 0, 0)?);
            return Some((Utc.from_utc_datetime(&ndt), DateGranularity::Year));
        }
    }

    None
}

/// Render a valid-time endpoint honestly at its recorded precision.
///
/// The granularity governs how many date components are included in the output.
/// The hard rule: output MUST NOT fabricate finer precision than `granularity`.
///
/// | Granularity       | Example output        |
/// |-------------------|-----------------------|
/// | `Year`            | `"2020"`              |
/// | `Month`           | `"2020-03"`           |
/// | `Day`             | `"2020-03-15"`        |
/// | `Instant`         | `"2020-03-15"` (day form; sub-day precision is not shown by default) |
/// | `None` (unknown)  | falls back to `"YYYY-MM-DD"` day form, or `None` when `date` is also `None` |
///
/// Returns `None` when `date` is `None` (open / unknown endpoint).
///
/// # Example
///
/// ```
/// use chrono::{TimeZone, Utc};
/// use mempill_types::time::{DateGranularity, format_valid_time_endpoint};
///
/// let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
/// assert_eq!(format_valid_time_endpoint(Some(dt), Some(DateGranularity::Month)), Some("2020-03".to_string()));
/// assert_eq!(format_valid_time_endpoint(Some(dt), Some(DateGranularity::Year)),  Some("2020".to_string()));
/// assert_eq!(format_valid_time_endpoint(Some(dt), Some(DateGranularity::Day)),   Some("2020-03-15".to_string()));
/// assert_eq!(format_valid_time_endpoint(None, Some(DateGranularity::Month)), None);
/// ```
pub fn format_valid_time_endpoint(
    date: Option<chrono::DateTime<chrono::Utc>>,
    granularity: Option<DateGranularity>,
) -> Option<String> {
    let dt = date?;
    Some(match granularity {
        Some(DateGranularity::Year) => format!("{}", dt.format("%Y")),
        Some(DateGranularity::Month) => format!("{}", dt.format("%Y-%m")),
        // Day and Instant both render at day precision — Instant sub-day detail is
        // intentionally omitted here to avoid surfacing fabricated precision for dates
        // that were normalised to midnight UTC during ingestion.
        Some(DateGranularity::Day) | Some(DateGranularity::Instant) => {
            format!("{}", dt.format("%Y-%m-%d"))
        }
        // None granularity (legacy row or unknown precision): fall back to day form.
        None => format!("{}", dt.format("%Y-%m-%d")),
    })
}

impl ValidTime {
    /// Returns true iff both start and end are None (unknown valid-time window).
    pub fn is_unknown(&self) -> bool {
        self.start.is_none() && self.end.is_none()
    }

    /// Returns true iff the interval is temporally incoherent: start > end,
    /// or start > tx_time (valid-time boundary must predate or equal the time it was learned).
    pub fn is_temporally_incoherent(&self, tx_time: &TransactionTime) -> bool {
        if let (Some(s), Some(e)) = (self.start, self.end) {
            if s > e {
                return true;
            }
        }
        if let Some(s) = self.start {
            if s > tx_time.0 {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn valid_time_unknown_when_both_none() {
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None};
        assert!(vt.is_unknown());
    }

    #[test]
    fn valid_time_not_unknown_when_start_set() {
        let vt = ValidTime { start: Some(Utc::now()), end: None, valid_time_confidence: 0.8 , start_granularity: None, end_granularity: None};
        assert!(!vt.is_unknown());
    }

    #[test]
    fn incoherent_when_start_after_end() {
        let now = Utc::now();
        let tx = TransactionTime(now);
        let vt = ValidTime {
            start: Some(now + chrono::Duration::hours(1)),
            end: Some(now),
            valid_time_confidence: 1.0,
            start_granularity: None, end_granularity: None,
        };
        assert!(vt.is_temporally_incoherent(&tx));
    }

    #[test]
    fn incoherent_when_valid_start_after_tx_time() {
        let now = Utc::now();
        let tx = TransactionTime(now);
        let vt = ValidTime {
            start: Some(now + chrono::Duration::hours(1)),
            end: None,
            valid_time_confidence: 1.0,
            start_granularity: None, end_granularity: None,
        };
        assert!(vt.is_temporally_incoherent(&tx));
    }

    #[test]
    fn coherent_normal_interval() {
        let now = Utc::now();
        let tx = TransactionTime(now);
        let vt = ValidTime {
            start: Some(now - chrono::Duration::days(1)),
            end: Some(now),
            valid_time_confidence: 0.9,
            start_granularity: None, end_granularity: None,
        };
        assert!(!vt.is_temporally_incoherent(&tx));
    }

    #[test]
    fn transaction_time_ordering() {
        let t1 = TransactionTime(Utc::now());
        let t2 = TransactionTime(Utc::now() + chrono::Duration::seconds(1));
        assert!(t1 < t2);
    }

    #[test]
    fn transaction_time_serializes_as_bare_iso8601_string() {
        use chrono::TimeZone;
        // Fixed timestamp: 2024-01-15T12:00:00Z
        let dt = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();
        let tt = TransactionTime(dt);
        let json = serde_json::to_string(&tt).unwrap();
        // chrono serializes DateTime<Utc> as RFC3339/ISO-8601 string
        assert!(json.starts_with('"'), "expected a bare JSON string, got: {json}");
        assert!(json.contains("2024-01-15"), "expected date in serialized form, got: {json}");
        let back: TransactionTime = serde_json::from_str(&json).unwrap();
        assert_eq!(tt, back);
    }

    #[test]
    fn valid_time_round_trip_serde() {
        let vt = ValidTime { start: Some(Utc::now()), end: None, valid_time_confidence: 0.7 , start_granularity: None, end_granularity: None};
        let json = serde_json::to_string(&vt).unwrap();
        let back: ValidTime = serde_json::from_str(&json).unwrap();
        assert_eq!(vt.start, back.start);
        assert_eq!(vt.end, back.end);
    }

    // ── W1c — DateGranularity + parse_valid_time_date ────────────────────────

    /// Old-format ValidTime (no granularity fields) must deserialise with both granularities = None.
    #[test]
    fn valid_time_old_format_compat_no_granularity_field() {
        let old_json = r#"{"start":null,"end":null,"valid_time_confidence":0.0}"#;
        let vt: ValidTime = serde_json::from_str(old_json).unwrap();
        assert_eq!(vt.start_granularity, None, "old-format ValidTime must deserialise start_granularity=None");
        assert_eq!(vt.end_granularity, None, "old-format ValidTime must deserialise end_granularity=None");
    }

    /// ValidTime with start_granularity=Year round-trips correctly.
    #[test]
    fn valid_time_with_granularity_round_trips() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let vt = ValidTime {
            start: Some(dt),
            end: None,
            valid_time_confidence: 0.9,
            start_granularity: Some(DateGranularity::Year),
            end_granularity: None,
        };
        let json = serde_json::to_string(&vt).unwrap();
        let back: ValidTime = serde_json::from_str(&json).unwrap();
        assert_eq!(back.start_granularity, Some(DateGranularity::Year));
        assert_eq!(back.end_granularity, None);
        assert_eq!(back.start, Some(dt));
    }

    /// ValidTime with both start and end granularity round-trips correctly.
    #[test]
    fn valid_time_with_both_granularities_round_trips() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2020, 3, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let vt = ValidTime {
            start: Some(start),
            end: Some(end),
            valid_time_confidence: 0.9,
            start_granularity: Some(DateGranularity::Month),
            end_granularity: Some(DateGranularity::Year),
        };
        let json = serde_json::to_string(&vt).unwrap();
        let back: ValidTime = serde_json::from_str(&json).unwrap();
        assert_eq!(back.start_granularity, Some(DateGranularity::Month));
        assert_eq!(back.end_granularity, Some(DateGranularity::Year));
    }

    /// parse_valid_time_date("YYYY") produces Year granularity and Jan 1 midnight UTC.
    #[test]
    fn parse_year_only() {
        let (dt, gran) = parse_valid_time_date("2024").unwrap();
        assert_eq!(gran, DateGranularity::Year);
        assert_eq!(dt.to_rfc3339(), "2024-01-01T00:00:00+00:00");
    }

    /// parse_valid_time_date("YYYY-MM") produces Month granularity and 1st-of-month midnight UTC.
    #[test]
    fn parse_year_month() {
        let (dt, gran) = parse_valid_time_date("2024-05").unwrap();
        assert_eq!(gran, DateGranularity::Month);
        assert_eq!(dt.to_rfc3339(), "2024-05-01T00:00:00+00:00");
    }

    /// parse_valid_time_date("YYYY-MM-DD") produces Day granularity and midnight UTC.
    #[test]
    fn parse_year_month_day() {
        let (dt, gran) = parse_valid_time_date("2024-05-15").unwrap();
        assert_eq!(gran, DateGranularity::Day);
        assert_eq!(dt.to_rfc3339(), "2024-05-15T00:00:00+00:00");
    }

    /// parse_valid_time_date with an RFC-3339 string produces Instant granularity.
    #[test]
    fn parse_full_instant() {
        let (dt, gran) = parse_valid_time_date("2024-05-15T10:30:00Z").unwrap();
        assert_eq!(gran, DateGranularity::Instant);
        assert_eq!(dt.to_rfc3339(), "2024-05-15T10:30:00+00:00");
    }

    /// Garbage input returns None.
    #[test]
    fn parse_invalid_returns_none() {
        assert!(parse_valid_time_date("not-a-date").is_none());
        assert!(parse_valid_time_date("").is_none());
        assert!(parse_valid_time_date("24-05").is_none());
    }

    /// DateGranularity serde uses snake_case.
    #[test]
    fn date_granularity_serde_snake_case() {
        let g = DateGranularity::Year;
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(json, r#""year""#);
        let back: DateGranularity = serde_json::from_str(&json).unwrap();
        assert_eq!(back, DateGranularity::Year);
    }

    /// Granularity fields are omitted from JSON when both are None (skip_serializing_if).
    #[test]
    fn granularity_none_not_serialised() {
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None };
        let json = serde_json::to_string(&vt).unwrap();
        assert!(!json.contains("granularity"), "None granularity fields must not appear in serialised JSON");
    }

    /// start_granularity Some(Month) IS serialised; end_granularity None is omitted.
    #[test]
    fn granularity_some_is_serialised() {
        use chrono::TimeZone;
        let vt = ValidTime {
            start: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            end: None,
            valid_time_confidence: 0.9,
            start_granularity: Some(DateGranularity::Month),
            end_granularity: None,
        };
        let json = serde_json::to_string(&vt).unwrap();
        assert!(json.contains("start_granularity"), "Some start_granularity must appear in serialised JSON");
        assert!(json.contains("month"), "granularity value must be 'month'");
        assert!(!json.contains("end_granularity"), "None end_granularity must not appear in serialised JSON");
    }

    // ── W5 — format_valid_time_endpoint render helper ────────────────────────

    /// Year granularity: only year component emitted, never month or day.
    #[test]
    fn render_helper_year_granularity_no_month_day() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
        let out = format_valid_time_endpoint(Some(dt), Some(DateGranularity::Year)).unwrap();
        assert_eq!(out, "2020", "Year granularity must output only YYYY");
        assert!(!out.contains('-'), "Year output must not contain a dash (no month/day)");
    }

    /// Month granularity: year and month emitted, never day.
    #[test]
    fn render_helper_month_granularity_no_day() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
        let out = format_valid_time_endpoint(Some(dt), Some(DateGranularity::Month)).unwrap();
        assert_eq!(out, "2020-03", "Month granularity must output YYYY-MM");
        // Must not contain a day component — splitting by '-' gives ["2020","03"] only.
        let parts: Vec<&str> = out.splitn(4, '-').collect();
        assert_eq!(parts.len(), 2, "Month output must have exactly two dash-separated parts (YYYY-MM)");
    }

    /// Day granularity: full YYYY-MM-DD.
    #[test]
    fn render_helper_day_granularity_full_date() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
        let out = format_valid_time_endpoint(Some(dt), Some(DateGranularity::Day)).unwrap();
        assert_eq!(out, "2020-03-15");
    }

    /// Instant granularity: rendered at day precision (sub-day suppressed to avoid
    /// fabricating precision when dates were normalised to midnight UTC at ingestion).
    #[test]
    fn render_helper_instant_granularity_renders_at_day() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 10, 30, 0).unwrap();
        let out = format_valid_time_endpoint(Some(dt), Some(DateGranularity::Instant)).unwrap();
        assert_eq!(out, "2020-03-15", "Instant renders at day precision");
    }

    /// None granularity (legacy / unknown precision): falls back to day form.
    #[test]
    fn render_helper_none_granularity_falls_back_to_day() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2020, 3, 15, 0, 0, 0).unwrap();
        let out = format_valid_time_endpoint(Some(dt), None).unwrap();
        assert_eq!(out, "2020-03-15", "None granularity must fall back to YYYY-MM-DD");
    }

    /// None date returns None regardless of granularity.
    #[test]
    fn render_helper_none_date_returns_none() {
        assert_eq!(format_valid_time_endpoint(None, Some(DateGranularity::Month)), None);
        assert_eq!(format_valid_time_endpoint(None, None), None);
    }
}
