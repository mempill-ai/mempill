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
    claim::{Cardinality, Claim, Criticality},
    edge::{ClaimEdge, EdgeKind},
    identity::{AgentId, ClaimRef},
    ledger::{LedgerEntry, LedgerEventKind},
    provenance::{ExternalKind, ProvenanceLabel},
    time::TransactionTime,
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
/// Used by the W6 read path.
#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

    // ── Read methods (W6 — read path not implemented in this wave) ────────────

    fn load_subject_line(
        &self,
        _agent_id: &AgentId,
        _subject: &str,
        _predicate: &str,
    ) -> Result<Vec<Claim>, SqliteStoreError> {
        // W6 — read path: canonical valid-time fold implemented in Wave 6.
        todo!("W6 — load_subject_line: canonical fold + subject-line query not yet implemented")
    }

    fn load_claim(
        &self,
        _agent_id: &AgentId,
        _claim_ref: &ClaimRef,
    ) -> Result<Option<Claim>, SqliteStoreError> {
        // W6 — read path.
        todo!("W6 — load_claim: single-claim lookup not yet implemented")
    }

    fn load_validity_assertions_for(
        &self,
        _agent_id: &AgentId,
        _claim_ref: &ClaimRef,
    ) -> Result<Vec<ValidityAssertion>, SqliteStoreError> {
        // W6 — read path.
        todo!("W6 — load_validity_assertions_for: not yet implemented")
    }

    fn load_ledger(
        &self,
        _agent_id: &AgentId,
        _from: Option<&TransactionTime>,
        _limit: usize,
    ) -> Result<Vec<LedgerEntry>, SqliteStoreError> {
        // W6 — read path.
        todo!("W6 — load_ledger: not yet implemented")
    }

    fn load_edges_for(
        &self,
        _agent_id: &AgentId,
        _claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, SqliteStoreError> {
        // W6 — read path.
        todo!("W6 — load_edges_for: not yet implemented")
    }

    fn load_injected_claims(
        &self,
        _agent_id: &AgentId,
    ) -> Result<Vec<ClaimRef>, SqliteStoreError> {
        // W6 — read path (C6 entailment check, F3).
        todo!("W6 — load_injected_claims: not yet implemented")
    }

    fn load_lineage(
        &self,
        _agent_id: &AgentId,
        _claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, SqliteStoreError> {
        // W6 — read path: recursive CTE lineage traversal (DB_REQUIREMENTS §1).
        todo!("W6 — load_lineage: recursive CTE not yet implemented")
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
