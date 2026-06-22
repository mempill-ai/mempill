//! `SqlitePersistenceStore` — impl of `PersistencePort` for mempill-sqlite (§4, §5, I1, I9).
//!
//! # Append-only invariant (I1)
//!
//! Every write method is an INSERT.  No UPDATE or DELETE paths exist in this file.
//! Attempts to update or delete data must be rejected at the application layer.
//!
//! # Atomic commit unit (I9)
//!
//! The store does NOT manage transaction lifecycle — the application use-case does (§4a).
//! `begin_atomic` moves the connection into a `SqliteTxn`; `commit` and `rollback` return
//! it.  This guarantees that {claim + validity assertion + ledger entry + edge} land in one
//! SQLite transaction or not at all.
//!
//! # Single-writer (DC-2)
//!
//! v0.1 is single-process embedded.  The `AgentWriteLockMap` in mempill-core coordinates
//! per-agent_id writes at the async boundary.  The store is structurally read-safe because
//! reads do not acquire any lock, and writes are serialised by the application layer.
//!
//! # Connection ownership model
//!
//! The store owns `Box<Connection>` inside a `std::cell::Cell`-like hand-off: `begin_atomic`
//! takes it out; `commit`/`rollback` put it back.  We use `Arc<Mutex<Option<Box<Connection>>>>`
//! so the store is `Send + Sync` and can be shared across `spawn_blocking` calls.
//! The `Option` is always `Some` except during the window between `begin_atomic` and
//! `commit`/`rollback`.  Calling `begin_atomic` while a txn is already open returns an error.

use std::sync::{Arc, Mutex};

use mempill_core::ports::persistence::PersistencePort;
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    edge::{ClaimEdge, EdgeKind},
    identity::{AgentId, ClaimRef},
    ledger::{LedgerEntry, LedgerEventKind},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
    validity::{AssertionKind, ValidityAssertion},
};
use rusqlite::Connection;

use crate::{txn::SqliteTxn, SqliteStoreError};

// ── SqlitePersistenceStore ────────────────────────────────────────────────────

/// The SQLite-backed implementation of `PersistencePort`.
///
/// Construct via `SqlitePersistenceStore::new(conn)` where `conn` is a fully-initialised
/// rusqlite `Connection` (PRAGMAs applied, migrations run — use `connection::open` or
/// `connection::open_in_memory`).
pub struct SqlitePersistenceStore {
    /// Connection slot.  `None` only while a `SqliteTxn` is active.
    conn: Arc<Mutex<Option<Box<Connection>>>>,
}

impl SqlitePersistenceStore {
    /// Create a store wrapping an already-initialised `Connection`.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(Some(Box::new(conn)))),
        }
    }
}

// SAFETY: Connection is Send (rusqlite guarantees this); Mutex makes it Sync.
unsafe impl Send for SqlitePersistenceStore {}
unsafe impl Sync for SqlitePersistenceStore {}

// ── Domain-type ↔ column mapping helpers ─────────────────────────────────────

/// Serialize `ProvenanceLabel` to the TEXT column value used in the schema (§5).
/// Format: `'ModelDerived'`, `'RecallReEntry'`, `'External_UserAsserted'`,
/// `'External_ExternalFirstHand'`.
fn provenance_to_str(p: &ProvenanceLabel) -> &'static str {
    match p {
        ProvenanceLabel::ModelDerived => "ModelDerived",
        ProvenanceLabel::RecallReEntry => "RecallReEntry",
        ProvenanceLabel::External(ExternalKind::UserAsserted) => "External_UserAsserted",
        ProvenanceLabel::External(ExternalKind::ExternalFirstHand) => "External_ExternalFirstHand",
        // ProvenanceLabel is #[non_exhaustive]; future variants will be caught here at compile time.
        _ => "Unknown",
    }
}

/// Deserialize the TEXT column value back to `ProvenanceLabel`.
/// Used by the read path.
fn str_to_provenance(s: &str) -> Result<ProvenanceLabel, SqliteStoreError> {
    match s {
        "ModelDerived" => Ok(ProvenanceLabel::ModelDerived),
        "RecallReEntry" => Ok(ProvenanceLabel::RecallReEntry),
        "External_UserAsserted" => {
            Ok(ProvenanceLabel::External(ExternalKind::UserAsserted))
        }
        "External_ExternalFirstHand" => {
            Ok(ProvenanceLabel::External(ExternalKind::ExternalFirstHand))
        }
        other => Err(SqliteStoreError::Mapping(format!(
            "unknown provenance_label value: {other}"
        ))),
    }
}

fn cardinality_to_str(c: &Cardinality) -> &'static str {
    match c {
        Cardinality::Functional => "Functional",
        Cardinality::SetValued => "SetValued",
        Cardinality::Unknown => "Unknown",
    }
}

fn str_to_cardinality(s: &str) -> Result<Cardinality, SqliteStoreError> {
    match s {
        "Functional" => Ok(Cardinality::Functional),
        "SetValued" => Ok(Cardinality::SetValued),
        "Unknown" => Ok(Cardinality::Unknown),
        other => Err(SqliteStoreError::Mapping(format!(
            "unknown cardinality value: {other}"
        ))),
    }
}

fn criticality_to_str(c: &Criticality) -> &'static str {
    match c {
        Criticality::Low => "Low",
        Criticality::Medium => "Medium",
        Criticality::High => "High",
        Criticality::Critical => "Critical",
    }
}

fn str_to_criticality(s: &str) -> Result<Criticality, SqliteStoreError> {
    match s {
        "Low" => Ok(Criticality::Low),
        "Medium" => Ok(Criticality::Medium),
        "High" => Ok(Criticality::High),
        "Critical" => Ok(Criticality::Critical),
        other => Err(SqliteStoreError::Mapping(format!(
            "unknown criticality value: {other}"
        ))),
    }
}

fn edge_kind_to_str(k: &EdgeKind) -> &'static str {
    match k {
        EdgeKind::DerivedFrom => "DerivedFrom",
        EdgeKind::Supersedes => "Supersedes",
        EdgeKind::DependsOn => "DependsOn",
        EdgeKind::MutualExclusion => "MutualExclusion",
    }
}

fn str_to_edge_kind(s: &str) -> Result<EdgeKind, SqliteStoreError> {
    match s {
        "DerivedFrom" => Ok(EdgeKind::DerivedFrom),
        "Supersedes" => Ok(EdgeKind::Supersedes),
        "DependsOn" => Ok(EdgeKind::DependsOn),
        "MutualExclusion" => Ok(EdgeKind::MutualExclusion),
        other => Err(SqliteStoreError::Mapping(format!(
            "unknown edge_kind value: {other}"
        ))),
    }
}

fn ledger_event_kind_to_str(k: &LedgerEventKind) -> &'static str {
    match k {
        LedgerEventKind::ClaimCommitted => "ClaimCommitted",
        LedgerEventKind::ValidityAsserted => "ValidityAsserted",
        LedgerEventKind::AdjudicationRequested => "AdjudicationRequested",
        LedgerEventKind::AdjudicationResolved => "AdjudicationResolved",
        LedgerEventKind::RecallReEntryDetected => "RecallReEntryDetected",
        LedgerEventKind::Quarantined => "Quarantined",
        LedgerEventKind::DependentFlaggedPendingReview => "DependentFlaggedPendingReview",
        LedgerEventKind::ServedAsInjected => "ServedAsInjected",
    }
}

fn str_to_ledger_event_kind(s: &str) -> Result<LedgerEventKind, SqliteStoreError> {
    match s {
        "ClaimCommitted" => Ok(LedgerEventKind::ClaimCommitted),
        "ValidityAsserted" => Ok(LedgerEventKind::ValidityAsserted),
        "AdjudicationRequested" => Ok(LedgerEventKind::AdjudicationRequested),
        "AdjudicationResolved" => Ok(LedgerEventKind::AdjudicationResolved),
        "RecallReEntryDetected" => Ok(LedgerEventKind::RecallReEntryDetected),
        "Quarantined" => Ok(LedgerEventKind::Quarantined),
        "DependentFlaggedPendingReview" => Ok(LedgerEventKind::DependentFlaggedPendingReview),
        "ServedAsInjected" => Ok(LedgerEventKind::ServedAsInjected),
        other => Err(SqliteStoreError::Mapping(format!(
            "unknown ledger event_kind value: {other}"
        ))),
    }
}

fn disposition_to_str(d: &mempill_types::disposition::Disposition) -> &'static str {
    use mempill_types::disposition::Disposition;
    match d {
        Disposition::CommittedCheap => "CommittedCheap",
        Disposition::CommittedInferred => "CommittedInferred",
        Disposition::QueuedForAdjudication => "QueuedForAdjudication",
        Disposition::Contested => "Contested",
        Disposition::PendingConflict => "PendingConflict",
        Disposition::PendingReview => "PendingReview",
        Disposition::PendingLowConfidence => "PendingLowConfidence",
        Disposition::Quarantined => "Quarantined",
        Disposition::Superseded => "Superseded",
        Disposition::Invalidated => "Invalidated",
        Disposition::Reinstated => "Reinstated",
        Disposition::Rejected => "Rejected",
    }
}

fn str_to_disposition(s: &str) -> Result<mempill_types::disposition::Disposition, SqliteStoreError> {
    use mempill_types::disposition::Disposition;
    match s {
        "CommittedCheap" => Ok(Disposition::CommittedCheap),
        "CommittedInferred" => Ok(Disposition::CommittedInferred),
        "QueuedForAdjudication" => Ok(Disposition::QueuedForAdjudication),
        "Contested" => Ok(Disposition::Contested),
        "PendingConflict" => Ok(Disposition::PendingConflict),
        "PendingReview" => Ok(Disposition::PendingReview),
        "PendingLowConfidence" => Ok(Disposition::PendingLowConfidence),
        "Quarantined" => Ok(Disposition::Quarantined),
        "Superseded" => Ok(Disposition::Superseded),
        "Invalidated" => Ok(Disposition::Invalidated),
        "Reinstated" => Ok(Disposition::Reinstated),
        "Rejected" => Ok(Disposition::Rejected),
        other => Err(SqliteStoreError::Mapping(format!(
            "unknown disposition value: {other}"
        ))),
    }
}

// ── Row-to-domain-type mapping helpers ───────────────────────────────────────

/// Map a rusqlite `Row` from the `claims` table to a `Claim` domain type.
///
/// Column order (must match every SELECT that feeds this function):
///   0  claim_id
///   1  agent_id
///   2  subject
///   3  predicate
///   4  value  (JSON text)
///   5  cardinality
///   6  provenance_label
///   7  nearest_external_anchor_id  (nullable TEXT)
///   8  derivation_depth
///   9  tx_time
///  10  valid_time_start  (nullable)
///  11  valid_time_end    (nullable)
///  12  valid_time_confidence
///  13  value_confidence
///  14  criticality
///  15  derived_from  (JSON array of UUID strings)
///  16  metadata      (nullable JSON text)
///  17  snapshot_schema_version  (nullable INTEGER)
fn row_to_claim(row: &rusqlite::Row<'_>) -> Result<Claim, rusqlite::Error> {
    // We map rusqlite errors to SqliteStoreError in the caller; use rusqlite::Error here
    // so this fn can be used directly as a row-mapper closure.
    let claim_id_str: String = row.get(0)?;
    let agent_id_str: String = row.get(1)?;
    let subject: String = row.get(2)?;
    let predicate: String = row.get(3)?;
    let value_json: String = row.get(4)?;
    let cardinality_str: String = row.get(5)?;
    let provenance_str: String = row.get(6)?;
    let nearest_anchor_str: Option<String> = row.get(7)?;
    let derivation_depth: i64 = row.get(8)?;
    let tx_time_str: String = row.get(9)?;
    let valid_time_start_str: Option<String> = row.get(10)?;
    let valid_time_end_str: Option<String> = row.get(11)?;
    let valid_time_confidence: f64 = row.get(12)?;
    let value_confidence: f64 = row.get(13)?;
    let criticality_str: String = row.get(14)?;
    let derived_from_json: String = row.get(15)?;
    let metadata_json: Option<String> = row.get(16)?;
    let snapshot_schema_version_raw: Option<i64> = row.get(17)?;

    // These mapping errors cannot be expressed as rusqlite::Error cleanly; use
    // rusqlite::Error::InvalidColumnType as a carrier — callers convert to SqliteStoreError.
    let to_rusqlite_err = |msg: String| rusqlite::Error::InvalidColumnType(
        0,
        msg,
        rusqlite::types::Type::Text,
    );

    let claim_id = uuid::Uuid::parse_str(&claim_id_str)
        .map_err(|e| to_rusqlite_err(format!("claim_id UUID parse: {e}")))?;

    let value: serde_json::Value = serde_json::from_str(&value_json)
        .map_err(|e| to_rusqlite_err(format!("value JSON parse: {e}")))?;

    let cardinality = str_to_cardinality(&cardinality_str)
        .map_err(|e| to_rusqlite_err(e.to_string()))?;

    let provenance = str_to_provenance(&provenance_str)
        .map_err(|e| to_rusqlite_err(e.to_string()))?;

    let nearest_external_anchor: Option<ClaimRef> = nearest_anchor_str
        .map(|s| {
            uuid::Uuid::parse_str(&s)
                .map(ClaimRef)
                .map_err(|e| to_rusqlite_err(format!("anchor UUID parse: {e}")))
        })
        .transpose()?;

    let tx_time = chrono::DateTime::parse_from_rfc3339(&tx_time_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| to_rusqlite_err(format!("tx_time parse: {e}")))?;

    let valid_time_start = valid_time_start_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| to_rusqlite_err(format!("valid_time_start parse: {e}")))
        })
        .transpose()?;

    let valid_time_end = valid_time_end_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| to_rusqlite_err(format!("valid_time_end parse: {e}")))
        })
        .transpose()?;

    let criticality = str_to_criticality(&criticality_str)
        .map_err(|e| to_rusqlite_err(e.to_string()))?;

    let derived_from_uuids: Vec<String> = serde_json::from_str(&derived_from_json)
        .map_err(|e| to_rusqlite_err(format!("derived_from JSON parse: {e}")))?;

    let derived_from: Vec<ClaimRef> = derived_from_uuids
        .iter()
        .map(|s| {
            uuid::Uuid::parse_str(s)
                .map(ClaimRef)
                .map_err(|e| to_rusqlite_err(format!("derived_from UUID parse: {e}")))
        })
        .collect::<Result<_, _>>()?;

    let metadata: Option<serde_json::Value> = metadata_json
        .map(|s| {
            serde_json::from_str(&s)
                .map_err(|e| to_rusqlite_err(format!("metadata JSON parse: {e}")))
        })
        .transpose()?;

    let snapshot_schema_version: Option<u32> =
        snapshot_schema_version_raw.map(|v| v as u32);

    Ok(Claim::new(
        ClaimRef(claim_id),
        AgentId(agent_id_str),
        Fact { subject, predicate, value },
        cardinality,
        provenance,
        ExternalAnchor {
            nearest_external_anchor,
            derivation_depth: derivation_depth as u32,
        },
        TransactionTime(tx_time),
        ValidTime {
            start: valid_time_start,
            end: valid_time_end,
            valid_time_confidence: valid_time_confidence as f32,
        },
        Confidence {
            value_confidence: value_confidence as f32,
            valid_time_confidence: valid_time_confidence as f32,
        },
        criticality,
        derived_from,
        metadata,
        snapshot_schema_version,
    ))
}

/// The SELECT column list that must be used with `row_to_claim`.
/// Columns must be in the exact order defined in `row_to_claim`.
const CLAIM_SELECT_COLS: &str = "
    claim_id, agent_id, subject, predicate, value, cardinality,
    provenance_label, nearest_external_anchor_id, derivation_depth,
    tx_time, valid_time_start, valid_time_end, valid_time_confidence,
    value_confidence, criticality, derived_from,
    metadata, snapshot_schema_version
";

/// Map a rusqlite `Row` from the `claim_edges` table to a `ClaimEdge` domain type.
fn row_to_edge(row: &rusqlite::Row<'_>) -> Result<ClaimEdge, rusqlite::Error> {
    let to_err = |msg: String| rusqlite::Error::InvalidColumnType(
        0, msg, rusqlite::types::Type::Text,
    );

    let edge_id_str: String = row.get(0)?;
    let agent_id_str: String = row.get(1)?;
    let from_claim_str: String = row.get(2)?;
    let to_claim_str: String = row.get(3)?;
    let kind_str: String = row.get(4)?;
    let created_at_str: String = row.get(5)?;

    let edge_id = uuid::Uuid::parse_str(&edge_id_str)
        .map_err(|e| to_err(format!("edge_id UUID: {e}")))?;
    let from_claim = uuid::Uuid::parse_str(&from_claim_str)
        .map(ClaimRef)
        .map_err(|e| to_err(format!("from_claim UUID: {e}")))?;
    let to_claim = uuid::Uuid::parse_str(&to_claim_str)
        .map(ClaimRef)
        .map_err(|e| to_err(format!("to_claim UUID: {e}")))?;
    let kind = str_to_edge_kind(&kind_str)
        .map_err(|e| to_err(e.to_string()))?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| to_err(format!("created_at parse: {e}")))?;

    Ok(ClaimEdge {
        edge_id,
        agent_id: AgentId(agent_id_str),
        from_claim,
        to_claim,
        kind,
        created_at: TransactionTime(created_at),
    })
}

// ── PersistencePort impl ──────────────────────────────────────────────────────

impl PersistencePort for SqlitePersistenceStore {
    type Transaction = SqliteTxn;
    type Error = SqliteStoreError;

    // ── Transaction lifecycle ─────────────────────────────────────────────────

    /// Open an explicit `BEGIN DEFERRED` transaction scoped to `agent_id`.
    ///
    /// The connection is moved into the returned `SqliteTxn`.  Calling `begin_atomic`
    /// again before `commit`/`rollback` returns `SqliteStoreError::TxnAlreadyOpen`.
    fn begin_atomic(&self, agent_id: &AgentId) -> Result<SqliteTxn, SqliteStoreError> {
        let mut slot = self.conn.lock().expect("SqlitePersistenceStore: mutex poisoned");
        let conn = slot.take().ok_or(SqliteStoreError::TxnAlreadyOpen)?;
        SqliteTxn::begin(agent_id.clone(), conn)
    }

    /// Commit the transaction and return the connection to the store.
    fn commit(&self, txn: SqliteTxn) -> Result<(), SqliteStoreError> {
        let conn = txn.commit_and_return()?;
        let mut slot = self.conn.lock().expect("SqlitePersistenceStore: mutex poisoned");
        *slot = Some(conn);
        Ok(())
    }

    /// Rollback the transaction and return the connection to the store.
    /// On rollback all rows appended within the txn are discarded (I9).
    fn rollback(&self, txn: SqliteTxn) -> Result<(), SqliteStoreError> {
        let conn = txn.rollback_and_return()?;
        let mut slot = self.conn.lock().expect("SqlitePersistenceStore: mutex poisoned");
        *slot = Some(conn);
        Ok(())
    }

    // ── Write methods (INSERT-only, I1) ───────────────────────────────────────

    /// Append a claim row within the open transaction.
    ///
    /// Column mapping (§5):
    /// - `claim_id` ← `claim.claim_ref().0` (UUID → TEXT)
    /// - `agent_id` ← `claim.agent_id().0`
    /// - `provenance_label` ← `provenance_to_str(claim.provenance())` (NOT NULL, I2)
    /// - `nearest_external_anchor_id` ← `ExternalAnchor.nearest_external_anchor` (nullable)
    /// - `derived_from` ← JSON array of ClaimRef UUIDs
    fn append_claim(
        &self,
        txn: &mut SqliteTxn,
        claim: &Claim,
    ) -> Result<ClaimRef, SqliteStoreError> {
        let conn = txn.conn();

        let claim_id = claim.claim_ref().0.to_string();
        let agent_id = claim.agent_id().0.as_str();
        let fact = claim.fact();
        let value_json = serde_json::to_string(&fact.value)
            .map_err(|e| SqliteStoreError::Mapping(format!("value serialization: {e}")))?;
        let cardinality = cardinality_to_str(claim.cardinality());
        let provenance = provenance_to_str(claim.provenance());
        let anchor = claim.external_anchor();
        let nearest_anchor: Option<String> =
            anchor.nearest_external_anchor.as_ref().map(|r| r.0.to_string());
        let derivation_depth = anchor.derivation_depth as i64;
        let tx_time = claim.transaction_time().0.to_rfc3339();
        let vt = claim.valid_time();
        let valid_time_start: Option<String> = vt.start.map(|dt| dt.to_rfc3339());
        let valid_time_end: Option<String> = vt.end.map(|dt| dt.to_rfc3339());
        let valid_time_confidence = vt.valid_time_confidence as f64;
        let conf = claim.confidence();
        let value_confidence = conf.value_confidence as f64;
        let criticality = criticality_to_str(claim.criticality());
        let derived_from_refs: Vec<String> =
            claim.derived_from().iter().map(|r| r.0.to_string()).collect();
        let derived_from_json = serde_json::to_string(&derived_from_refs)
            .map_err(|e| SqliteStoreError::Mapping(format!("derived_from serialization: {e}")))?;
        let metadata: Option<String> = claim
            .metadata()
            .map(|v| {
                serde_json::to_string(v)
                    .map_err(|e| SqliteStoreError::Mapping(format!("metadata serialization: {e}")))
            })
            .transpose()?;
        let snapshot_schema_version: Option<i64> =
            claim.snapshot_schema_version().map(|v| v as i64);

        conn.execute(
            "INSERT INTO claims (
                claim_id, agent_id, subject, predicate, value, cardinality,
                provenance_label, nearest_external_anchor_id, derivation_depth,
                tx_time, valid_time_start, valid_time_end, valid_time_confidence,
                value_confidence, criticality, derived_from,
                metadata, snapshot_schema_version, embedding_model_id
            ) VALUES (
                ?1,  ?2,  ?3,  ?4,  ?5,  ?6,
                ?7,  ?8,  ?9,
                ?10, ?11, ?12, ?13,
                ?14, ?15, ?16,
                ?17, ?18, NULL
            )",
            rusqlite::params![
                claim_id,
                agent_id,
                fact.subject.as_str(),
                fact.predicate.as_str(),
                value_json.as_str(),
                cardinality,
                provenance,
                nearest_anchor,
                derivation_depth,
                tx_time.as_str(),
                valid_time_start,
                valid_time_end,
                valid_time_confidence,
                value_confidence,
                criticality,
                derived_from_json.as_str(),
                metadata,
                snapshot_schema_version,
            ],
        )?;

        Ok(claim.claim_ref().clone())
    }

    /// Append a validity assertion row (Bound or Reopen) within the open transaction.
    fn append_validity_assertion(
        &self,
        txn: &mut SqliteTxn,
        assertion: &ValidityAssertion,
    ) -> Result<(), SqliteStoreError> {
        let conn = txn.conn();

        let assertion_id = assertion.assertion_ref.to_string();
        let agent_id = assertion.agent_id.0.as_str();
        let target_claim_id = assertion.target_claim.0.to_string();
        let provenance = provenance_to_str(&assertion.provenance);
        let value_confidence = assertion.confidence.value_confidence as f64;
        let valid_time_confidence = assertion.confidence.valid_time_confidence as f64;
        let asserted_at = assertion.asserted_at.0.to_rfc3339();

        let (assertion_kind, bound_at, reopen_at): (&str, Option<String>, Option<String>) =
            match &assertion.kind {
                AssertionKind::Bound { bound_at } => {
                    ("Bound", Some(bound_at.to_rfc3339()), None)
                }
                AssertionKind::Reopen { reopen_at } => {
                    ("Reopen", None, Some(reopen_at.to_rfc3339()))
                }
            };

        conn.execute(
            "INSERT INTO validity_assertions (
                assertion_id, agent_id, target_claim_id,
                assertion_kind, bound_at, reopen_at,
                provenance_label, value_confidence, valid_time_confidence, asserted_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                assertion_id.as_str(),
                agent_id,
                target_claim_id.as_str(),
                assertion_kind,
                bound_at,
                reopen_at,
                provenance,
                value_confidence,
                valid_time_confidence,
                asserted_at.as_str(),
            ],
        )?;

        Ok(())
    }

    /// Append a ledger entry row within the open transaction.
    fn append_ledger_entry(
        &self,
        txn: &mut SqliteTxn,
        entry: &LedgerEntry,
    ) -> Result<(), SqliteStoreError> {
        let conn = txn.conn();

        let entry_id = entry.entry_id.to_string();
        let agent_id = entry.agent_id.0.as_str();
        let claim_id = entry.claim_ref.0.to_string();
        let event_kind = ledger_event_kind_to_str(&entry.event_kind);
        let disposition = disposition_to_str(&entry.disposition);
        let rationale: Option<String> = entry
            .rationale
            .as_ref()
            .map(|v| {
                serde_json::to_string(v)
                    .map_err(|e| SqliteStoreError::Mapping(format!("rationale serialization: {e}")))
            })
            .transpose()?;
        let recorded_at = entry.recorded_at.0.to_rfc3339();

        conn.execute(
            "INSERT INTO ledger_entries (
                entry_id, agent_id, claim_id, event_kind, disposition, rationale, recorded_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                entry_id.as_str(),
                agent_id,
                claim_id.as_str(),
                event_kind,
                disposition,
                rationale,
                recorded_at.as_str(),
            ],
        )?;

        Ok(())
    }

    /// Append a claim edge row within the open transaction.
    fn append_claim_edge(
        &self,
        txn: &mut SqliteTxn,
        edge: &ClaimEdge,
    ) -> Result<(), SqliteStoreError> {
        let conn = txn.conn();

        let edge_id = edge.edge_id.to_string();
        let agent_id = edge.agent_id.0.as_str();
        let from_claim_id = edge.from_claim.0.to_string();
        let to_claim_id = edge.to_claim.0.to_string();
        let edge_kind = edge_kind_to_str(&edge.kind);
        let created_at = edge.created_at.0.to_rfc3339();

        conn.execute(
            "INSERT INTO claim_edges (
                edge_id, agent_id, from_claim_id, to_claim_id, edge_kind, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                edge_id.as_str(),
                agent_id,
                from_claim_id.as_str(),
                to_claim_id.as_str(),
                edge_kind,
                created_at.as_str(),
            ],
        )?;

        Ok(())
    }

    // ── Read methods (non-mutating; lock connection slot directly) ───────────

    /// Load all claims on the given (agent_id, subject, predicate) subject-line,
    /// ordered by tx_time ASC (oldest first — callers fold in tx_time order).
    ///
    /// Uses `idx_claims_subject_line` covering index (§5).
    fn load_subject_line(
        &self,
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
    ) -> Result<Vec<Claim>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let sql = format!(
            "SELECT {cols} FROM claims
             WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
             ORDER BY tx_time ASC",
            cols = CLAIM_SELECT_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params![agent_id.0.as_str(), subject, predicate],
            row_to_claim,
        )?;

        let mut claims = Vec::new();
        for row in rows {
            claims.push(row?);
        }
        Ok(claims)
    }

    /// Load a single claim by its `ClaimRef`. Returns `None` if not found.
    fn load_claim(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Option<Claim>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let sql = format!(
            "SELECT {cols} FROM claims
             WHERE agent_id = ?1 AND claim_id = ?2",
            cols = CLAIM_SELECT_COLS
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map(
            rusqlite::params![agent_id.0.as_str(), claim_ref.0.to_string()],
            row_to_claim,
        )?;

        match rows.next() {
            None => Ok(None),
            Some(row) => Ok(Some(row?)),
        }
    }

    /// Load all validity assertions targeting a claim, ordered by asserted_at ASC.
    ///
    /// Uses `idx_validity_assertions_target` index (§5).
    fn load_validity_assertions_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ValidityAssertion>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let mut stmt = conn.prepare(
            "SELECT assertion_id, agent_id, target_claim_id,
                    assertion_kind, bound_at, reopen_at,
                    provenance_label, value_confidence, valid_time_confidence, asserted_at
             FROM validity_assertions
             WHERE agent_id = ?1 AND target_claim_id = ?2
             ORDER BY asserted_at ASC",
        )?;

        let to_err = |msg: String| rusqlite::Error::InvalidColumnType(
            0, msg, rusqlite::types::Type::Text,
        );

        let rows = stmt.query_map(
            rusqlite::params![agent_id.0.as_str(), claim_ref.0.to_string()],
            |row| {
                let assertion_id_str: String = row.get(0)?;
                let agent_id_str: String = row.get(1)?;
                let target_claim_str: String = row.get(2)?;
                let kind_str: String = row.get(3)?;
                let bound_at_str: Option<String> = row.get(4)?;
                let reopen_at_str: Option<String> = row.get(5)?;
                let prov_str: String = row.get(6)?;
                let value_confidence: f64 = row.get(7)?;
                let valid_time_confidence: f64 = row.get(8)?;
                let asserted_at_str: String = row.get(9)?;

                let assertion_ref = uuid::Uuid::parse_str(&assertion_id_str)
                    .map_err(|e| to_err(format!("assertion_id UUID: {e}")))?;
                let target_claim = uuid::Uuid::parse_str(&target_claim_str)
                    .map(ClaimRef)
                    .map_err(|e| to_err(format!("target_claim UUID: {e}")))?;
                let provenance = str_to_provenance(&prov_str)
                    .map_err(|e| to_err(e.to_string()))?;
                let asserted_at = chrono::DateTime::parse_from_rfc3339(&asserted_at_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| to_err(format!("asserted_at parse: {e}")))?;

                let kind = match kind_str.as_str() {
                    "Bound" => {
                        let s = bound_at_str.ok_or_else(|| to_err("bound_at is NULL for Bound assertion".into()))?;
                        let dt = chrono::DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .map_err(|e| to_err(format!("bound_at parse: {e}")))?;
                        AssertionKind::Bound { bound_at: dt }
                    }
                    "Reopen" => {
                        let s = reopen_at_str.ok_or_else(|| to_err("reopen_at is NULL for Reopen assertion".into()))?;
                        let dt = chrono::DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .map_err(|e| to_err(format!("reopen_at parse: {e}")))?;
                        AssertionKind::Reopen { reopen_at: dt }
                    }
                    other => return Err(to_err(format!("unknown assertion_kind: {other}"))),
                };

                Ok(ValidityAssertion {
                    assertion_ref,
                    agent_id: AgentId(agent_id_str),
                    target_claim,
                    kind,
                    provenance,
                    confidence: Confidence {
                        value_confidence: value_confidence as f32,
                        valid_time_confidence: valid_time_confidence as f32,
                    },
                    asserted_at: TransactionTime(asserted_at),
                })
            },
        )?;

        let mut assertions = Vec::new();
        for row in rows {
            assertions.push(row?);
        }
        Ok(assertions)
    }

    /// Load ledger entries for an agent, optionally starting from `from` (inclusive),
    /// limited to `limit` rows, ordered by recorded_at ASC.
    ///
    /// Uses `idx_ledger_agent_time` index (§5). `from = None` returns from the beginning.
    fn load_ledger(
        &self,
        agent_id: &AgentId,
        from: Option<&TransactionTime>,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let to_err = |msg: String| rusqlite::Error::InvalidColumnType(
            0, msg, rusqlite::types::Type::Text,
        );

        let from_str: Option<String> = from.map(|t| t.0.to_rfc3339());
        let limit_i64 = limit as i64;

        let map_row = |row: &rusqlite::Row<'_>| {
            let entry_id_str: String = row.get(0)?;
            let agent_id_str: String = row.get(1)?;
            let claim_id_str: String = row.get(2)?;
            let event_kind_str: String = row.get(3)?;
            let disposition_str: String = row.get(4)?;
            let rationale_json: Option<String> = row.get(5)?;
            let recorded_at_str: String = row.get(6)?;

            let entry_id = uuid::Uuid::parse_str(&entry_id_str)
                .map_err(|e| to_err(format!("entry_id UUID: {e}")))?;
            let claim_id = uuid::Uuid::parse_str(&claim_id_str)
                .map(ClaimRef)
                .map_err(|e| to_err(format!("claim_id UUID: {e}")))?;
            let event_kind = str_to_ledger_event_kind(&event_kind_str)
                .map_err(|e| to_err(e.to_string()))?;
            let disposition = str_to_disposition(&disposition_str)
                .map_err(|e| to_err(e.to_string()))?;
            let rationale: Option<serde_json::Value> = rationale_json
                .map(|s| serde_json::from_str(&s).map_err(|e| to_err(format!("rationale JSON: {e}"))))
                .transpose()?;
            let recorded_at = chrono::DateTime::parse_from_rfc3339(&recorded_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| to_err(format!("recorded_at parse: {e}")))?;

            Ok(LedgerEntry {
                entry_id,
                agent_id: AgentId(agent_id_str),
                claim_ref: claim_id,
                event_kind,
                disposition,
                rationale,
                recorded_at: TransactionTime(recorded_at),
            })
        };

        let mut entries = Vec::new();

        if let Some(ref from_val) = from_str {
            let mut stmt = conn.prepare(
                "SELECT entry_id, agent_id, claim_id, event_kind, disposition, rationale, recorded_at
                 FROM ledger_entries
                 WHERE agent_id = ?1 AND recorded_at >= ?2
                 ORDER BY recorded_at ASC
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![agent_id.0.as_str(), from_val.as_str(), limit_i64],
                map_row,
            )?;
            for row in rows {
                entries.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT entry_id, agent_id, claim_id, event_kind, disposition, rationale, recorded_at
                 FROM ledger_entries
                 WHERE agent_id = ?1
                 ORDER BY recorded_at ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![agent_id.0.as_str(), limit_i64],
                map_row,
            )?;
            for row in rows {
                entries.push(row?);
            }
        }

        Ok(entries)
    }

    /// Load all edges where `claim_ref` is either the from or to end, for this agent.
    /// Ordered by `created_at ASC` (deterministic cascade — required by convention).
    ///
    /// Uses `idx_edges_from` and `idx_edges_to` indexes (§5).
    fn load_edges_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let claim_id_str = claim_ref.0.to_string();

        let mut stmt = conn.prepare(
            "SELECT edge_id, agent_id, from_claim_id, to_claim_id, edge_kind, created_at
             FROM claim_edges
             WHERE agent_id = ?1
               AND (from_claim_id = ?2 OR to_claim_id = ?2)
             ORDER BY created_at ASC",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![agent_id.0.as_str(), claim_id_str.as_str()],
            row_to_edge,
        )?;

        let mut edges = Vec::new();
        for row in rows {
            edges.push(row?);
        }
        Ok(edges)
    }

    /// Load the set of ClaimRefs served as injected claims for this agent (C6, F3).
    ///
    /// Scans `ledger_entries` for `event_kind = 'ServedAsInjected'` and returns
    /// the distinct set of claim IDs, ordered by recorded_at ASC.
    fn load_injected_claims(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<ClaimRef>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let to_err = |msg: String| rusqlite::Error::InvalidColumnType(
            0, msg, rusqlite::types::Type::Text,
        );

        let mut stmt = conn.prepare(
            "SELECT claim_id
             FROM ledger_entries
             WHERE agent_id = ?1 AND event_kind = 'ServedAsInjected'
             GROUP BY claim_id
             ORDER BY MIN(recorded_at) ASC",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![agent_id.0.as_str()],
            |row| {
                let claim_id_str: String = row.get(0)?;
                uuid::Uuid::parse_str(&claim_id_str)
                    .map(ClaimRef)
                    .map_err(|e| to_err(format!("claim_id UUID: {e}")))
            },
        )?;

        let mut refs = Vec::new();
        for row in rows {
            refs.push(row?);
        }
        Ok(refs)
    }

    /// Recursive CTE lineage traversal (DB_REQUIREMENTS §1, §5).
    ///
    /// Traverses `DerivedFrom` edges upward (from `claim_ref` to its ancestors),
    /// returning all `ClaimEdge` rows in the lineage sub-graph, ordered by depth
    /// (shallowest first, then by `created_at ASC` within the same depth level).
    ///
    /// The CTE is bounded by `max_depth = 64` to prevent runaway on pathological graphs.
    fn load_lineage(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, SqliteStoreError> {
        let slot = self.conn.lock().expect("mutex poisoned");
        let conn = slot.as_ref().ok_or(SqliteStoreError::TxnAlreadyOpen)?;

        let start_id = claim_ref.0.to_string();

        // Recursive CTE: start from claim_ref and follow DerivedFrom edges upward.
        // Each step follows edges where the current node is the `from_claim_id`
        // (meaning: this claim was DerivedFrom to_claim_id, so ancestor is to_claim_id).
        let mut stmt = conn.prepare(
            "WITH RECURSIVE lineage(edge_id, depth) AS (
                -- Base case: all DerivedFrom edges leaving from our starting claim
                SELECT ce.edge_id, 1
                FROM claim_edges ce
                WHERE ce.agent_id = ?1
                  AND ce.from_claim_id = ?2
                  AND ce.edge_kind = 'DerivedFrom'
                UNION ALL
                -- Recursive case: follow the to_claim of the previous edge onward
                SELECT ce2.edge_id, l.depth + 1
                FROM claim_edges ce2
                JOIN lineage l ON ce2.from_claim_id = (
                    SELECT to_claim_id FROM claim_edges WHERE edge_id = l.edge_id
                )
                WHERE ce2.agent_id = ?1
                  AND ce2.edge_kind = 'DerivedFrom'
                  AND l.depth < 64
            )
            SELECT ce.edge_id, ce.agent_id, ce.from_claim_id, ce.to_claim_id,
                   ce.edge_kind, ce.created_at,
                   l.depth
            FROM claim_edges ce
            JOIN lineage l ON ce.edge_id = l.edge_id
            ORDER BY l.depth ASC, ce.created_at ASC",
        )?;

        let to_err = |msg: String| rusqlite::Error::InvalidColumnType(
            0, msg, rusqlite::types::Type::Text,
        );

        let rows = stmt.query_map(
            rusqlite::params![agent_id.0.as_str(), start_id.as_str()],
            |row| {
                let edge_id_str: String = row.get(0)?;
                let agent_id_str: String = row.get(1)?;
                let from_claim_str: String = row.get(2)?;
                let to_claim_str: String = row.get(3)?;
                let kind_str: String = row.get(4)?;
                let created_at_str: String = row.get(5)?;
                // col 6 = depth (used only for ordering; not part of ClaimEdge)

                let edge_id = uuid::Uuid::parse_str(&edge_id_str)
                    .map_err(|e| to_err(format!("edge_id UUID: {e}")))?;
                let from_claim = uuid::Uuid::parse_str(&from_claim_str)
                    .map(ClaimRef)
                    .map_err(|e| to_err(format!("from_claim UUID: {e}")))?;
                let to_claim = uuid::Uuid::parse_str(&to_claim_str)
                    .map(ClaimRef)
                    .map_err(|e| to_err(format!("to_claim UUID: {e}")))?;
                let kind = str_to_edge_kind(&kind_str)
                    .map_err(|e| to_err(e.to_string()))?;
                let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| to_err(format!("created_at parse: {e}")))?;

                Ok(ClaimEdge {
                    edge_id,
                    agent_id: AgentId(agent_id_str),
                    from_claim,
                    to_claim,
                    kind,
                    created_at: TransactionTime(created_at),
                })
            },
        )?;

        let mut edges = Vec::new();
        for row in rows {
            edges.push(row?);
        }
        Ok(edges)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use chrono::Utc;
    use mempill_types::{
        claim::{Cardinality, Claim, Confidence, Criticality, Fact},
        disposition::Disposition,
        identity::AgentId,
        ledger::LedgerEventKind,
        provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
        time::{TransactionTime, ValidTime},
        validity::AssertionKind,
    };
    use uuid::Uuid;

    fn make_store() -> SqlitePersistenceStore {
        let conn = open_in_memory().expect("in-memory connection should open");
        SqlitePersistenceStore::new(conn)
    }

    fn make_agent() -> AgentId {
        AgentId("test-agent-1".into())
    }

    fn make_claim(agent_id: &AgentId) -> Claim {
        Claim::new(
            ClaimRef(Uuid::new_v4()),
            agent_id.clone(),
            Fact {
                subject: "user".into(),
                predicate: "favourite_colour".into(),
                value: serde_json::json!("blue"),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        )
    }

    fn make_ledger_entry(
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> LedgerEntry {
        LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent_id.clone(),
            claim_ref: claim_ref.clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(Utc::now()),
        }
    }

    // ── WRITE ROUND-TRIP ──────────────────────────────────────────────────────

    /// Append a claim within a Txn, commit, then verify the row exists via raw SELECT.
    /// (Typed read path is W6; we use raw SQL here.)
    #[test]
    fn write_round_trip_claim_persists_after_commit() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_id = claim.claim_ref().0.to_string();

        let mut txn = store.begin_atomic(&agent).expect("begin_atomic should succeed");
        store.append_claim(&mut txn, &claim).expect("append_claim should succeed");
        store.commit(txn).expect("commit should succeed");

        // Re-acquire the connection to verify via raw SQL.
        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().expect("connection must be back after commit");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM claims WHERE claim_id = ?1",
                [claim_id.as_str()],
                |r| r.get(0),
            )
            .expect("SELECT should succeed");
        assert_eq!(count, 1, "claim row must exist after commit");
    }

    /// Append a claim and verify all provenance columns are stored correctly (I2).
    #[test]
    fn write_round_trip_provenance_not_null() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_id = claim.claim_ref().0.to_string();

        let mut txn = store.begin_atomic(&agent).expect("begin_atomic should succeed");
        store.append_claim(&mut txn, &claim).expect("append_claim should succeed");
        store.commit(txn).expect("commit should succeed");

        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().unwrap();

        // provenance_label must be non-NULL (I2 — NOT NULL constraint in schema).
        let prov: String = conn
            .query_row(
                "SELECT provenance_label FROM claims WHERE claim_id = ?1",
                [claim_id.as_str()],
                |r| r.get(0),
            )
            .expect("provenance_label must be selectable");
        assert_eq!(
            prov, "External_UserAsserted",
            "provenance_label column must be non-NULL and correct"
        );

        // tx_time must be non-NULL (I2).
        let tx_time: String = conn
            .query_row(
                "SELECT tx_time FROM claims WHERE claim_id = ?1",
                [claim_id.as_str()],
                |r| r.get(0),
            )
            .expect("tx_time must be selectable");
        assert!(!tx_time.is_empty(), "tx_time must be non-NULL");
    }

    // ── ATOMICITY (I9) ────────────────────────────────────────────────────────

    /// Begin a Txn, append {claim + validity assertion + ledger entry}, force rollback.
    /// All three rows must be absent after rollback — all-or-nothing (I9).
    #[test]
    fn atomicity_rollback_leaves_zero_rows() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();
        let claim_id = claim_ref.0.to_string();

        let assertion = mempill_types::validity::ValidityAssertion {
            assertion_ref: Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at: Utc::now() },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: mempill_types::claim::Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.9,
            },
            asserted_at: TransactionTime(Utc::now()),
        };
        let assertion_id = assertion.assertion_ref.to_string();

        let ledger_entry = make_ledger_entry(&agent, &claim_ref);
        let entry_id = ledger_entry.entry_id.to_string();

        let mut txn = store.begin_atomic(&agent).expect("begin_atomic should succeed");
        store.append_claim(&mut txn, &claim).expect("append_claim in txn should succeed");
        store
            .append_validity_assertion(&mut txn, &assertion)
            .expect("append_validity_assertion in txn should succeed");
        store
            .append_ledger_entry(&mut txn, &ledger_entry)
            .expect("append_ledger_entry in txn should succeed");

        // Force rollback — must leave zero rows.
        store.rollback(txn).expect("rollback should succeed");

        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().expect("connection must be back after rollback");

        let claim_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM claims WHERE claim_id = ?1",
                [claim_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let assertion_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM validity_assertions WHERE assertion_id = ?1",
                [assertion_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let ledger_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ledger_entries WHERE entry_id = ?1",
                [entry_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(claim_count, 0, "claim row must not exist after rollback");
        assert_eq!(assertion_count, 0, "validity_assertion row must not exist after rollback");
        assert_eq!(ledger_count, 0, "ledger_entry row must not exist after rollback");
    }

    // ── VALIDITY ASSERTION ROUND-TRIP ─────────────────────────────────────────

    #[test]
    fn write_round_trip_validity_assertion() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let assertion = mempill_types::validity::ValidityAssertion {
            assertion_ref: Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at: Utc::now() },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: mempill_types::claim::Confidence {
                value_confidence: 0.95,
                valid_time_confidence: 0.8,
            },
            asserted_at: TransactionTime(Utc::now()),
        };
        let assertion_id = assertion.assertion_ref.to_string();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.append_validity_assertion(&mut txn, &assertion).unwrap();
        store.commit(txn).unwrap();

        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM validity_assertions WHERE assertion_id = ?1",
                [assertion_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "validity_assertion row must exist after commit");
    }

    // ── LEDGER ENTRY ROUND-TRIP ───────────────────────────────────────────────

    #[test]
    fn write_round_trip_ledger_entry() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();
        let entry = make_ledger_entry(&agent, &claim_ref);
        let entry_id = entry.entry_id.to_string();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.append_ledger_entry(&mut txn, &entry).unwrap();
        store.commit(txn).unwrap();

        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ledger_entries WHERE entry_id = ?1",
                [entry_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "ledger_entry row must exist after commit");
    }

    // ── CLAIM EDGE ROUND-TRIP ─────────────────────────────────────────────────

    #[test]
    fn write_round_trip_claim_edge() {
        let store = make_store();
        let agent = make_agent();
        let from_claim = make_claim(&agent);
        let to_claim = make_claim(&agent);
        let from_ref = from_claim.claim_ref().clone();
        let to_ref = to_claim.claim_ref().clone();

        let edge = ClaimEdge {
            edge_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: from_ref.clone(),
            to_claim: to_ref.clone(),
            kind: EdgeKind::DerivedFrom,
            created_at: TransactionTime(Utc::now()),
        };
        let edge_id = edge.edge_id.to_string();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &from_claim).unwrap();
        store.append_claim(&mut txn, &to_claim).unwrap();
        store.append_claim_edge(&mut txn, &edge).unwrap();
        store.commit(txn).unwrap();

        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM claim_edges WHERE edge_id = ?1",
                [edge_id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "claim_edge row must exist after commit");
    }

    // ── READ PATH TESTS (W6) ──────────────────────────────────────────────────

    /// Write a claim then load_claim returns it with all fields intact (round-trip).
    #[test]
    fn read_load_claim_round_trip() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.commit(txn).unwrap();

        let loaded = store.load_claim(&agent, &claim_ref).unwrap();
        assert!(loaded.is_some(), "load_claim must return Some for existing claim");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.claim_ref(), &claim_ref);
        assert_eq!(loaded.agent_id(), &agent);
        assert_eq!(loaded.fact().subject, "user");
        assert_eq!(loaded.fact().predicate, "favourite_colour");
        assert_eq!(loaded.fact().value, serde_json::json!("blue"));
        assert_eq!(loaded.provenance(), claim.provenance());
        assert_eq!(loaded.cardinality(), claim.cardinality());
        assert_eq!(loaded.criticality(), claim.criticality());
    }

    /// load_claim returns None for a non-existent ClaimRef.
    #[test]
    fn read_load_claim_missing_returns_none() {
        let store = make_store();
        let agent = make_agent();
        let missing_ref = ClaimRef(Uuid::new_v4());
        let result = store.load_claim(&agent, &missing_ref).unwrap();
        assert!(result.is_none(), "load_claim must return None for unknown claim_ref");
    }

    /// Write a claim then load_subject_line returns it.
    #[test]
    fn read_load_subject_line_round_trip() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.commit(txn).unwrap();

        let claims = store.load_subject_line(&agent, "user", "favourite_colour").unwrap();
        assert_eq!(claims.len(), 1, "load_subject_line must return the single written claim");
        assert_eq!(claims[0].claim_ref(), &claim_ref);
    }

    /// load_subject_line returns empty vec when nothing matches.
    #[test]
    fn read_load_subject_line_empty_when_no_match() {
        let store = make_store();
        let agent = make_agent();
        let claims = store.load_subject_line(&agent, "nonexistent", "pred").unwrap();
        assert!(claims.is_empty(), "load_subject_line must return empty vec for unknown subject-line");
    }

    /// Write a validity assertion then load_validity_assertions_for returns it.
    #[test]
    fn read_load_validity_assertions_round_trip() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let assertion = mempill_types::validity::ValidityAssertion {
            assertion_ref: Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at: Utc::now() },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: mempill_types::claim::Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.8,
            },
            asserted_at: TransactionTime(Utc::now()),
        };

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.append_validity_assertion(&mut txn, &assertion).unwrap();
        store.commit(txn).unwrap();

        let loaded = store.load_validity_assertions_for(&agent, &claim_ref).unwrap();
        assert_eq!(loaded.len(), 1, "must return one validity assertion");
        assert_eq!(loaded[0].assertion_ref, assertion.assertion_ref);
        assert_eq!(loaded[0].target_claim, claim_ref);
        assert!(matches!(loaded[0].kind, AssertionKind::Bound { .. }));
    }

    /// load_validity_assertions_for returns empty when no assertions exist.
    #[test]
    fn read_load_validity_assertions_empty_when_none() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.commit(txn).unwrap();

        let loaded = store.load_validity_assertions_for(&agent, &claim_ref).unwrap();
        assert!(loaded.is_empty(), "must return empty vec when no assertions");
    }

    /// Write a ledger entry and load_ledger returns it.
    #[test]
    fn read_load_ledger_round_trip() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();
        let entry = make_ledger_entry(&agent, &claim_ref);

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.append_ledger_entry(&mut txn, &entry).unwrap();
        store.commit(txn).unwrap();

        let loaded = store.load_ledger(&agent, None, 100).unwrap();
        assert_eq!(loaded.len(), 1, "must return one ledger entry");
        assert_eq!(loaded[0].entry_id, entry.entry_id);
        assert_eq!(loaded[0].claim_ref, claim_ref);
        assert_eq!(loaded[0].event_kind, LedgerEventKind::ClaimCommitted);
    }

    /// load_ledger respects the `from` bound — entries before `from` are excluded.
    #[test]
    fn read_load_ledger_from_bound_filters_earlier_entries() {
        let store = make_store();
        let agent = make_agent();

        // Two claims: early and late
        let claim_early = make_claim(&agent);
        let claim_late = make_claim(&agent);
        let ref_early = claim_early.claim_ref().clone();
        let ref_late = claim_late.claim_ref().clone();

        let t_early = TransactionTime(Utc::now() - chrono::Duration::seconds(10));
        let t_late = TransactionTime(Utc::now());

        let entry_early = mempill_types::ledger::LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref_early.clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: mempill_types::disposition::Disposition::CommittedCheap,
            rationale: None,
            recorded_at: t_early.clone(),
        };
        let entry_late = mempill_types::ledger::LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref_late.clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: mempill_types::disposition::Disposition::CommittedCheap,
            rationale: None,
            recorded_at: t_late.clone(),
        };

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim_early).unwrap();
        store.append_claim(&mut txn, &claim_late).unwrap();
        store.append_ledger_entry(&mut txn, &entry_early).unwrap();
        store.append_ledger_entry(&mut txn, &entry_late).unwrap();
        store.commit(txn).unwrap();

        // Load from t_late — should only see the late entry
        let loaded = store.load_ledger(&agent, Some(&t_late), 100).unwrap();
        assert_eq!(loaded.len(), 1, "only the late entry must be returned when from=t_late");
        assert_eq!(loaded[0].entry_id, entry_late.entry_id);
    }

    /// load_ledger returns empty when agent has no entries.
    #[test]
    fn read_load_ledger_empty_when_none() {
        let store = make_store();
        let agent = make_agent();
        let loaded = store.load_ledger(&agent, None, 100).unwrap();
        assert!(loaded.is_empty(), "must return empty vec when no ledger entries");
    }

    /// load_edges_for returns edges and they are ordered by created_at ASC (deterministic).
    #[test]
    fn read_load_edges_for_ordering_created_at_asc() {
        let store = make_store();
        let agent = make_agent();

        let claim_a = make_claim(&agent);
        let claim_b = make_claim(&agent);
        let claim_c = make_claim(&agent);
        let ref_a = claim_a.claim_ref().clone();
        let ref_b = claim_b.claim_ref().clone();
        let ref_c = claim_c.claim_ref().clone();

        // Edge A→B created first, A→C created second (microsecond gap guaranteed by sleep or offset)
        let t1 = TransactionTime(Utc::now() - chrono::Duration::seconds(5));
        let t2 = TransactionTime(Utc::now());

        let edge_ab = ClaimEdge {
            edge_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: ref_a.clone(),
            to_claim: ref_b.clone(),
            kind: EdgeKind::DependsOn,
            created_at: t1,
        };
        let edge_ac = ClaimEdge {
            edge_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: ref_a.clone(),
            to_claim: ref_c.clone(),
            kind: EdgeKind::DependsOn,
            created_at: t2,
        };

        let mut txn = store.begin_atomic(&agent).unwrap();
        // Insert in reverse order to prove ORDER BY drives the result
        store.append_claim(&mut txn, &claim_a).unwrap();
        store.append_claim(&mut txn, &claim_b).unwrap();
        store.append_claim(&mut txn, &claim_c).unwrap();
        store.append_claim_edge(&mut txn, &edge_ac).unwrap(); // insert late edge first
        store.append_claim_edge(&mut txn, &edge_ab).unwrap(); // insert early edge second
        store.commit(txn).unwrap();

        let loaded = store.load_edges_for(&agent, &ref_a).unwrap();
        assert_eq!(loaded.len(), 2, "must return both edges");
        // Verify ASC ordering: AB (earlier created_at) must come before AC
        assert_eq!(loaded[0].to_claim, ref_b, "earlier edge (A→B) must be first");
        assert_eq!(loaded[1].to_claim, ref_c, "later edge (A→C) must be second");
    }

    /// load_edges_for returns empty when no edges exist for the claim.
    #[test]
    fn read_load_edges_for_empty_when_none() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.commit(txn).unwrap();

        let loaded = store.load_edges_for(&agent, &claim_ref).unwrap();
        assert!(loaded.is_empty(), "must return empty vec when no edges");
    }

    /// load_injected_claims returns ClaimRefs from ServedAsInjected ledger entries.
    #[test]
    fn read_load_injected_claims_round_trip() {
        use mempill_types::disposition::Disposition;

        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let injected_entry = mempill_types::ledger::LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: claim_ref.clone(),
            event_kind: LedgerEventKind::ServedAsInjected,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(Utc::now()),
        };

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.append_ledger_entry(&mut txn, &injected_entry).unwrap();
        store.commit(txn).unwrap();

        let loaded = store.load_injected_claims(&agent).unwrap();
        assert_eq!(loaded.len(), 1, "must return one injected claim ref");
        assert_eq!(loaded[0], claim_ref);
    }

    /// load_injected_claims returns empty when no ServedAsInjected entries exist.
    #[test]
    fn read_load_injected_claims_empty_when_none() {
        let store = make_store();
        let agent = make_agent();
        let loaded = store.load_injected_claims(&agent).unwrap();
        assert!(loaded.is_empty(), "must return empty vec when no injected claims");
    }

    /// LINEAGE CTE: multi-hop A→B→C chain is fully traversed.
    #[test]
    fn read_load_lineage_multi_hop_derived_from() {
        let store = make_store();
        let agent = make_agent();

        // A is derived from B; B is derived from C.
        // load_lineage(A) must return edges: A→B and B→C (full chain).
        let claim_a = make_claim(&agent);
        let claim_b = make_claim(&agent);
        let claim_c = make_claim(&agent);
        let ref_a = claim_a.claim_ref().clone();
        let ref_b = claim_b.claim_ref().clone();
        let ref_c = claim_c.claim_ref().clone();

        let edge_ab = ClaimEdge {
            edge_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: ref_a.clone(),
            to_claim: ref_b.clone(),
            kind: EdgeKind::DerivedFrom,
            created_at: TransactionTime(Utc::now() - chrono::Duration::seconds(2)),
        };
        let edge_bc = ClaimEdge {
            edge_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: ref_b.clone(),
            to_claim: ref_c.clone(),
            kind: EdgeKind::DerivedFrom,
            created_at: TransactionTime(Utc::now() - chrono::Duration::seconds(1)),
        };

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim_a).unwrap();
        store.append_claim(&mut txn, &claim_b).unwrap();
        store.append_claim(&mut txn, &claim_c).unwrap();
        store.append_claim_edge(&mut txn, &edge_ab).unwrap();
        store.append_claim_edge(&mut txn, &edge_bc).unwrap();
        store.commit(txn).unwrap();

        let lineage = store.load_lineage(&agent, &ref_a).unwrap();
        assert_eq!(lineage.len(), 2, "lineage must contain both DerivedFrom hops A→B and B→C");

        // Shallowest (depth 1) first: A→B edge
        assert_eq!(lineage[0].from_claim, ref_a, "first edge must start from A");
        assert_eq!(lineage[0].to_claim, ref_b, "first edge must point to B");
        // Deeper (depth 2): B→C edge
        assert_eq!(lineage[1].from_claim, ref_b, "second edge must start from B");
        assert_eq!(lineage[1].to_claim, ref_c, "second edge must point to C");
    }

    /// load_lineage returns empty when the claim has no DerivedFrom edges.
    #[test]
    fn read_load_lineage_empty_when_no_derived_from_edges() {
        let store = make_store();
        let agent = make_agent();
        let claim = make_claim(&agent);
        let claim_ref = claim.claim_ref().clone();

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim).unwrap();
        store.commit(txn).unwrap();

        let lineage = store.load_lineage(&agent, &claim_ref).unwrap();
        assert!(lineage.is_empty(), "load_lineage must return empty vec when no DerivedFrom edges");
    }

    // ── TXN ALREADY OPEN guard ────────────────────────────────────────────────

    #[test]
    fn begin_atomic_while_txn_open_returns_error() {
        let store = make_store();
        let agent = make_agent();

        let _txn = store.begin_atomic(&agent).expect("first begin_atomic should succeed");
        let result = store.begin_atomic(&agent);
        assert!(
            matches!(result, Err(SqliteStoreError::TxnAlreadyOpen)),
            "second begin_atomic must return TxnAlreadyOpen"
        );
    }

    // ── FULL ATOMIC UNIT (I9 positive path) ───────────────────────────────────

    /// Append {claim + validity assertion + ledger entry + edge} and commit.
    /// All four rows must land atomically.
    #[test]
    fn atomic_unit_all_four_rows_on_commit() {
        let store = make_store();
        let agent = make_agent();
        let claim_a = make_claim(&agent);
        let claim_b = make_claim(&agent);
        let claim_ref_a = claim_a.claim_ref().clone();
        let claim_ref_b = claim_b.claim_ref().clone();

        let assertion = mempill_types::validity::ValidityAssertion {
            assertion_ref: Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref_a.clone(),
            kind: AssertionKind::Bound { bound_at: Utc::now() },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: mempill_types::claim::Confidence {
                value_confidence: 0.9,
                valid_time_confidence: 0.9,
            },
            asserted_at: TransactionTime(Utc::now()),
        };
        let ledger = make_ledger_entry(&agent, &claim_ref_a);
        let edge = ClaimEdge {
            edge_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            from_claim: claim_ref_a.clone(),
            to_claim: claim_ref_b.clone(),
            kind: EdgeKind::Supersedes,
            created_at: TransactionTime(Utc::now()),
        };

        let mut txn = store.begin_atomic(&agent).unwrap();
        store.append_claim(&mut txn, &claim_a).unwrap();
        store.append_claim(&mut txn, &claim_b).unwrap();
        store.append_validity_assertion(&mut txn, &assertion).unwrap();
        store.append_ledger_entry(&mut txn, &ledger).unwrap();
        store.append_claim_edge(&mut txn, &edge).unwrap();
        store.commit(txn).unwrap();

        let slot = store.conn.lock().unwrap();
        let conn = slot.as_ref().unwrap();

        let claims: i64 = conn
            .query_row("SELECT COUNT(*) FROM claims", [], |r| r.get(0))
            .unwrap();
        let assertions: i64 = conn
            .query_row("SELECT COUNT(*) FROM validity_assertions", [], |r| r.get(0))
            .unwrap();
        let ledger_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ledger_entries", [], |r| r.get(0))
            .unwrap();
        let edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM claim_edges", [], |r| r.get(0))
            .unwrap();

        assert_eq!(claims, 2, "two claim rows must exist");
        assert_eq!(assertions, 1, "one validity_assertion row must exist");
        assert_eq!(ledger_count, 1, "one ledger_entry row must exist");
        assert_eq!(edges, 1, "one claim_edge row must exist");
    }
}
