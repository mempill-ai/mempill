//! Concurrency primitives for mempill-core.
//!
//! Provides the per-agent_id write lock that enforces single-writer-per-agent_id
//! at the async task layer within a single process.
#![allow(dead_code)]

pub(crate) mod agent_lock;
