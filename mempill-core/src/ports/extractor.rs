//! ExtractorPort — stochastic proposer port (SDK_CONTRACT §7, I5, A16).
//!
//! Host-implemented. `extract()` returns PROPOSALS only.
//! The deterministic core decides all dispositions — the extractor never commits.

use mempill_types::ClaimProposal;

/// Stochastic extractor port (SDK_CONTRACT §7, I5, A16). Host-implemented.
/// `extract()` returns PROPOSALS. The deterministic core decides all dispositions.
pub trait ExtractorPort: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    fn extract(
        &self,
        raw_content: &str,
        context: &str,
    ) -> Result<Vec<ClaimProposal>, Self::Error>;
}
