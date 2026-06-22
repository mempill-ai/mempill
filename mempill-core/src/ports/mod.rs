//! Port traits — the hexagonal seams of mempill-core (A4, A15, A16, A18).
//!
//! All port traits are SYNCHRONOUS (no async fn) per F1 decision.
//! Async lives only at the EngineHandle boundary (W7) via spawn_blocking.
//!
//! Visibility: `pub` — port traits must be visible to adapter crates (e.g. mempill-sqlite).

pub mod embedding;
pub mod extractor;
pub mod model;
pub mod oracle;
pub mod persistence;

// Flat re-exports for ergonomic use within mempill-core and adapter crates.
pub use embedding::{EmbeddingPort, VectorPort};
pub use extractor::ExtractorPort;
pub use oracle::OraclePort;
pub use persistence::{PersistencePort, Txn};
