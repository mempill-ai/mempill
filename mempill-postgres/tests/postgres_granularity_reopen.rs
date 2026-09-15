//! TASK-33 / QA-A (🔴5): Postgres durable-reopen granularity proof.
//!
//! Ingests month/year-granular facts, DROPS the `PostgresPersistenceStore` handle
//! entirely, then opens a NEW `PostgresPersistenceStore` on the SAME database and
//! proves `query_history` still reports the correct `valid_from_granularity` /
//! `valid_until_granularity` display fields — i.e. granularity survives a full
//! store-handle recycle, not just an in-process read. Follows the same durable-reopen
//! pattern as `postgres_oracle_conformance.rs`'s `run_09_reopen` (oc_09).

mod common;

use mempill_core::{
    application::{dto::QueryHistoryRequest, query_history::QueryHistoryUseCase},
    config::EngineConfig,
    noop::NoOpVector,
    ports::persistence::PersistencePort,
};
use mempill_postgres::PostgresPersistenceStore;
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    identity::{AgentId, ClaimRef},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
    DateGranularity, HistoryEntryStatus,
};
use std::sync::Arc;

fn run_reopen_scenario(pg_tag: &str) {
    common::with_pg_and_conn(pg_tag, |store1, conn_str| {
        let agent = AgentId("gran-reopen-agent".into());

        // "2020-03" (Month) and "2020-06-15" (Day). tx_times and `now` must be REAL,
        // chronologically-consistent epoch values alongside the valid_time dates (unlike
        // the small synthetic offsets used elsewhere in the conformance suite, which
        // never mix with real 2020-era valid_time dates) — otherwise `now` (used as the
        // D2 backward-compat valid_at instant) falls outside both windows and succession
        // selection breaks.
        let start1 = chrono::DateTime::<chrono::Utc>::from_timestamp(1_583_020_800, 0).unwrap(); // 2020-03-01
        let start2 = chrono::DateTime::<chrono::Utc>::from_timestamp(1_592_179_200, 0).unwrap(); // 2020-06-15
        let tx1 = start1 - chrono::Duration::days(1); // 2020-02-29
        let tx2 = start2 - chrono::Duration::days(1); // 2020-06-14

        let claim1 = Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "reopen-project".into(), predicate: "kickoff".into(), value: serde_json::json!("phase-1") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx1),
            // Explicit (non-open) end at claim2's start — classic non-overlapping
            // succession, matching the write_ceo_succession pattern used elsewhere in the
            // conformance suite. An open-ended claim1 would OVERLAP claim2's open window
            // and correctly NOT trigger succession — this test targets granularity
            // survival across reopen, not overlap/conflict semantics.
            //
            // end_granularity=Day: under the fold-derived history design, an entry's own
            // explicit end always wins over a successor's ordering key (bug A fix), so
            // valid_until_granularity is sourced from THIS claim's own end_granularity, not
            // claim2's start_granularity. Set explicitly here so the test still proves
            // granularity survives a full store-handle reopen (not fabricated as None).
            ValidTime { start: Some(start1), end: Some(start2), valid_time_confidence: 0.9, start_granularity: Some(DateGranularity::Month), end_granularity: Some(DateGranularity::Day) },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            Criticality::Medium,
            vec![],
            None,
            None,
        );
        let claim2 = Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "reopen-project".into(), predicate: "kickoff".into(), value: serde_json::json!("phase-2") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx2),
            ValidTime { start: Some(start2), end: None, valid_time_confidence: 0.9, start_granularity: Some(DateGranularity::Day), end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            Criticality::Medium,
            vec![],
            None,
            None,
        );

        // ── Phase 1: write via store1, then DROP the handle entirely ────────────────
        {
            let mut txn = store1.begin_atomic(&agent).expect("reopen: begin_atomic");
            store1.append_claim(&mut txn, &claim1).expect("reopen: append claim1");
            store1.append_claim(&mut txn, &claim2).expect("reopen: append claim2");
            store1.commit(txn).expect("reopen: commit");
        }
        drop(store1);

        // ── Phase 2: open a BRAND NEW PostgresPersistenceStore on the SAME database ──
        let store2 = Arc::new(
            PostgresPersistenceStore::new(&conn_str)
                .unwrap_or_else(|e| panic!("reopen[{pg_tag}]: second PostgresPersistenceStore::new must succeed — {e}")),
        );

        let uc = QueryHistoryUseCase::new(Arc::clone(&store2), None::<Arc<NoOpVector>>, EngineConfig::default());
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap(); // 2023-11-14, well after start2
        let resp = uc
            .execute_with_time(
                QueryHistoryRequest { agent_id: agent, subject: "reopen-project".into(), predicate: "kickoff".into() },
                now,
            )
            .expect("reopen: query_history on reopened store must not error");

        assert_eq!(resp.entries.len(), 2, "reopen[{pg_tag}]: must have 2 history entries after full store-handle reopen");

        let e1 = resp.entries.iter().find(|e| e.value == serde_json::json!("phase-1")).expect("reopen: phase-1 entry must be present");
        // phase-1's own explicit end (start2) closes its window via a genuine trusted
        // valid-time succession, but no ValidityAssertion::Bound is ever written here (this
        // test only calls append_claim) — a clean succession never supersedes (only
        // HeavyPath / oracle-affirmed supersession writes a Bound assertion). phase-1
        // therefore stays raw-live: its correct status is `Ended` (window closed, no
        // conflict, not the narrowed-current selection), NOT `Superseded` (which means
        // "explicitly bounded/disposed") — this is the fold-derived history design's bug C fix.
        assert_eq!(e1.status, HistoryEntryStatus::Ended, "reopen[{pg_tag}]: phase-1's window has closed via succession but it was never explicitly Bound → Ended, not Superseded");
        assert_eq!(e1.valid_from_granularity, Some(DateGranularity::Month), "reopen[{pg_tag}]: phase-1's start_granularity (Month) must survive a full store-handle reopen");
        // Derived endpoint rule: own end wins over the successor's key (bug A fix), so
        // valid_until_granularity is phase-1's OWN end_granularity (Day), not phase-2's
        // start_granularity — though both happen to be Day here by construction.
        assert_eq!(e1.valid_until_granularity, Some(DateGranularity::Day), "reopen[{pg_tag}]: phase-1's own end_granularity (Day) must survive a full store-handle reopen");

        let e2 = resp.entries.iter().find(|e| e.value == serde_json::json!("phase-2")).expect("reopen: phase-2 entry must be present");
        assert_eq!(e2.status, HistoryEntryStatus::Current, "reopen[{pg_tag}]: phase-2 must be Current after reopen");
        assert_eq!(e2.valid_from_granularity, Some(DateGranularity::Day), "reopen[{pg_tag}]: phase-2's start_granularity (Day) must survive a full store-handle reopen");
        assert_eq!(e2.valid_until_granularity, None, "reopen[{pg_tag}]: phase-2 is open-ended — valid_until_granularity must be None");
    });
}

#[test]
fn postgres_granularity_survives_full_store_reopen_pg16() {
    run_reopen_scenario("16");
}

#[test]
fn postgres_granularity_survives_full_store_reopen_pg18() {
    run_reopen_scenario("18");
}
