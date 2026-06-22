//! Concurrency primitives for mempill-core.
//!
//! Wave 3: per-agent_id write lock (single-writer enforcement, DC-2, I9, A22).
//!
//! Dead-code lint suppressed: `AgentWriteLockMap` is `pub(crate)` and will be
//! consumed by `EngineHandle` in W7. The lint fires now because no caller outside
//! tests exists yet.
#![allow(dead_code)]

pub(crate) mod agent_lock;
