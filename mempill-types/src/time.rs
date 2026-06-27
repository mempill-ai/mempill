//! Temporal types: bi-temporal model support.

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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ValidTime {
    /// Start of the valid-time window (`None` = unknown / open-ended).
    pub start: Option<chrono::DateTime<chrono::Utc>>,
    /// End of the valid-time window (`None` = unknown / open-ended).
    pub end: Option<chrono::DateTime<chrono::Utc>>,
    /// Confidence in the valid-time extraction itself (mirrors Confidence.valid_time_confidence).
    pub valid_time_confidence: f32,
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
        let vt = ValidTime { start: None, end: None, valid_time_confidence: 0.0 };
        assert!(vt.is_unknown());
    }

    #[test]
    fn valid_time_not_unknown_when_start_set() {
        let vt = ValidTime { start: Some(Utc::now()), end: None, valid_time_confidence: 0.8 };
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
        let vt = ValidTime { start: Some(Utc::now()), end: None, valid_time_confidence: 0.7 };
        let json = serde_json::to_string(&vt).unwrap();
        let back: ValidTime = serde_json::from_str(&json).unwrap();
        assert_eq!(vt.start, back.start);
        assert_eq!(vt.end, back.end);
    }
}
