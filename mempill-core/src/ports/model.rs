//! Re-exports all port traits for convenient import by engine modules and adapter crates.

pub use super::embedding::{EmbeddingPort, VectorPort};
pub use super::extractor::ExtractorPort;
pub use super::oracle::OraclePort;
pub use super::persistence::{PersistencePort, Txn};
