//! EngineConfig — all OP-3 tuning parameters (A21, CONSTRAINTS.md §E3).
//!
//! All tunables are named fields here. Never hardcoded elsewhere.
//! Default values are the illustrative OP-3 starting points from TECHNICAL_DESIGN.md §12.
//! Tagged `// OP-3: calibrate post-v0.1` on every tunable field.

use std::time::Duration;

use mempill_types::Criticality;

/// Configuration for the mempill engine (A21, OP-3).
///
/// All fields are OP-3 tuning parameters — loadable from environment or config file.
/// Defaults are the illustrative v0.1 starting values; calibrate post-v0.1 based on
/// observed incident distribution (see TECHNICAL_DESIGN.md §12 OP-3 threat model).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Minimum valid_time_confidence required to treat extracted valid-time as authoritative.
    /// Below this → treat as unknown; fall back to tx_time ordering (B7, F4).
    // OP-3: calibrate post-v0.1
    pub valid_time_confidence_threshold: f32,

    /// Minimum number of provenance-independent corroborating sources for a currency boost.
    /// Corroboration is a CONFIDENCE MODIFIER only — not a truth-gate (F2, A6).
    // OP-3: calibrate post-v0.1
    pub corroboration_count_for_currency_boost: u32,

    /// Daily fractional decay rate for currency (I11, V3-6).
    /// Applied as: `current_currency = initial * (1 - rate) ^ days_since_refresh`.
    // OP-3: calibrate post-v0.1
    pub currency_decay_rate_per_day: f32,

    /// Minimum criticality for which oracle escalation is mandatory on belief-overturning ops.
    // OP-3: calibrate post-v0.1
    pub criticality_overturn_floor: Criticality,

    /// Maximum derivation depth eligible for currency boost (OP-1 provenance laundering cap).
    /// Claims with depth > cap cannot receive currency boosts from corroboration.
    // OP-3: calibrate post-v0.1
    pub derivation_depth_cap_for_currency_boost: u32,

    /// Maximum derivation depth eligible to overturn an incumbent belief (OP-1 self-limiting cap).
    /// Claims with depth > cap cannot overturn; routed to PendingConflict instead.
    // OP-3: calibrate post-v0.1
    pub derivation_depth_cap_for_overturning: u32,

    /// Number of identical RecallReEntry candidates in a single write batch that triggers Quarantine.
    /// I6 / C6 amplification guard burst detection threshold (firewall.rs).
    // OP-3: calibrate post-v0.1
    pub quarantine_burst_threshold: u32,

    /// Days since last currency refresh before a belief enters the AgingUnconfirmed state (I11, V3-6).
    // OP-3: calibrate post-v0.1
    pub aging_unconfirmed_threshold_days: u32,

    /// Days since last currency refresh before a belief enters the Decayed state (I11, V3-6).
    // OP-3: calibrate post-v0.1
    pub decayed_threshold_days: u32,

    /// Default TTL for pending adjudications (W6).
    ///
    /// When `Some(d)`, every pending row gets `expires_at = queued_at + d`. When `None`,
    /// no TTL is set and pending rows never expire via the engine sweep.
    ///
    /// Per-request TTL override is deferred to a future wave (the `IngestClaimRequest` DTO
    /// does not yet carry a TTL field); the config default is the v1 mechanism.
    // OP-3: calibrate post-v0.1
    pub default_adjudication_ttl: Option<Duration>,
}

impl Default for EngineConfig {
    /// Illustrative OP-3 defaults (CONSTRAINTS.md E3, TECHNICAL_DESIGN.md §12).
    /// These are starting points — calibrate post-v0.1 based on production incident distribution.
    fn default() -> Self {
        Self {
            valid_time_confidence_threshold: 0.7,
            corroboration_count_for_currency_boost: 2,
            currency_decay_rate_per_day: 0.05,
            criticality_overturn_floor: Criticality::High,
            derivation_depth_cap_for_currency_boost: 3,
            derivation_depth_cap_for_overturning: 2,
            quarantine_burst_threshold: 10,
            aging_unconfirmed_threshold_days: 30,
            decayed_threshold_days: 90,
            default_adjudication_ttl: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_valid_time_confidence_threshold() {
        assert_eq!(EngineConfig::default().valid_time_confidence_threshold, 0.7);
    }

    #[test]
    fn default_corroboration_count_for_currency_boost() {
        assert_eq!(EngineConfig::default().corroboration_count_for_currency_boost, 2);
    }

    #[test]
    fn default_currency_decay_rate_per_day() {
        // Use approximate equality for f32
        let delta = (EngineConfig::default().currency_decay_rate_per_day - 0.05).abs();
        assert!(delta < f32::EPSILON * 10.0, "expected 0.05, got {}", EngineConfig::default().currency_decay_rate_per_day);
    }

    #[test]
    fn default_criticality_overturn_floor() {
        assert_eq!(EngineConfig::default().criticality_overturn_floor, Criticality::High);
    }

    #[test]
    fn default_derivation_depth_cap_for_currency_boost() {
        assert_eq!(EngineConfig::default().derivation_depth_cap_for_currency_boost, 3);
    }

    #[test]
    fn default_derivation_depth_cap_for_overturning() {
        assert_eq!(EngineConfig::default().derivation_depth_cap_for_overturning, 2);
    }

    #[test]
    fn default_quarantine_burst_threshold() {
        assert_eq!(EngineConfig::default().quarantine_burst_threshold, 10);
    }

    #[test]
    fn default_aging_unconfirmed_threshold_days() {
        assert_eq!(EngineConfig::default().aging_unconfirmed_threshold_days, 30);
    }

    #[test]
    fn default_decayed_threshold_days() {
        assert_eq!(EngineConfig::default().decayed_threshold_days, 90);
    }

    #[test]
    fn engine_config_is_clone() {
        let cfg = EngineConfig::default();
        let cloned = cfg.clone();
        assert_eq!(cloned.quarantine_burst_threshold, 10);
    }

    #[test]
    fn engine_config_debug_format_contains_field_names() {
        let s = format!("{:?}", EngineConfig::default());
        assert!(s.contains("valid_time_confidence_threshold"));
        assert!(s.contains("quarantine_burst_threshold"));
    }
}
