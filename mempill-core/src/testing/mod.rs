//! Test support utilities for mempill-core.
//!
//! Gated by `#[cfg(any(test, feature = "test-support"))]`.
//! Not compiled into production builds.

#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
