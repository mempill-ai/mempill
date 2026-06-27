//! W8 oracle-resolution conformance suite — Postgres adapter.
//!
//! Proves that the SAME generic oracle-conformance scenarios (defined in
//! `mempill_core::testing::oracle_conformance`) pass on Postgres 16 AND Postgres 18,
//! producing identical behavior to the SQLite adapter.
//!
//! Every test spawns an OS thread that owns its tokio runtime + postgres client
//! (the postgres sync crate calls `block_on` in `Client::drop`, which is unsafe on
//! a tokio executor thread — the thread-isolation pattern is mandatory).
//!
//! # Scenario index  (PG16 + PG18 = 2× each)
//!
//! | Sub-test | PG16 fn | PG18 fn |
//! |----------|---------|---------|
//! | 1  affirm_challenger_wins    | `pg16_oc_01_affirm` | `pg18_oc_01_affirm` |
//! | 2  deny_incumbent_stands     | `pg16_oc_02_deny`   | `pg18_oc_02_deny`   |
//! | 3  unknown_stays_contested   | `pg16_oc_03_unknown`| `pg18_oc_03_unknown`|
//! | 4  queued_surfaces_contested | `pg16_oc_04_queued` | `pg18_oc_04_queued` |
//! | 5  stale_handle_not_found    | `pg16_oc_05_stale`  | `pg18_oc_05_stale`  |
//! | 6  duplicate_submit          | `pg16_oc_06_dup`    | `pg18_oc_06_dup`    |
//! | 7  ttl_expiry_reverts        | `pg16_oc_07_ttl`    | `pg18_oc_07_ttl`    |
//! | 8a sweep_reverts_expired     | `pg16_oc_08a_sweep` | `pg18_oc_08a_sweep` |
//! | 8b sweep_recovers_orphan     | `pg16_oc_08b_orphan`| `pg18_oc_08b_orphan`|
//! | 9  durable_store_reopen      | `pg16_oc_09_reopen` | `pg18_oc_09_reopen` |
//! | 10 atomicity_no_torn_write   | `pg16_oc_10_atom`   | `pg18_oc_10_atom`   |
//! | 11a ledger_affirm            | `pg16_oc_11a`       | `pg18_oc_11a`       |
//! | 11b ledger_deny              | `pg16_oc_11b`       | `pg18_oc_11b`       |
//! | 11c ledger_unknown           | `pg16_oc_11c`       | `pg18_oc_11c`       |
//! | 12 b11_oracle_absent         | `pg16_oc_12_b11`    | `pg18_oc_12_b11`    |

mod common;

use std::sync::Arc;

use mempill_core::{
    testing::oracle_conformance::{self as oc, TestOracle},
    EngineConfig, EngineHandle,
};
use mempill_postgres::{open_postgres, open_postgres_with_oracle, PostgresPersistenceStore};
use mempill_types::{AdjudicationVerdict, Disposition, LedgerEventKind};

// ── Engine builder helpers ────────────────────────────────────────────────────

type OracleEng = EngineHandle<PostgresPersistenceStore, TestOracle, mempill_core::NoOpVector>;
type DefaultEng = EngineHandle<PostgresPersistenceStore, mempill_core::NoOpOracle, mempill_core::NoOpVector>;

fn build_engine(conn_str: &str, handle_id: uuid::Uuid) -> OracleEng {
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });
    open_postgres_with_oracle(
        conn_str,
        oracle,
        None::<Arc<mempill_core::NoOpVector>>,
        EngineConfig::default(),
    )
    .expect("open_postgres_with_oracle must succeed")
}

fn build_engine_tiny_ttl(conn_str: &str, handle_id: uuid::Uuid) -> OracleEng {
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });
    open_postgres_with_oracle(
        conn_str,
        oracle,
        None::<Arc<mempill_core::NoOpVector>>,
        oc::tiny_ttl_config(),
    )
    .expect("open_postgres_with_oracle (tiny ttl) must succeed")
}

fn build_default_engine(conn_str: &str) -> DefaultEng {
    open_postgres::<mempill_core::NoOpOracle, mempill_core::NoOpVector>(
        conn_str,
        None,
        None,
        EngineConfig::default(),
    )
    .expect("open_postgres (no-oracle) must succeed")
}

// ── Thread-isolation runner ───────────────────────────────────────────────────
//
// All Postgres tests use this pattern: spawn an OS thread, build a tokio runtime,
// run the async scenario, drop the engine, return Ok or Err string.

fn run_in_thread<F>(f: F)
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    let join = std::thread::spawn(f);
    join.join().expect("test thread must not panic")
        .expect("pg oracle conformance scenario must pass");
}

// ── Scenario runners (generic over PG tag) ────────────────────────────────────

fn run_01_affirm(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_affirm_challenger_wins_with_handle(&engine, handle_id).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_02_deny(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_deny_incumbent_stands(&engine, handle_id).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_03_unknown(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_unknown_stays_contested(&engine, handle_id).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_04_queued(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_queued_surfaces_contested(&engine).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_05_stale(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_stale_handle_not_found(&engine).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_06_dup(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_duplicate_submit_not_found(&engine, handle_id).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_07_ttl(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine_tiny_ttl(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_ttl_expiry_reverts_contested(&engine, handle_id).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_08a_sweep(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine_tiny_ttl(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_sweep_reverts_expired(&engine).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_08b_orphan(conn_str: &str) {
    use mempill_core::{
        engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter},
        ports::PersistencePort,
    };
    use mempill_postgres::PostgresPendingStore;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimRef, Confidence, Criticality, Disposition, ExternalAnchor,
        ExternalKind, Fact, LedgerEntry, LedgerEventKind, ProvenanceLabel, TransactionTime, ValidTime,
    };

    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let agent_name = "pg-oc-orphan-agent";
        let agent = AgentId(agent_name.into());

        let persistence = Arc::new(
            PostgresPersistenceStore::new(&conn_str)
                .expect("PostgresPersistenceStore for orphan test must open"),
        );

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
        let mut txn = persistence.begin_atomic(&agent).expect("begin txn incumbent");
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
        let mut txn2 = persistence.begin_atomic(&agent).expect("begin txn challenger");
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
        let pending_pg: PostgresPendingStore = persistence.pending_store();
        let pending_store: Arc<dyn ErasedPendingStore> =
            Arc::new(ErasedPendingStoreAdapter::new(pending_pg));
        let dummy_handle = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: dummy_handle });
        let engine = EngineHandle::<_, _, mempill_core::NoOpVector>::new_with_pending_store::<()>(
            persistence,
            Some(oracle),
            None::<Arc<mempill_core::NoOpVector>>,
            pending_store,
            EngineConfig::default(),
        );

        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_sweep_recovers_orphan(&engine, agent_name).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_09_reopen(conn_str: &str) {
    // For Postgres, "reopen" means opening a second EngineHandle on the SAME database
    // (same container, same conn_str).  The pending row is DB-durable, so the second
    // engine can submit the verdict on the pre-restart handle.
    //
    // POSTGRES DROP CONSTRAINT:
    // postgres::Client::drop calls block_on internally, which panics if called from within
    // a tokio runtime.  We must drop engine1 AFTER the tokio runtime is shut down.
    //
    // Strategy:
    //   1. Build engine1, ingest the conflict (rt1.block_on).
    //   2. Drop rt1 completely, then drop engine1 — safe because no runtime is active.
    //   3. Build engine2, submit Affirm and verify (rt2.block_on), then drop rt2 then engine2.
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let agent = mempill_types::AgentId("conformance-reopen-agent".into());

        // ── Phase 1: ingest conflict on engine1 ──────────────────────────────
        let engine1 = build_engine(&conn_str, handle_id);
        let (challenger_ref, ingest_err) = {
            let rt1 = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
            let agent_cl = agent.clone();
            let res = rt1.block_on(async {
                let inc = engine1.ingest_claim(oc::ingest_req(&agent_cl, "reopen-incumbent")).await
                    .map_err(|e| format!("ingest incumbent: {e:?}"))?;
                assert_eq!(inc.disposition, mempill_types::Disposition::CommittedCheap,
                    "conformance[reopen/pg]: incumbent must be CommittedCheap");
                let ch = engine1.ingest_claim(oc::ingest_req(&agent_cl, "reopen-challenger")).await
                    .map_err(|e| format!("ingest challenger: {e:?}"))?;
                assert_eq!(ch.disposition, mempill_types::Disposition::QueuedForAdjudication,
                    "conformance[reopen/pg]: challenger must be QueuedForAdjudication");
                Ok::<_, String>(ch.claim_ref.clone())
            });
            // Drop rt1 first so no tokio runtime threads are active on this OS thread.
            drop(rt1);
            match res {
                Ok(cr) => (Some(cr), None),
                Err(e) => (None, Some(e)),
            }
        };
        // Drop engine1 AFTER the runtime is shut down (safe: no active runtime).
        drop(engine1);

        if let Some(e) = ingest_err {
            return Err(e);
        }
        let challenger_ref = challenger_ref.unwrap();

        // ── Phase 2: open engine2, submit Affirm, verify ──────────────────────
        let engine2 = build_engine(&conn_str, handle_id);
        let result = {
            let rt2 = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
            let agent_cl = agent.clone();
            let ch_ref = challenger_ref.clone();
            let res = rt2.block_on(async {
                let outcome = engine2.submit_adjudication(
                    handle_id,
                    mempill_types::AdjudicationResponse {
                        handle_id,
                        verdict: mempill_types::AdjudicationVerdict::Affirm,
                        evidence_provenance: mempill_types::ProvenanceLabel::External(
                            mempill_types::ExternalKind::ExternalFirstHand,
                        ),
                    },
                ).await.map_err(|e| format!("submit Affirm on pre-restart handle: {e:?}"))?;
                assert_eq!(outcome.disposition, mempill_types::Disposition::CommittedCheap,
                    "conformance[reopen/pg]: challenger must be CommittedCheap after cross-restart Affirm");
                assert_eq!(outcome.claim_ref, ch_ref,
                    "conformance[reopen/pg]: outcome.claim_ref must be challenger");

                let qr = engine2.query_memory(oc::query_req(&agent_cl)).await
                    .map_err(|e| format!("query on engine2: {e:?}"))?;
                let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
                assert_eq!(primary_val, Some(serde_json::json!("reopen-challenger")),
                    "conformance[reopen/pg]: challenger must be surfaced after cross-restart Affirm");
                Ok::<_, String>(())
            });
            drop(rt2);
            res
        };
        drop(engine2);
        result
    });
}

fn run_10_atom(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_atomicity_no_torn_write(&engine, handle_id).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_11_ledger(conn_str: &str, verdict: AdjudicationVerdict, exp_disp: Disposition, exp_kind: LedgerEventKind) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let handle_id = uuid::Uuid::new_v4();
        let engine = build_engine(&conn_str, handle_id);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_ledger_entry_expectations(&engine, handle_id, verdict, exp_disp, exp_kind).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

fn run_12_b11(conn_str: &str) {
    let conn_str = conn_str.to_owned();
    run_in_thread(move || {
        let engine = build_default_engine(&conn_str);
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let result = rt.block_on(async {
            oc::scenario_b11_oracle_absent_contested(&engine).await;
            Ok(())
        });
        drop(engine);
        result
    });
}

// ── PG16 tests ────────────────────────────────────────────────────────────────

#[test]
fn pg16_oc_01_affirm_challenger_wins() {
    common::with_pg_and_conn("16", |_store, conn_str| run_01_affirm(&conn_str));
}

#[test]
fn pg16_oc_02_deny_incumbent_stands() {
    common::with_pg_and_conn("16", |_store, conn_str| run_02_deny(&conn_str));
}

#[test]
fn pg16_oc_03_unknown_stays_contested() {
    common::with_pg_and_conn("16", |_store, conn_str| run_03_unknown(&conn_str));
}

#[test]
fn pg16_oc_04_queued_surfaces_contested() {
    common::with_pg_and_conn("16", |_store, conn_str| run_04_queued(&conn_str));
}

#[test]
fn pg16_oc_05_stale_handle_not_found() {
    common::with_pg_and_conn("16", |_store, conn_str| run_05_stale(&conn_str));
}

#[test]
fn pg16_oc_06_duplicate_submit_not_found() {
    common::with_pg_and_conn("16", |_store, conn_str| run_06_dup(&conn_str));
}

#[test]
fn pg16_oc_07_ttl_expiry_reverts_contested() {
    common::with_pg_and_conn("16", |_store, conn_str| run_07_ttl(&conn_str));
}

#[test]
fn pg16_oc_08a_sweep_reverts_expired() {
    common::with_pg_and_conn("16", |_store, conn_str| run_08a_sweep(&conn_str));
}

#[test]
fn pg16_oc_08b_sweep_recovers_orphan() {
    common::with_pg_and_conn("16", |_store, conn_str| run_08b_orphan(&conn_str));
}

#[test]
fn pg16_oc_09_durable_store_survives_reopen() {
    common::with_pg_and_conn("16", |_store, conn_str| run_09_reopen(&conn_str));
}

#[test]
fn pg16_oc_10_atomicity_no_torn_write() {
    common::with_pg_and_conn("16", |_store, conn_str| run_10_atom(&conn_str));
}

#[test]
fn pg16_oc_11a_ledger_affirm() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_11_ledger(&conn_str, AdjudicationVerdict::Affirm, Disposition::CommittedCheap, LedgerEventKind::AdjudicationResolved);
    });
}

#[test]
fn pg16_oc_11b_ledger_deny() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_11_ledger(&conn_str, AdjudicationVerdict::Deny, Disposition::Superseded, LedgerEventKind::ValidityAsserted);
    });
}

#[test]
fn pg16_oc_11c_ledger_unknown() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_11_ledger(&conn_str, AdjudicationVerdict::Unknown, Disposition::Contested, LedgerEventKind::AdjudicationResolved);
    });
}

#[test]
fn pg16_oc_12_b11_oracle_absent_contested() {
    common::with_pg_and_conn("16", |_store, conn_str| run_12_b11(&conn_str));
}

// ── PG18 tests ────────────────────────────────────────────────────────────────

#[test]
fn pg18_oc_01_affirm_challenger_wins() {
    common::with_pg_and_conn("18", |_store, conn_str| run_01_affirm(&conn_str));
}

#[test]
fn pg18_oc_02_deny_incumbent_stands() {
    common::with_pg_and_conn("18", |_store, conn_str| run_02_deny(&conn_str));
}

#[test]
fn pg18_oc_03_unknown_stays_contested() {
    common::with_pg_and_conn("18", |_store, conn_str| run_03_unknown(&conn_str));
}

#[test]
fn pg18_oc_04_queued_surfaces_contested() {
    common::with_pg_and_conn("18", |_store, conn_str| run_04_queued(&conn_str));
}

#[test]
fn pg18_oc_05_stale_handle_not_found() {
    common::with_pg_and_conn("18", |_store, conn_str| run_05_stale(&conn_str));
}

#[test]
fn pg18_oc_06_duplicate_submit_not_found() {
    common::with_pg_and_conn("18", |_store, conn_str| run_06_dup(&conn_str));
}

#[test]
fn pg18_oc_07_ttl_expiry_reverts_contested() {
    common::with_pg_and_conn("18", |_store, conn_str| run_07_ttl(&conn_str));
}

#[test]
fn pg18_oc_08a_sweep_reverts_expired() {
    common::with_pg_and_conn("18", |_store, conn_str| run_08a_sweep(&conn_str));
}

#[test]
fn pg18_oc_08b_sweep_recovers_orphan() {
    common::with_pg_and_conn("18", |_store, conn_str| run_08b_orphan(&conn_str));
}

#[test]
fn pg18_oc_09_durable_store_survives_reopen() {
    common::with_pg_and_conn("18", |_store, conn_str| run_09_reopen(&conn_str));
}

#[test]
fn pg18_oc_10_atomicity_no_torn_write() {
    common::with_pg_and_conn("18", |_store, conn_str| run_10_atom(&conn_str));
}

#[test]
fn pg18_oc_11a_ledger_affirm() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_11_ledger(&conn_str, AdjudicationVerdict::Affirm, Disposition::CommittedCheap, LedgerEventKind::AdjudicationResolved);
    });
}

#[test]
fn pg18_oc_11b_ledger_deny() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_11_ledger(&conn_str, AdjudicationVerdict::Deny, Disposition::Superseded, LedgerEventKind::ValidityAsserted);
    });
}

#[test]
fn pg18_oc_11c_ledger_unknown() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_11_ledger(&conn_str, AdjudicationVerdict::Unknown, Disposition::Contested, LedgerEventKind::AdjudicationResolved);
    });
}

#[test]
fn pg18_oc_12_b11_oracle_absent_contested() {
    common::with_pg_and_conn("18", |_store, conn_str| run_12_b11(&conn_str));
}
