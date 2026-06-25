//! `PostgresPersistenceStore` — impl of `PersistencePort` for mempill-postgres.
//!
//! # Append-only
//!
//! Every write method is an INSERT. No UPDATE or DELETE paths exist in this file.
//!
//! # Atomic commit unit
//!
//! `begin_atomic` acquires a pooled connection and opens a `BEGIN` transaction.
//! `commit`/`rollback` close the transaction; the connection returns to the r2d2 pool.
//!
//! # JSONB handling
//!
//! `value` and `metadata` are JSONB columns in Postgres (TEXT in SQLite).
//! On INSERT: serialized to JSON string, cast with `$n::jsonb` in SQL.
//! On SELECT: cast back to `::text` in `CLAIM_SELECT_COLS` → `serde_json::from_str`.
//! This confines the JSONB divergence to the INSERT SQL; all row mapping code is identical
//! to the SQLite path.
//!
//! # stream_seq (monotone per-agent sequence number)
//!
//! `append_ledger_entry` assigns `stream_seq` via:
//! `SELECT COALESCE(MAX(stream_seq), 0) + 1 FROM ledger_entries WHERE agent_id = $1`
//! within the same transaction, under the advisory lock.
//! INVARIANT: safe only under `pg_advisory_xact_lock`; replace with a Postgres SEQUENCE
//! if the advisory lock is ever removed.

use std::sync::Arc;

use mempill_core::{
    ports::pending_adjudication::{PendingAdjudicationPort, PendingAdjudicationRow},
    ports::persistence::PersistencePort,
    EngineConfig, EngineHandle,
};
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    disposition::Disposition,
    edge::{ClaimEdge, EdgeKind},
    identity::{AgentId, ClaimRef},
    ledger::{LedgerEntry, LedgerEventKind},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
    validity::{AssertionKind, ValidityAssertion},
};

use crate::{
    connection::{PostgresPersistenceStore, PostgresStoreError},
    txn::PostgresTxn,
};

// ── Domain-type ↔ column mapping helpers (mirrors mempill-sqlite/src/store.rs) ──

fn provenance_to_str(p: &ProvenanceLabel) -> &'static str {
    match p {
        ProvenanceLabel::ModelDerived => "ModelDerived",
        ProvenanceLabel::RecallReEntry => "RecallReEntry",
        ProvenanceLabel::External(ExternalKind::UserAsserted) => "External_UserAsserted",
        ProvenanceLabel::External(ExternalKind::ExternalFirstHand) => "External_ExternalFirstHand",
        _ => "Unknown",
    }
}

fn str_to_provenance(s: &str) -> Result<ProvenanceLabel, PostgresStoreError> {
    match s {
        "ModelDerived" => Ok(ProvenanceLabel::ModelDerived),
        "RecallReEntry" => Ok(ProvenanceLabel::RecallReEntry),
        "External_UserAsserted" => Ok(ProvenanceLabel::External(ExternalKind::UserAsserted)),
        "External_ExternalFirstHand" => Ok(ProvenanceLabel::External(ExternalKind::ExternalFirstHand)),
        other => Err(PostgresStoreError::Mapping(format!("unknown provenance_label: {other}"))),
    }
}

fn cardinality_to_str(c: &Cardinality) -> &'static str {
    match c {
        Cardinality::Functional => "Functional",
        Cardinality::SetValued => "SetValued",
        Cardinality::Unknown => "Unknown",
    }
}

fn str_to_cardinality(s: &str) -> Result<Cardinality, PostgresStoreError> {
    match s {
        "Functional" => Ok(Cardinality::Functional),
        "SetValued" => Ok(Cardinality::SetValued),
        "Unknown" => Ok(Cardinality::Unknown),
        other => Err(PostgresStoreError::Mapping(format!("unknown cardinality: {other}"))),
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

fn str_to_criticality(s: &str) -> Result<Criticality, PostgresStoreError> {
    match s {
        "Low" => Ok(Criticality::Low),
        "Medium" => Ok(Criticality::Medium),
        "High" => Ok(Criticality::High),
        "Critical" => Ok(Criticality::Critical),
        other => Err(PostgresStoreError::Mapping(format!("unknown criticality: {other}"))),
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

fn str_to_edge_kind(s: &str) -> Result<EdgeKind, PostgresStoreError> {
    match s {
        "DerivedFrom" => Ok(EdgeKind::DerivedFrom),
        "Supersedes" => Ok(EdgeKind::Supersedes),
        "DependsOn" => Ok(EdgeKind::DependsOn),
        "MutualExclusion" => Ok(EdgeKind::MutualExclusion),
        other => Err(PostgresStoreError::Mapping(format!("unknown edge_kind: {other}"))),
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
        LedgerEventKind::AdjudicationExpired => "AdjudicationExpired",
    }
}

fn str_to_ledger_event_kind(s: &str) -> Result<LedgerEventKind, PostgresStoreError> {
    match s {
        "ClaimCommitted" => Ok(LedgerEventKind::ClaimCommitted),
        "ValidityAsserted" => Ok(LedgerEventKind::ValidityAsserted),
        "AdjudicationRequested" => Ok(LedgerEventKind::AdjudicationRequested),
        "AdjudicationResolved" => Ok(LedgerEventKind::AdjudicationResolved),
        "RecallReEntryDetected" => Ok(LedgerEventKind::RecallReEntryDetected),
        "Quarantined" => Ok(LedgerEventKind::Quarantined),
        "DependentFlaggedPendingReview" => Ok(LedgerEventKind::DependentFlaggedPendingReview),
        "ServedAsInjected" => Ok(LedgerEventKind::ServedAsInjected),
        "AdjudicationExpired" => Ok(LedgerEventKind::AdjudicationExpired),
        other => Err(PostgresStoreError::Mapping(format!("unknown ledger event_kind: {other}"))),
    }
}

fn disposition_to_str(d: &Disposition) -> &'static str {
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

fn str_to_disposition(s: &str) -> Result<Disposition, PostgresStoreError> {
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
        other => Err(PostgresStoreError::Mapping(format!("unknown disposition: {other}"))),
    }
}

// ── Row-to-domain-type mapping helpers ───────────────────────────────────────

/// The SELECT column list for `claims` table.
///
/// Note: `value::text` and `metadata::text` cast JSONB → TEXT at read time so
/// `row_to_claim` can call `serde_json::from_str` identically to the SQLite path.
/// This confines the JSONB divergence to the INSERT path only (§2 CLAIM_SELECT_COLS note).
///
/// Column order must exactly match `row_to_claim` indices below.
const CLAIM_SELECT_COLS: &str = "
    claim_id, agent_id, subject, predicate, value::text, cardinality,
    provenance_label, nearest_external_anchor_id, derivation_depth,
    tx_time, valid_time_start, valid_time_end, valid_time_confidence,
    value_confidence, criticality, derived_from,
    metadata::text, snapshot_schema_version
";

/// Map a postgres `Row` from the `claims` table to a `Claim` domain type.
///
/// Column order (must match `CLAIM_SELECT_COLS`):
///   0  claim_id
///   1  agent_id
///   2  subject
///   3  predicate
///   4  value::text  (JSONB cast to TEXT)
///   5  cardinality
///   6  provenance_label
///   7  nearest_external_anchor_id  (nullable)
///   8  derivation_depth
///   9  tx_time
///  10  valid_time_start  (nullable)
///  11  valid_time_end    (nullable)
///  12  valid_time_confidence
///  13  value_confidence
///  14  criticality
///  15  derived_from  (JSON array TEXT)
///  16  metadata::text (nullable JSONB cast to TEXT)
///  17  snapshot_schema_version (nullable INTEGER)
fn row_to_claim(row: &postgres::Row) -> Result<Claim, PostgresStoreError> {
    let claim_id_str: String = row.get(0);
    let agent_id_str: String = row.get(1);
    let subject: String = row.get(2);
    let predicate: String = row.get(3);
    let value_json: String = row.get(4);
    let cardinality_str: String = row.get(5);
    let provenance_str: String = row.get(6);
    let nearest_anchor_str: Option<String> = row.get(7);
    let derivation_depth: i32 = row.get(8);
    let tx_time_str: String = row.get(9);
    let valid_time_start_str: Option<String> = row.get(10);
    let valid_time_end_str: Option<String> = row.get(11);
    let valid_time_confidence: f64 = row.get(12);
    let value_confidence: f64 = row.get(13);
    let criticality_str: String = row.get(14);
    let derived_from_json: String = row.get(15);
    let metadata_json: Option<String> = row.get(16);
    let snapshot_schema_version_raw: Option<i32> = row.get(17);

    let claim_id = uuid::Uuid::parse_str(&claim_id_str)
        .map_err(|e| PostgresStoreError::Mapping(format!("claim_id UUID: {e}")))?;

    let value: serde_json::Value = serde_json::from_str(&value_json)
        .map_err(|e| PostgresStoreError::Mapping(format!("value JSON: {e}")))?;

    let cardinality = str_to_cardinality(&cardinality_str)?;
    let provenance = str_to_provenance(&provenance_str)?;

    let nearest_external_anchor: Option<ClaimRef> = nearest_anchor_str
        .map(|s| {
            uuid::Uuid::parse_str(&s)
                .map(ClaimRef)
                .map_err(|e| PostgresStoreError::Mapping(format!("anchor UUID: {e}")))
        })
        .transpose()?;

    let tx_time = chrono::DateTime::parse_from_rfc3339(&tx_time_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| PostgresStoreError::Mapping(format!("tx_time parse: {e}")))?;

    let valid_time_start = valid_time_start_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| PostgresStoreError::Mapping(format!("valid_time_start: {e}")))
        })
        .transpose()?;

    let valid_time_end = valid_time_end_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| PostgresStoreError::Mapping(format!("valid_time_end: {e}")))
        })
        .transpose()?;

    let criticality = str_to_criticality(&criticality_str)?;

    let derived_from_uuids: Vec<String> = serde_json::from_str(&derived_from_json)
        .map_err(|e| PostgresStoreError::Mapping(format!("derived_from JSON: {e}")))?;

    let derived_from: Vec<ClaimRef> = derived_from_uuids
        .iter()
        .map(|s| {
            uuid::Uuid::parse_str(s)
                .map(ClaimRef)
                .map_err(|e| PostgresStoreError::Mapping(format!("derived_from UUID: {e}")))
        })
        .collect::<Result<_, _>>()?;

    let metadata: Option<serde_json::Value> = metadata_json
        .map(|s| {
            serde_json::from_str(&s)
                .map_err(|e| PostgresStoreError::Mapping(format!("metadata JSON: {e}")))
        })
        .transpose()?;

    let snapshot_schema_version: Option<u32> = snapshot_schema_version_raw.map(|v| v as u32);

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

/// Map a postgres `Row` from the `claim_edges` table to a `ClaimEdge` domain type.
fn row_to_edge(row: &postgres::Row) -> Result<ClaimEdge, PostgresStoreError> {
    let edge_id_str: String = row.get(0);
    let agent_id_str: String = row.get(1);
    let from_claim_str: String = row.get(2);
    let to_claim_str: String = row.get(3);
    let kind_str: String = row.get(4);
    let created_at_str: String = row.get(5);

    let edge_id = uuid::Uuid::parse_str(&edge_id_str)
        .map_err(|e| PostgresStoreError::Mapping(format!("edge_id UUID: {e}")))?;
    let from_claim = uuid::Uuid::parse_str(&from_claim_str)
        .map(ClaimRef)
        .map_err(|e| PostgresStoreError::Mapping(format!("from_claim UUID: {e}")))?;
    let to_claim = uuid::Uuid::parse_str(&to_claim_str)
        .map(ClaimRef)
        .map_err(|e| PostgresStoreError::Mapping(format!("to_claim UUID: {e}")))?;
    let kind = str_to_edge_kind(&kind_str)?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .map_err(|e| PostgresStoreError::Mapping(format!("created_at parse: {e}")))?;

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

impl PostgresPersistenceStore {
    /// Return a `PostgresPendingStore` that shares the same r2d2 connection pool.
    ///
    /// Both `PostgresPersistenceStore` and `PostgresPendingStore` acquire connections
    /// from the same pool. The per-agent write lock held by `EngineHandle` ensures that
    /// the pending insert is serialized with the claim transaction commit.
    pub fn pending_store(&self) -> PostgresPendingStore {
        PostgresPendingStore::new(self.pool.clone())
    }
}

impl PersistencePort for PostgresPersistenceStore {
    type Transaction = PostgresTxn;
    type Error = PostgresStoreError;

    // ── Transaction lifecycle ─────────────────────────────────────────────────

    /// Open an explicit `BEGIN` transaction scoped to `agent_id`.
    ///
    /// Acquires a connection from the r2d2 pool, issues `BEGIN`, then acquires the
    /// per-agent_id advisory lock: `SELECT pg_advisory_xact_lock(hashtext($1)::bigint)`.
    fn begin_atomic(&self, agent_id: &AgentId) -> Result<PostgresTxn, PostgresStoreError> {
        let conn = self.pool.get()?;
        PostgresTxn::begin(agent_id.clone(), conn)
    }

    /// Commit the transaction. The pooled connection returns to the r2d2 pool.
    fn commit(&self, txn: PostgresTxn) -> Result<(), PostgresStoreError> {
        txn.commit_and_drop()
    }

    /// Rollback the transaction. The pooled connection returns to the r2d2 pool.
    fn rollback(&self, txn: PostgresTxn) -> Result<(), PostgresStoreError> {
        txn.rollback_and_drop()
    }

    // ── Write methods (INSERT-only, I1) ───────────────────────────────────────

    /// Append a claim row within the open transaction.
    ///
    /// `value` and `metadata` are cast to JSONB via `$n::jsonb` SQL cast (§2 JSONB note).
    fn append_claim(
        &self,
        txn: &mut PostgresTxn,
        claim: &Claim,
    ) -> Result<ClaimRef, PostgresStoreError> {
        let claim_id = claim.claim_ref().0.to_string();
        let agent_id = claim.agent_id().0.clone();
        let fact = claim.fact();
        // Pass value and metadata as serde_json::Value so the postgres driver can
        // encode them as JSONB binary directly (requires feature "with-serde_json-1").
        // A String with `$n::jsonb` SQL cast does NOT work — the driver type-checks
        // the Rust type against the declared column OID before the cast runs.
        let value_jsonb: &serde_json::Value = &fact.value;
        let cardinality = cardinality_to_str(claim.cardinality()).to_owned();
        let provenance = provenance_to_str(claim.provenance()).to_owned();
        let anchor = claim.external_anchor();
        let nearest_anchor: Option<String> =
            anchor.nearest_external_anchor.as_ref().map(|r| r.0.to_string());
        let derivation_depth = anchor.derivation_depth as i32;
        let tx_time = claim.transaction_time().0.to_rfc3339();
        let vt = claim.valid_time();
        let valid_time_start: Option<String> = vt.start.map(|dt| dt.to_rfc3339());
        let valid_time_end: Option<String> = vt.end.map(|dt| dt.to_rfc3339());
        let valid_time_confidence = vt.valid_time_confidence as f64;
        let conf = claim.confidence();
        let value_confidence = conf.value_confidence as f64;
        let criticality = criticality_to_str(claim.criticality()).to_owned();
        let derived_from_refs: Vec<String> =
            claim.derived_from().iter().map(|r| r.0.to_string()).collect();
        let derived_from_json = serde_json::to_string(&derived_from_refs)
            .map_err(|e| PostgresStoreError::Mapping(format!("derived_from serialization: {e}")))?;
        // metadata is Option<serde_json::Value>: pass as Option<&serde_json::Value>
        let metadata_jsonb: Option<serde_json::Value> = claim.metadata().cloned();
        let snapshot_schema_version: Option<i32> =
            claim.snapshot_schema_version().map(|v| v as i32);

        txn.client().execute(
            "INSERT INTO claims (
                claim_id, agent_id, subject, predicate, value, cardinality,
                provenance_label, nearest_external_anchor_id, derivation_depth,
                tx_time, valid_time_start, valid_time_end, valid_time_confidence,
                value_confidence, criticality, derived_from,
                metadata, snapshot_schema_version, embedding_model_id
            ) VALUES (
                $1,  $2,  $3,  $4,  $5,  $6,
                $7,  $8,  $9,
                $10, $11, $12, $13,
                $14, $15, $16,
                $17, $18, NULL
            )",
            &[
                &claim_id,
                &agent_id,
                &fact.subject.as_str(),
                &fact.predicate.as_str(),
                &value_jsonb,
                &cardinality,
                &provenance,
                &nearest_anchor,
                &derivation_depth,
                &tx_time,
                &valid_time_start,
                &valid_time_end,
                &valid_time_confidence,
                &value_confidence,
                &criticality,
                &derived_from_json,
                &metadata_jsonb,
                &snapshot_schema_version,
            ],
        )?;

        Ok(claim.claim_ref().clone())
    }

    /// Append a validity assertion row within the open transaction.
    fn append_validity_assertion(
        &self,
        txn: &mut PostgresTxn,
        assertion: &ValidityAssertion,
    ) -> Result<(), PostgresStoreError> {
        let assertion_id = assertion.assertion_ref.to_string();
        let agent_id = assertion.agent_id.0.clone();
        let target_claim_id = assertion.target_claim.0.to_string();
        let provenance = provenance_to_str(&assertion.provenance).to_owned();
        let value_confidence = assertion.confidence.value_confidence as f64;
        let valid_time_confidence = assertion.confidence.valid_time_confidence as f64;
        let asserted_at = assertion.asserted_at.0.to_rfc3339();

        let (assertion_kind, bound_at, reopen_at): (&str, Option<String>, Option<String>) =
            match &assertion.kind {
                AssertionKind::Bound { bound_at } => ("Bound", Some(bound_at.to_rfc3339()), None),
                AssertionKind::Reopen { reopen_at } => ("Reopen", None, Some(reopen_at.to_rfc3339())),
            };

        txn.client().execute(
            "INSERT INTO validity_assertions (
                assertion_id, agent_id, target_claim_id,
                assertion_kind, bound_at, reopen_at,
                provenance_label, value_confidence, valid_time_confidence, asserted_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &assertion_id,
                &agent_id,
                &target_claim_id,
                &assertion_kind,
                &bound_at,
                &reopen_at,
                &provenance,
                &value_confidence,
                &valid_time_confidence,
                &asserted_at,
            ],
        )?;

        Ok(())
    }

    /// Append a ledger entry row within the open transaction.
    ///
    /// `stream_seq` is assigned via:
    /// `SELECT COALESCE(MAX(stream_seq), 0) + 1 FROM ledger_entries WHERE agent_id = $1`
    /// within the same transaction, under the per-agent advisory lock.
    ///
    /// INVARIANT: this MAX+1 assignment is safe ONLY under `pg_advisory_xact_lock`.
    /// If the advisory lock is ever removed, replace with a Postgres SEQUENCE object.
    fn append_ledger_entry(
        &self,
        txn: &mut PostgresTxn,
        entry: &LedgerEntry,
    ) -> Result<(), PostgresStoreError> {
        let entry_id = entry.entry_id.to_string();
        let agent_id = entry.agent_id.0.clone();
        let claim_id = entry.claim_ref.0.to_string();
        let event_kind = ledger_event_kind_to_str(&entry.event_kind).to_owned();
        let disposition = disposition_to_str(&entry.disposition).to_owned();
        // Pass rationale as Option<serde_json::Value> so the driver encodes it as JSONB.
        // A String with `$6::jsonb` cast does NOT work — see append_claim note above.
        let rationale_jsonb: Option<serde_json::Value> = entry.rationale.clone();
        let recorded_at = entry.recorded_at.0.to_rfc3339();

        // INVARIANT: safe only under pg_advisory_xact_lock; replace with a SEQUENCE if the lock is ever removed.
        let row = txn.client().query_one(
            "SELECT COALESCE(MAX(stream_seq), 0) + 1 FROM ledger_entries WHERE agent_id = $1",
            &[&agent_id],
        )?;
        let stream_seq: i64 = row.get(0);

        txn.client().execute(
            "INSERT INTO ledger_entries (
                entry_id, agent_id, claim_id, event_kind, disposition, rationale, recorded_at, stream_seq
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &entry_id,
                &agent_id,
                &claim_id,
                &event_kind,
                &disposition,
                &rationale_jsonb,
                &recorded_at,
                &stream_seq,
            ],
        )?;

        Ok(())
    }

    /// Append a claim edge row within the open transaction.
    fn append_claim_edge(
        &self,
        txn: &mut PostgresTxn,
        edge: &ClaimEdge,
    ) -> Result<(), PostgresStoreError> {
        let edge_id = edge.edge_id.to_string();
        let agent_id = edge.agent_id.0.clone();
        let from_claim_id = edge.from_claim.0.to_string();
        let to_claim_id = edge.to_claim.0.to_string();
        let edge_kind = edge_kind_to_str(&edge.kind).to_owned();
        let created_at = edge.created_at.0.to_rfc3339();

        txn.client().execute(
            "INSERT INTO claim_edges (
                edge_id, agent_id, from_claim_id, to_claim_id, edge_kind, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &edge_id,
                &agent_id,
                &from_claim_id,
                &to_claim_id,
                &edge_kind,
                &created_at,
            ],
        )?;

        Ok(())
    }

    // ── Read methods (pool.get() per call; non-mutating) ─────────────────────

    /// Load all claims on (agent_id, subject, predicate), ordered by tx_time ASC.
    fn load_subject_line(
        &self,
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
    ) -> Result<Vec<Claim>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let sql = format!(
            "SELECT {cols} FROM claims
             WHERE agent_id = $1 AND subject = $2 AND predicate = $3
             ORDER BY tx_time ASC",
            cols = CLAIM_SELECT_COLS
        );
        let rows = conn.query(
            &sql,
            &[&agent_id.0.as_str(), &subject, &predicate],
        )?;
        rows.iter().map(row_to_claim).collect()
    }

    /// Load a single claim by `ClaimRef`. Returns `None` if not found.
    fn load_claim(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Option<Claim>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let claim_id_str = claim_ref.0.to_string();
        let sql = format!(
            "SELECT {cols} FROM claims WHERE agent_id = $1 AND claim_id = $2",
            cols = CLAIM_SELECT_COLS
        );
        let rows = conn.query(&sql, &[&agent_id.0.as_str(), &claim_id_str.as_str()])?;
        match rows.first() {
            None => Ok(None),
            Some(row) => Ok(Some(row_to_claim(row)?)),
        }
    }

    /// Load all validity assertions targeting a claim, ordered by asserted_at ASC.
    fn load_validity_assertions_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ValidityAssertion>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let claim_id_str = claim_ref.0.to_string();
        let rows = conn.query(
            "SELECT assertion_id, agent_id, target_claim_id,
                    assertion_kind, bound_at, reopen_at,
                    provenance_label, value_confidence, valid_time_confidence, asserted_at
             FROM validity_assertions
             WHERE agent_id = $1 AND target_claim_id = $2
             ORDER BY asserted_at ASC",
            &[&agent_id.0.as_str(), &claim_id_str.as_str()],
        )?;

        rows.iter()
            .map(|row| {
                let assertion_id_str: String = row.get(0);
                let agent_id_str: String = row.get(1);
                let target_claim_str: String = row.get(2);
                let kind_str: String = row.get(3);
                let bound_at_str: Option<String> = row.get(4);
                let reopen_at_str: Option<String> = row.get(5);
                let prov_str: String = row.get(6);
                let value_confidence: f64 = row.get(7);
                let valid_time_confidence: f64 = row.get(8);
                let asserted_at_str: String = row.get(9);

                let assertion_ref = uuid::Uuid::parse_str(&assertion_id_str)
                    .map_err(|e| PostgresStoreError::Mapping(format!("assertion_id UUID: {e}")))?;
                let target_claim = uuid::Uuid::parse_str(&target_claim_str)
                    .map(ClaimRef)
                    .map_err(|e| PostgresStoreError::Mapping(format!("target_claim UUID: {e}")))?;
                let provenance = str_to_provenance(&prov_str)?;
                let asserted_at = chrono::DateTime::parse_from_rfc3339(&asserted_at_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| PostgresStoreError::Mapping(format!("asserted_at: {e}")))?;

                let kind = match kind_str.as_str() {
                    "Bound" => {
                        let s = bound_at_str.ok_or_else(|| {
                            PostgresStoreError::Mapping("bound_at is NULL for Bound assertion".into())
                        })?;
                        let dt = chrono::DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .map_err(|e| PostgresStoreError::Mapping(format!("bound_at: {e}")))?;
                        AssertionKind::Bound { bound_at: dt }
                    }
                    "Reopen" => {
                        let s = reopen_at_str.ok_or_else(|| {
                            PostgresStoreError::Mapping("reopen_at is NULL for Reopen assertion".into())
                        })?;
                        let dt = chrono::DateTime::parse_from_rfc3339(&s)
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .map_err(|e| PostgresStoreError::Mapping(format!("reopen_at: {e}")))?;
                        AssertionKind::Reopen { reopen_at: dt }
                    }
                    other => {
                        return Err(PostgresStoreError::Mapping(format!(
                            "unknown assertion_kind: {other}"
                        )))
                    }
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
            })
            .collect()
    }

    /// Load ledger entries for an agent, optionally starting from `from` (inclusive),
    /// limited to `limit` rows, ordered by recorded_at ASC.
    fn load_ledger(
        &self,
        agent_id: &AgentId,
        from: Option<&TransactionTime>,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let limit_i64 = limit as i64;

        let map_row = |row: &postgres::Row| -> Result<LedgerEntry, PostgresStoreError> {
            let entry_id_str: String = row.get(0);
            let agent_id_str: String = row.get(1);
            let claim_id_str: String = row.get(2);
            let event_kind_str: String = row.get(3);
            let disposition_str: String = row.get(4);
            let rationale_json: Option<String> = row.get(5);
            let recorded_at_str: String = row.get(6);

            let entry_id = uuid::Uuid::parse_str(&entry_id_str)
                .map_err(|e| PostgresStoreError::Mapping(format!("entry_id UUID: {e}")))?;
            let claim_id = uuid::Uuid::parse_str(&claim_id_str)
                .map(ClaimRef)
                .map_err(|e| PostgresStoreError::Mapping(format!("claim_id UUID: {e}")))?;
            let event_kind = str_to_ledger_event_kind(&event_kind_str)?;
            let disposition = str_to_disposition(&disposition_str)?;
            let rationale: Option<serde_json::Value> = rationale_json
                .map(|s| {
                    serde_json::from_str(&s)
                        .map_err(|e| PostgresStoreError::Mapping(format!("rationale JSON: {e}")))
                })
                .transpose()?;
            let recorded_at = chrono::DateTime::parse_from_rfc3339(&recorded_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| PostgresStoreError::Mapping(format!("recorded_at: {e}")))?;

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

        let rows = if let Some(from_time) = from {
            let from_str = from_time.0.to_rfc3339();
            conn.query(
                "SELECT entry_id, agent_id, claim_id, event_kind, disposition, rationale::text, recorded_at
                 FROM ledger_entries
                 WHERE agent_id = $1 AND recorded_at >= $2
                 ORDER BY recorded_at ASC
                 LIMIT $3",
                &[&agent_id.0.as_str(), &from_str.as_str(), &limit_i64],
            )?
        } else {
            conn.query(
                "SELECT entry_id, agent_id, claim_id, event_kind, disposition, rationale::text, recorded_at
                 FROM ledger_entries
                 WHERE agent_id = $1
                 ORDER BY recorded_at ASC
                 LIMIT $2",
                &[&agent_id.0.as_str(), &limit_i64],
            )?
        };

        rows.iter().map(map_row).collect()
    }

    /// Load all edges where `claim_ref` is either the from or to end, ordered by created_at ASC.
    fn load_edges_for(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let claim_id_str = claim_ref.0.to_string();

        let rows = conn.query(
            "SELECT edge_id, agent_id, from_claim_id, to_claim_id, edge_kind, created_at
             FROM claim_edges
             WHERE agent_id = $1
               AND (from_claim_id = $2 OR to_claim_id = $2)
             ORDER BY created_at ASC",
            &[&agent_id.0.as_str(), &claim_id_str.as_str()],
        )?;

        rows.iter().map(row_to_edge).collect()
    }

    /// Load the set of ClaimRefs served as injected claims for this agent (used by the Amplification Guard).
    fn load_injected_claims(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<ClaimRef>, PostgresStoreError> {
        let mut conn = self.pool.get()?;

        let rows = conn.query(
            "SELECT claim_id
             FROM ledger_entries
             WHERE agent_id = $1 AND event_kind = 'ServedAsInjected'
             GROUP BY claim_id
             ORDER BY MIN(recorded_at) ASC",
            &[&agent_id.0.as_str()],
        )?;

        rows.iter()
            .map(|row| {
                let claim_id_str: String = row.get(0);
                uuid::Uuid::parse_str(&claim_id_str)
                    .map(ClaimRef)
                    .map_err(|e| PostgresStoreError::Mapping(format!("claim_id UUID: {e}")))
            })
            .collect()
    }

    /// Recursive CTE lineage traversal — identical SQL to the SQLite adapter.
    ///
    /// Traverses `DerivedFrom` edges upward from `claim_ref`, returning all `ClaimEdge`
    /// rows in the lineage sub-graph ordered by depth ASC, then created_at ASC within depth.
    /// Bounded at depth 64 to prevent runaway on pathological graphs.
    fn load_lineage(
        &self,
        agent_id: &AgentId,
        claim_ref: &ClaimRef,
    ) -> Result<Vec<ClaimEdge>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let start_id = claim_ref.0.to_string();

        let rows = conn.query(
            "WITH RECURSIVE lineage(edge_id, depth) AS (
                -- Base case: all DerivedFrom edges leaving from our starting claim
                SELECT ce.edge_id, 1
                FROM claim_edges ce
                WHERE ce.agent_id = $1
                  AND ce.from_claim_id = $2
                  AND ce.edge_kind = 'DerivedFrom'
                UNION ALL
                -- Recursive case: follow the to_claim of the previous edge onward
                SELECT ce2.edge_id, l.depth + 1
                FROM claim_edges ce2
                JOIN lineage l ON ce2.from_claim_id = (
                    SELECT to_claim_id FROM claim_edges WHERE edge_id = l.edge_id
                )
                WHERE ce2.agent_id = $1
                  AND ce2.edge_kind = 'DerivedFrom'
                  AND l.depth < 64
            )
            SELECT ce.edge_id, ce.agent_id, ce.from_claim_id, ce.to_claim_id,
                   ce.edge_kind, ce.created_at,
                   l.depth
            FROM claim_edges ce
            JOIN lineage l ON ce.edge_id = l.edge_id
            ORDER BY l.depth ASC, ce.created_at ASC",
            &[&agent_id.0.as_str(), &start_id.as_str()],
        )?;

        rows.iter()
            .map(|row| {
                let edge_id_str: String = row.get(0);
                let agent_id_str: String = row.get(1);
                let from_claim_str: String = row.get(2);
                let to_claim_str: String = row.get(3);
                let kind_str: String = row.get(4);
                let created_at_str: String = row.get(5);
                // col 6 = depth (ordering only; not part of ClaimEdge)

                let edge_id = uuid::Uuid::parse_str(&edge_id_str)
                    .map_err(|e| PostgresStoreError::Mapping(format!("edge_id UUID: {e}")))?;
                let from_claim = uuid::Uuid::parse_str(&from_claim_str)
                    .map(ClaimRef)
                    .map_err(|e| PostgresStoreError::Mapping(format!("from_claim UUID: {e}")))?;
                let to_claim = uuid::Uuid::parse_str(&to_claim_str)
                    .map(ClaimRef)
                    .map_err(|e| PostgresStoreError::Mapping(format!("to_claim UUID: {e}")))?;
                let kind = str_to_edge_kind(&kind_str)?;
                let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .map_err(|e| PostgresStoreError::Mapping(format!("created_at: {e}")))?;

                Ok(ClaimEdge {
                    edge_id,
                    agent_id: AgentId(agent_id_str),
                    from_claim,
                    to_claim,
                    kind,
                    created_at: TransactionTime(created_at),
                })
            })
            .collect()
    }

    /// Postgres uses a pool + per-agent advisory lock — no global write lock is needed.
    fn requires_global_write_serialization(&self) -> bool {
        false
    }
}

// ── PostgresPendingStore ──────────────────────────────────────────────────────

/// PostgreSQL-backed `PendingAdjudicationPort` implementation (W3, Amendment 1).
///
/// Uses the same r2d2 pool as `PostgresPersistenceStore`. Each method borrows a pooled
/// connection for the duration of a single non-transactional statement (auto-commit).
/// Serialization is provided by the per-agent write lock in `EngineHandle`.
pub struct PostgresPendingStore {
    pool: r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres::NoTls>>,
}

impl PostgresPendingStore {
    /// Create a pending store sharing the same connection pool.
    pub fn new(pool: r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres::NoTls>>) -> Self {
        Self { pool }
    }
}

impl PendingAdjudicationPort for PostgresPendingStore {
    type Error = PostgresStoreError;

    fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let request_payload = serde_json::to_string(&row.request_payload)
            .map_err(|e| PostgresStoreError::Mapping(format!("request_payload serialization: {e}")))?;
        // queued_at and expires_at are TIMESTAMPTZ columns — pass as chrono::DateTime<Utc>
        // directly (requires postgres feature "with-chrono-0_4"). Passing as String caused
        // WrongType { postgres: Timestamptz, rust: "alloc::string::String" } errors.
        let queued_at: chrono::DateTime<chrono::Utc> = row.queued_at;
        let expires_at: Option<chrono::DateTime<chrono::Utc>> = row.expires_at;
        conn.execute(
            "INSERT INTO pending_adjudications (
                handle_id, agent_id, subject, predicate,
                challenger_claim_ref, incumbent_claim_ref,
                request_payload, queued_at, expires_at, status
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &row.handle_id.to_string(),
                &row.agent_id.0.as_str(),
                &row.subject.as_str(),
                &row.predicate.as_str(),
                &row.challenger_claim_ref.0.to_string(),
                &row.incumbent_claim_ref.0.to_string(),
                &request_payload.as_str(),
                &queued_at,
                &expires_at,
                &row.status.as_str(),
            ],
        )?;
        Ok(())
    }

    fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let rows = conn.query(
            "SELECT handle_id, agent_id, subject, predicate,
                    challenger_claim_ref, incumbent_claim_ref,
                    request_payload, queued_at, expires_at, status
             FROM pending_adjudications
             WHERE handle_id = $1",
            &[&handle_id.to_string()],
        )?;
        match rows.into_iter().next() {
            None => Ok(None),
            Some(row) => Ok(Some(pg_row_to_pending(&row)?)),
        }
    }

    fn list_pending(&self, agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        let rows = if let Some(aid) = agent_id {
            conn.query(
                "SELECT handle_id, agent_id, subject, predicate,
                        challenger_claim_ref, incumbent_claim_ref,
                        request_payload, queued_at, expires_at, status
                 FROM pending_adjudications
                 WHERE agent_id = $1 AND status = 'pending'
                 ORDER BY queued_at ASC",
                &[&aid.0.as_str()],
            )?
        } else {
            conn.query(
                "SELECT handle_id, agent_id, subject, predicate,
                        challenger_claim_ref, incumbent_claim_ref,
                        request_payload, queued_at, expires_at, status
                 FROM pending_adjudications
                 WHERE status = 'pending'
                 ORDER BY queued_at ASC",
                &[],
            )?
        };
        rows.iter().map(pg_row_to_pending).collect()
    }

    fn list_expired(&self, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<PendingAdjudicationRow>, PostgresStoreError> {
        let mut conn = self.pool.get()?;
        // Pass now as chrono::DateTime<Utc> directly — expires_at is TIMESTAMPTZ.
        let rows = conn.query(
            "SELECT handle_id, agent_id, subject, predicate,
                    challenger_claim_ref, incumbent_claim_ref,
                    request_payload, queued_at, expires_at, status
             FROM pending_adjudications
             WHERE expires_at IS NOT NULL AND expires_at <= $1 AND status = 'pending'
             ORDER BY expires_at ASC",
            &[&now],
        )?;
        rows.iter().map(pg_row_to_pending).collect()
    }

    fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), PostgresStoreError> {
        let mut conn = self.pool.get()?;
        conn.execute(
            "UPDATE pending_adjudications SET status = 'resolved' WHERE handle_id = $1",
            &[&handle_id.to_string()],
        )?;
        Ok(())
    }

    fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), PostgresStoreError> {
        let mut conn = self.pool.get()?;
        conn.execute(
            "UPDATE pending_adjudications SET status = 'expired' WHERE handle_id = $1",
            &[&handle_id.to_string()],
        )?;
        Ok(())
    }

    /// Detect QueuedForAdjudication claims with no matching pending row (Postgres adapter).
    ///
    /// Same approach as SQLite: cross-table query joining ledger_entries + claims + pending_adjudications.
    fn list_queued_orphan_claims(
        &self,
    ) -> Result<Vec<mempill_core::ports::pending_adjudication::OrphanedQueuedClaim>, PostgresStoreError> {
        let mut conn = self.pool.get()?;

        // Phase 1: find orphaned QueuedForAdjudication claim refs.
        // NOTE: the schema column is `claim_id` in both `ledger_entries` and `claims`
        // (not `claim_ref` — that was the original bug caught by live PG tests).
        let orphan_rows = conn.query(
            "SELECT l.agent_id, l.claim_id, c.subject, c.predicate
             FROM ledger_entries l
             JOIN claims c ON c.claim_id = l.claim_id AND c.agent_id = l.agent_id
             WHERE l.disposition = 'QueuedForAdjudication'
               AND l.recorded_at = (
                   SELECT MAX(l2.recorded_at) FROM ledger_entries l2
                   WHERE l2.claim_id = l.claim_id AND l2.agent_id = l.agent_id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM pending_adjudications pa
                   WHERE pa.challenger_claim_ref = l.claim_id
                     AND pa.agent_id = l.agent_id
                     AND pa.status = 'pending'
               )",
            &[],
        )?;

        let mut results = Vec::new();
        for row in &orphan_rows {
            let agent_id_str: String = row.get(0);
            let challenger_str: String = row.get(1);
            let subject: String = row.get(2);
            let predicate: String = row.get(3);

            let challenger_ref = uuid::Uuid::parse_str(&challenger_str)
                .map(mempill_types::ClaimRef)
                .map_err(|e| PostgresStoreError::Mapping(format!("challenger_claim_ref UUID: {e}")))?;

            // Phase 2: find incumbent CommittedCheap claim on the same subject line.
            // NOTE: schema column is `claim_id` (not `claim_ref`) in both tables.
            let incumbent_rows = conn.query(
                "SELECT l.claim_id
                 FROM ledger_entries l
                 JOIN claims c ON c.claim_id = l.claim_id AND c.agent_id = l.agent_id
                 WHERE l.agent_id = $1
                   AND c.subject = $2
                   AND c.predicate = $3
                   AND l.disposition = 'CommittedCheap'
                   AND l.recorded_at = (
                       SELECT MAX(l2.recorded_at) FROM ledger_entries l2
                       WHERE l2.claim_id = l.claim_id AND l2.agent_id = l.agent_id
                   )
                 ORDER BY l.recorded_at DESC
                 LIMIT 1",
                &[&agent_id_str.as_str(), &subject.as_str(), &predicate.as_str()],
            )?;

            let incumbent_ref = incumbent_rows.first()
                .map(|ir| {
                    let ref_str: String = ir.get(0);
                    uuid::Uuid::parse_str(&ref_str)
                        .map(mempill_types::ClaimRef)
                        .map_err(|e| PostgresStoreError::Mapping(format!("incumbent UUID: {e}")))
                })
                .transpose()?;

            results.push(mempill_core::ports::pending_adjudication::OrphanedQueuedClaim {
                agent_id: mempill_types::AgentId(agent_id_str),
                challenger_claim_ref: challenger_ref,
                incumbent_claim_ref: incumbent_ref,
                subject,
                predicate,
            });
        }

        Ok(results)
    }
}

/// Map a Postgres `Row` from `pending_adjudications` to a `PendingAdjudicationRow`.
///
/// `queued_at` and `expires_at` are `TIMESTAMPTZ` columns; we read them as
/// `chrono::DateTime<chrono::Utc>` directly via the `with-chrono-0_4` postgres feature.
/// All other UUID-like columns are stored as TEXT and parsed manually.
fn pg_row_to_pending(row: &postgres::Row) -> Result<PendingAdjudicationRow, PostgresStoreError> {
    let handle_id_str: String = row.get(0);
    let agent_id_str: String = row.get(1);
    let subject: String = row.get(2);
    let predicate: String = row.get(3);
    let challenger_str: String = row.get(4);
    let incumbent_str: String = row.get(5);
    let payload_json: String = row.get(6);
    // TIMESTAMPTZ columns — read as native chrono type (with-chrono-0_4 feature).
    let queued_at: chrono::DateTime<chrono::Utc> = row.get(7);
    let expires_at: Option<chrono::DateTime<chrono::Utc>> = row.get(8);
    let status: String = row.get(9);

    let handle_id = uuid::Uuid::parse_str(&handle_id_str)
        .map_err(|e| PostgresStoreError::Mapping(format!("handle_id UUID: {e}")))?;
    let challenger_claim_ref = uuid::Uuid::parse_str(&challenger_str)
        .map(ClaimRef)
        .map_err(|e| PostgresStoreError::Mapping(format!("challenger_claim_ref UUID: {e}")))?;
    let incumbent_claim_ref = uuid::Uuid::parse_str(&incumbent_str)
        .map(ClaimRef)
        .map_err(|e| PostgresStoreError::Mapping(format!("incumbent_claim_ref UUID: {e}")))?;
    let request_payload: mempill_types::AdjudicationRequest =
        serde_json::from_str(&payload_json)
            .map_err(|e| PostgresStoreError::Mapping(format!("request_payload JSON: {e}")))?;

    Ok(PendingAdjudicationRow {
        handle_id,
        agent_id: AgentId(agent_id_str),
        subject,
        predicate,
        challenger_claim_ref,
        incumbent_claim_ref,
        request_payload,
        queued_at,
        expires_at,
        status,
    })
}

// ── Constructor ───────────────────────────────────────────────────────────────

/// Convenience constructor: build a `PostgresEngine<O, V>` (an `EngineHandle` backed
/// by `PostgresPersistenceStore`) from a connection string.
///
/// This is the recommended entry point for callers that want the full async EngineHandle.
pub fn open_postgres<O, V>(
    connection_string: &str,
    oracle: Option<Arc<O>>,
    vector: Option<Arc<V>>,
    config: EngineConfig,
) -> Result<EngineHandle<PostgresPersistenceStore, O, V>, PostgresStoreError>
where
    O: mempill_core::ports::OraclePort + Send + Sync + 'static,
    V: mempill_core::ports::VectorPort + Send + Sync + 'static,
{
    let store = PostgresPersistenceStore::new(connection_string)?;
    Ok(EngineHandle::new(Arc::new(store), oracle, vector, config))
}

/// Convenience constructor: build a `PostgresEngine<O, V>` wired with a real oracle
/// and the Postgres-backed pending-adjudication store.
///
/// Mirrors `open_postgres` but calls `EngineHandle::new_with_pending_store` so that
/// `QueuedForAdjudication` rows are persisted and verdicts can be delivered via
/// `EngineHandle::submit_adjudication`.
///
/// `open_postgres` (no-oracle variant) is left UNCHANGED.
pub fn open_postgres_with_oracle<O, V>(
    connection_string: &str,
    oracle: Arc<O>,
    vector: Option<Arc<V>>,
    config: EngineConfig,
) -> Result<EngineHandle<PostgresPersistenceStore, O, V>, PostgresStoreError>
where
    O: mempill_core::ports::OraclePort + Send + Sync + 'static,
    V: mempill_core::ports::VectorPort + Send + Sync + 'static,
{
    let store = PostgresPersistenceStore::new(connection_string)?;
    let store_arc = Arc::new(store);
    let pending_store: Arc<dyn mempill_core::ErasedPendingStore> = Arc::new(
        mempill_core::ErasedPendingStoreAdapter::new(store_arc.pending_store()),
    );
    Ok(EngineHandle::new_with_pending_store::<()>(
        store_arc,
        Some(oracle),
        vector,
        pending_store,
        config,
    ))
}
