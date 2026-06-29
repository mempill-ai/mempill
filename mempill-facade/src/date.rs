//! Lenient date normalization for the Tier-1 / Tier-2 ergonomic surface.
//!
//! Accepts human-friendly date strings and normalises them to `DateTime<Utc>`.
//! Natural-language dates ("March 2020") are the host's concern — they produce
//! `MempillDxError::UnparsableDate` with an actionable hint.
//!
//! The granularity-aware version is [`mempill_types::time::parse_valid_time_date`].
//! The write path in `ergonomic.rs` calls that directly; `parse_lenient_date` is
//! kept as a thin shim for callers that only need the `DateTime` (no granularity).

use chrono::{DateTime, Utc};
use mempill_types::time::parse_valid_time_date;

use crate::ergonomic::MempillDxError;

const HINT: &str =
    "Use YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339 (e.g. 2026-01-01T00:00:00Z). \
     Natural-language dates must be resolved by the caller before passing to remember().";

/// Parse a lenient date string into a UTC `DateTime`, discarding granularity.
///
/// This is a thin shim over [`mempill_types::time::parse_valid_time_date`] for callers
/// that only need the `DateTime<Utc>` value (no precision tracking). The write path in
/// `ergonomic.rs` calls `parse_valid_time_date` directly to capture both components.
///
/// | Input | Normalised to |
/// |-------|--------------|
/// | `"2020"` | `2020-01-01T00:00:00Z` |
/// | `"2020-03"` | `2020-03-01T00:00:00Z` |
/// | `"2020-03-15"` | `2020-03-15T00:00:00Z` |
/// | `"2020-03-15T12:00:00Z"` | pass-through |
/// | anything else | `MempillDxError::UnparsableDate` |
///
/// # Errors
/// Returns `MempillDxError::UnparsableDate { input, hint }` for unrecognised formats.
pub fn parse_lenient_date(s: &str) -> Result<DateTime<Utc>, MempillDxError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(MempillDxError::UnparsableDate { input: s.to_string(), hint: HINT });
    }
    parse_valid_time_date(s)
        .map(|(dt, _gran)| dt)
        .ok_or_else(|| MempillDxError::UnparsableDate { input: s.to_string(), hint: HINT })
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
            other => panic!("expected UnparsableDate, got {other:?}"),
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

