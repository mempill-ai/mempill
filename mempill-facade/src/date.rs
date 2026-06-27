//! Lenient date normalization for the Tier-1 / Tier-2 ergonomic surface.
//!
//! Accepts human-friendly date strings and normalises them to `DateTime<Utc>`.
//! Natural-language dates ("March 2020") are the host's concern — they produce
//! `MempillDxError::UnparsableDate` with an actionable hint.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::ergonomic::MempillDxError;

const HINT: &str =
    "Use YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339 (e.g. 2026-01-01T00:00:00Z). \
     Natural-language dates must be resolved by the caller before passing to remember().";

/// Parse a lenient date string into a UTC `DateTime`.
///
/// | Input | Normalised to |
/// |-------|--------------|
/// | `"2020"` | `2020-01-01T00:00:00Z` |
/// | `"2020-03"` | `2020-03-01T00:00:00Z` |
/// | `"2020-03-15"` | `2020-03-15T00:00:00Z` |
/// | `"2020-03-15T12:00:00Z"` | pass-through |
/// | anything else | `MempillDxError::UnparsableDate` |
///
/// **Precision note:** a partial date snaps to the **start of the period** (`2020` → Jan 1,
/// `2020-03` → the 1st). The filled-in day/month is a normalization placeholder for a sortable
/// instant — **not** asserted precision. Granularity-aware valid-time (rendering "March 2020")
/// is planned for v0.3.
///
/// # Errors
/// Returns `MempillDxError::UnparsableDate { input, hint }` for unrecognised formats.
pub fn parse_lenient_date(s: &str) -> Result<DateTime<Utc>, MempillDxError> {
    let s = s.trim();

    if s.is_empty() {
        return Err(MempillDxError::UnparsableDate { input: s.to_string(), hint: HINT });
    }

    // ── RFC3339 / ISO-8601 full timestamp ────────────────────────────────────
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // ── YYYY-MM-DD ───────────────────────────────────────────────────────────
    if s.len() == 10 && s.chars().filter(|c| *c == '-').count() == 2 {
        let parts: Vec<&str> = s.splitn(3, '-').collect();
        if parts.len() == 3 {
            if let (Ok(y), Ok(m), Ok(d)) = (
                parts[0].parse::<i32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            ) {
                if let Some(nd) = NaiveDate::from_ymd_opt(y, m, d) {
                    let ndt = NaiveDateTime::new(nd, chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());
                    return Ok(Utc.from_utc_datetime(&ndt));
                }
            }
        }
    }

    // ── YYYY-MM ──────────────────────────────────────────────────────────────
    if s.len() == 7 && s.chars().filter(|c| *c == '-').count() == 1 {
        let parts: Vec<&str> = s.splitn(2, '-').collect();
        if parts.len() == 2 {
            if let (Ok(y), Ok(m)) = (parts[0].parse::<i32>(), parts[1].parse::<u32>()) {
                if let Some(nd) = NaiveDate::from_ymd_opt(y, m, 1) {
                    let ndt = NaiveDateTime::new(nd, chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());
                    return Ok(Utc.from_utc_datetime(&ndt));
                }
            }
        }
    }

    // ── YYYY ─────────────────────────────────────────────────────────────────
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(y) = s.parse::<i32>() {
            if let Some(nd) = NaiveDate::from_ymd_opt(y, 1, 1) {
                let ndt = NaiveDateTime::new(nd, chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap());
                return Ok(Utc.from_utc_datetime(&ndt));
            }
        }
    }

    Err(MempillDxError::UnparsableDate { input: s.to_string(), hint: HINT })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn parse_year_only() {
        let dt = parse_lenient_date("2026").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn parse_year_month() {
        let dt = parse_lenient_date("2026-06").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 6);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn parse_full_date() {
        let dt = parse_lenient_date("2026-06-15").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 6);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn parse_rfc3339_passthrough() {
        let input = "2026-06-15T12:30:00Z";
        let dt = parse_lenient_date(input).unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 6);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 12);
    }

    #[test]
    fn unparsable_natural_language() {
        let err = parse_lenient_date("March 2020").unwrap_err();
        match err {
            MempillDxError::UnparsableDate { input, hint } => {
                assert_eq!(input, "March 2020");
                assert!(!hint.contains("premature end of input"), "hint must not say 'premature end of input'");
                assert!(hint.contains("YYYY"), "hint must mention YYYY format");
            }
            other => panic!("expected UnparsableDate, got {:?}", other),
        }
    }

    #[test]
    fn unparsable_empty_string() {
        let err = parse_lenient_date("").unwrap_err();
        assert!(matches!(err, MempillDxError::UnparsableDate { .. }));
    }

    #[test]
    fn midnight_utc_for_all_non_rfc3339_forms() {
        for s in &["2020", "2020-03", "2020-03-15"] {
            let dt = parse_lenient_date(s).unwrap();
            assert_eq!(dt.hour(), 0);
            assert_eq!(dt.minute(), 0);
            assert_eq!(dt.second(), 0);
        }
    }
}

