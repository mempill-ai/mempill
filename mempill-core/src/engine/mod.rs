//! Engine domain internals — pub(crate) only (A29, TECHNICAL_DESIGN.md §1).
//!
//! This module hosts the deterministic core (C1–C8). Nothing here is part of
//! the public API; only `application/` and `EngineHandle` are public.
//!
//! Wave 3 modules (C1 + C7):
//! - `gateway` — C1 ingestion / write gateway (provenance stamp, tx-time)
//! - `gate`    — C7 adjudication gate (pure deterministic function)
//!
//! Dead-code lints are suppressed here: items are `pub(crate)` and will be consumed
//! by application/ and EngineHandle in W4–W7. The lints fire now only because no
//! callers exist outside tests yet.
#![allow(dead_code)]

pub(crate) mod gate;
pub(crate) mod gateway;
