//! Engine domain internals — pub(crate) only.
//!
//! This module hosts the deterministic engine core. Nothing here is part of
//! the public API; only `application/` and `EngineHandle` are public.
//!
//! Components:
//! - `gateway`      — ingestion / write gateway (provenance stamp, tx-time)
//! - `gate`         — adjudication gate (pure deterministic function)
//!
//! Dead-code lints are suppressed here: items are `pub(crate)` and will be consumed
//! by application/ and EngineHandle. The lints fire now only because no
//! callers exist outside tests yet.
#![allow(dead_code)]

pub(crate) mod gate;
pub(crate) mod gateway;
pub(crate) mod firewall;
pub(crate) mod reconciler;
pub(crate) mod supersession;
pub(crate) mod truth_engine;
pub(crate) mod projection;
pub(crate) mod audit_ledger;
pub(crate) mod valid_time_helpers;
