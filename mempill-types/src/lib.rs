//! # mempill-types
//!
//! Shared domain types for the mempill temporally-correct AI-agent memory engine.
//!
//! This crate contains only pure data types (Value Objects and Entities in DDD terms).
//! It has no I/O, no port traits, no engine logic, and no SQL.
//!
//! ## Modules
//! - `identity`    — [`AgentId`], [`ClaimRef`], [`SubjectLineRef`]
//! - `provenance`  — [`ProvenanceLabel`], [`ExternalKind`], [`ExternalAnchor`]
//! - `time`        — [`TransactionTime`], [`ValidTime`]
//! - `claim`       — [`Claim`], [`Fact`], [`Cardinality`], [`Confidence`], [`Criticality`]
//! - `disposition` — [`Disposition`] (12-state model)
//! - `belief`      — [`BeliefProjection`], [`Belief`], [`CurrencySignal`], [`CurrencyState`],
//!                   [`BeliefStatus`], [`StalenessFlag`], [`Marker`]
//! - `validity`    — [`ValidityAssertion`], [`AssertionKind`]
//! - `edge`        — [`ClaimEdge`], [`EdgeKind`]
//! - `ledger`      — [`LedgerEntry`], [`LedgerEventKind`]
//! - `proposal`    — [`ClaimProposal`], [`AdjudicationRequest`], [`AdjudicationResponse`],
//!                   [`AdjudicationVerdict`], [`OverturnReason`], [`AdjudicationOutcome`]

pub mod belief;
pub mod claim;
pub mod disposition;
pub mod edge;
pub mod identity;
pub mod ledger;
pub mod proposal;
pub mod provenance;
pub mod time;
pub mod validity;

// ── Public re-exports ────────────────────────────────────────────────────────

pub use belief::{
    Belief, BeliefProjection, BeliefStatus, CurrencySignal, CurrencyState, HistoryEntryStatus,
    Marker, StalenessFlag,
};
pub use claim::{Cardinality, Claim, Confidence, Criticality, Fact};
pub use disposition::{Disposition, WriteOutcome};
pub use edge::{ClaimEdge, EdgeKind};
pub use identity::{AgentId, ClaimRef, SubjectLineRef};
pub use ledger::{LedgerEntry, LedgerEventKind};
pub use proposal::{
    AdjudicationOutcome, AdjudicationRequest, AdjudicationResponse, AdjudicationVerdict,
    ClaimProposal, OverturnReason,
};
pub use provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel};
pub use time::{TransactionTime, ValidTime};
pub use validity::{AssertionKind, ValidityAssertion};
