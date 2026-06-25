//! ExtractorPort — stochastic proposer port.
//!
//! Host-implemented. `extract()` returns proposals only.
//! The deterministic core decides all dispositions — the extractor never commits.

use mempill_types::ClaimProposal;

/// Stochastic extractor port. Host-implemented.
///
/// `extract()` returns proposals; the deterministic core (reconciler + adjudication gate)
/// decides all dispositions. No stochastic output can commit directly.
pub trait ExtractorPort: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn extract(
        &self,
        raw_content: &str,
        context: &str,
    ) -> Result<Vec<ClaimProposal>, Self::Error>;
}
