//! W8 oracle-resolution conformance suite — SQLite adapter.
//!
//! Proves that the SAME generic oracle-conformance scenarios (defined in
//! `mempill_core::testing::oracle_conformance`) pass on SQLite — both in-memory
//! and file-backed (for the durable-reopen test).
//!
//! # Scenario index
//!
//! | Sub-test | #test function |
//! |----------|----------------|
//! | 1  affirm_challenger_wins    | `sqlite_oc_01_affirm_challenger_wins` |
//! | 2  deny_incumbent_stands     | `sqlite_oc_02_deny_incumbent_stands` |
//! | 3  unknown_stays_contested   | `sqlite_oc_03_unknown_stays_contested` |
//! | 4  queued_surfaces_contested | `sqlite_oc_04_queued_surfaces_contested` |
//! | 5  stale_handle_not_found    | `sqlite_oc_05_stale_handle_not_found` |
//! | 6  duplicate_submit          | `sqlite_oc_06_duplicate_submit_not_found` |
//! | 7  ttl_expiry_reverts        | `sqlite_oc_07_ttl_expiry_reverts_contested` |
//! | 8a sweep_reverts_expired     | `sqlite_oc_08a_sweep_reverts_expired` |
//! | 8b sweep_recovers_orphan     | `sqlite_oc_08b_sweep_recovers_orphan` |
//! | 9  durable_store_reopen      | `sqlite_oc_09_durable_store_survives_reopen` |
//! | 10 atomicity_no_torn_write   | `sqlite_oc_10_atomicity_no_torn_write` |
//! | 11 ledger_entry_affirm       | `sqlite_oc_11a_ledger_affirm` |
//! | 11 ledger_entry_deny         | `sqlite_oc_11b_ledger_deny` |
//! | 11 ledger_entry_unknown      | `sqlite_oc_11c_ledger_unknown` |
//! | 12 b11_oracle_absent         | `sqlite_oc_12_b11_oracle_absent_contested` |

use std::sync::Arc;

use mempill_core::{
    engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter},
    testing::oracle_conformance::{
        self as oc, TestOracle,
    },
    EngineConfig, EngineHandle,
};
use mempill_sqlite::{
    connection::{open_in_memory, open as open_file},
    store::SqlitePersistenceStore,
};
use mempill_types::{
    AdjudicationVerdict, Disposition, LedgerEventKind,
};

// ── Engine builders ───────────────────────────────────────────────────────────

type OracleEng = EngineHandle<SqlitePersistenceStore, TestOracle, mempill_core::NoOpVector>;

/// In-memory oracle engine — TestOracle returns the given UUID.
fn build_engine(handle_id: uuid::Uuid) -> OracleEng {
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });
    EngineHandle::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        EngineConfig::default(),
    )
}

/// In-memory oracle engine with tiny TTL (1 ns) for TTL/sweep tests.
fn build_engine_tiny_ttl(handle_id: uuid::Uuid) -> OracleEng {
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });
    EngineHandle::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        oc::tiny_ttl_config(),
    )
}

/// File-backed oracle engine for the reopen test.
fn build_engine_file(path: &str, handle_id: uuid::Uuid) -> OracleEng {
    let conn = open_file(path).expect("file SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });
    EngineHandle::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        EngineConfig::default(),
    )
}

/// No-oracle DefaultEngine for B11 test.
fn build_default_engine() -> mempill_sqlite::DefaultEngine {
    mempill_sqlite::open_default_in_memory().expect("DefaultEngine must open")
}

// ── Sub-test 1: Affirm ────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_01_affirm_challenger_wins() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_affirm_challenger_wins_with_handle(&engine, handle_id).await;
}

// ── Sub-test 2: Deny ──────────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_02_deny_incumbent_stands() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_deny_incumbent_stands(&engine, handle_id).await;
}

// ── Sub-test 3: Unknown ───────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_03_unknown_stays_contested() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_unknown_stays_contested(&engine, handle_id).await;
}

// ── Sub-test 4: Queued (before submit) ───────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_04_queued_surfaces_contested() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_queued_surfaces_contested(&engine).await;
}

// ── Sub-test 5: Stale handle ──────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_05_stale_handle_not_found() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_stale_handle_not_found(&engine).await;
}

// ── Sub-test 6: Duplicate submit ──────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_06_duplicate_submit_not_found() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_duplicate_submit_not_found(&engine, handle_id).await;
}

// ── Sub-test 7: TTL expiry ────────────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_07_ttl_expiry_reverts_contested() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine_tiny_ttl(handle_id);
    oc::scenario_ttl_expiry_reverts_contested(&engine, handle_id).await;
}

// ── Sub-test 8a: Sweep reverts expired ───────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_08a_sweep_reverts_expired() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine_tiny_ttl(handle_id);
    oc::scenario_sweep_reverts_expired(&engine).await;
}

// ── Sub-test 8b: Sweep recovers orphan ───────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_08b_sweep_recovers_orphan() {
    use mempill_core::ports::PersistencePort;
    use mempill_types::{
        Cardinality, Claim, ClaimRef, Confidence, Criticality, ExternalAnchor, ExternalKind,
        Fact, LedgerEntry, LedgerEventKind, ProvenanceLabel, TransactionTime, ValidTime,
    };

    let agent_name = "oc-orphan-agent";
    let agent = mempill_types::AgentId(agent_name.into());

    // Build engine on a fresh in-memory store.
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));

    let now = chrono::Utc::now();

    // Seed incumbent (CommittedCheap).
    let incumbent_ref = ClaimRef(uuid::Uuid::new_v4());
    let incumbent_claim = Claim::new(
        incumbent_ref.clone(),
        agent.clone(),
        Fact {
            subject: "subject".into(),
            predicate: "predicate".into(),
            value: serde_json::json!("orphan-incumbent"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(now - chrono::Duration::seconds(10)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::High,
        vec![],
        None,
        None,
    );
    let mut txn = persistence.begin_atomic(&agent).expect("begin txn for incumbent");
    persistence.append_claim(&mut txn, &incumbent_claim).expect("insert incumbent");
    persistence.append_ledger_entry(&mut txn, &LedgerEntry {
        entry_id: uuid::Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: incumbent_ref.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(now - chrono::Duration::seconds(10)),
    }).expect("insert incumbent ledger");
    persistence.commit(txn).expect("commit incumbent");

    // Seed orphaned challenger (QueuedForAdjudication, NO pending row).
    let challenger_ref = ClaimRef(uuid::Uuid::new_v4());
    let challenger_claim = Claim::new(
        challenger_ref.clone(),
        agent.clone(),
        Fact {
            subject: "subject".into(),
            predicate: "predicate".into(),
            value: serde_json::json!("orphan-challenger"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(now),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::High,
        vec![],
        None,
        None,
    );
    let mut txn2 = persistence.begin_atomic(&agent).expect("begin txn for challenger");
    persistence.append_claim(&mut txn2, &challenger_claim).expect("insert challenger");
    persistence.append_ledger_entry(&mut txn2, &LedgerEntry {
        entry_id: uuid::Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: challenger_ref.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::QueuedForAdjudication,
        rationale: None,
        recorded_at: TransactionTime(now),
    }).expect("insert challenger ledger");
    persistence.commit(txn2).expect("commit challenger");

    // Build engine on the seeded store (no pending row for the orphan).
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let dummy_handle = uuid::Uuid::new_v4();
    let oracle = Arc::new(TestOracle { fixed_uuid: dummy_handle });
    let engine = EngineHandle::<_, _, mempill_core::NoOpVector>::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        EngineConfig::default(),
    );

    // Delegate to the harness scenario function.
    oc::scenario_sweep_recovers_orphan(&engine, agent_name).await;
}

// ── Sub-test 9: Durable store survives reopen (file-backed SQLite) ────────────

#[tokio::test]
async fn sqlite_oc_09_durable_store_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().join("reopen.db");
    let path_str = path.to_str().unwrap().to_owned();
    let handle_id = uuid::Uuid::new_v4();

    let engine1 = build_engine_file(&path_str, handle_id);
    let path_str_2 = path_str.clone();
    let build_engine2 = move || build_engine_file(&path_str_2, handle_id);

    oc::scenario_durable_store_survives_reopen(engine1, build_engine2, handle_id).await;
}

// ── Sub-test 10: Atomicity — no torn write ────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_10_atomicity_no_torn_write() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_atomicity_no_torn_write(&engine, handle_id).await;
}

// ── Sub-test 11a: Ledger — Affirm ────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_11a_ledger_affirm() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_ledger_entry_expectations(
        &engine,
        handle_id,
        AdjudicationVerdict::Affirm,
        Disposition::CommittedCheap,
        LedgerEventKind::AdjudicationResolved,
    ).await;
}

// ── Sub-test 11b: Ledger — Deny ───────────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_11b_ledger_deny() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_ledger_entry_expectations(
        &engine,
        handle_id,
        AdjudicationVerdict::Deny,
        Disposition::Superseded,
        LedgerEventKind::ValidityAsserted,
    ).await;
}

// ── Sub-test 11c: Ledger — Unknown ───────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_11c_ledger_unknown() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine(handle_id);
    oc::scenario_ledger_entry_expectations(
        &engine,
        handle_id,
        AdjudicationVerdict::Unknown,
        Disposition::Contested,
        LedgerEventKind::AdjudicationResolved,
    ).await;
}

// ── Sub-test 12: B11 oracle-absent ───────────────────────────────────────────

#[tokio::test]
async fn sqlite_oc_12_b11_oracle_absent_contested() {
    let engine = build_default_engine();
    oc::scenario_b11_oracle_absent_contested(&engine).await;
}
