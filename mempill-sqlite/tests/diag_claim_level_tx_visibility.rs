//! Regression test: claim-level transaction-time visibility.
//!
//! Verifies that `load_subject_line` with `as_of_tx_time = Some(T)` excludes
//! claims whose `transaction_time > T`, so that a bi-temporal `as_of` query does
//! not see claims that did not yet exist at the queried point.
//!
//! ## Bug that this test guards against
//!
//! Before the fix, `load_subject_line` carried no `as_of_tx_time` parameter and
//! returned ALL claims regardless of their `transaction_time`. A query at T2 would
//! return claim B (ingested at T3 > T2), causing the fold to surface `Contested`
//! instead of the correct single-claim result.
//!
//! ## Root cause (resolved)
//!
//! `PersistencePort::load_subject_line` now accepts `as_of_tx_time: Option<DateTime<Utc>>`.
//! The SQLite and Postgres implementations add `AND tx_time <= ?` when `Some(T)` is
//! supplied. `QueryMemoryUseCase` passes `Some(as_of)` on the read path. All write-path
//! callers (ingest, reconcile, adjudication) pass `None` to preserve full current-state
//! visibility, which is required for correct conflict detection and succession.

#![allow(missing_docs)]

use chrono::{TimeZone, Utc};
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_core::application::ingest_claim::IngestClaimUseCase;
use mempill_core::application::query_memory::QueryMemoryUseCase;
use mempill_core::noop::{NoOpOracle, NoOpVector};
use mempill_core::config::EngineConfig;
use mempill_sqlite::store::SqlitePersistenceStore;
use mempill_sqlite::connection;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality,
    ExternalKind, ProvenanceLabel,
};
use std::sync::Arc;

fn dt(y: i32, m: u32, d: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
}

fn make_request(agent: AgentId, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent,
        subject: "acme".into(),
        predicate: "hq".into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None, // no valid_time: relies purely on tx-time ordering
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

/// Regression: as_of query excludes claims ingested after the cutoff.
///
/// Scenario:
///   - T1 = 2020-01-01: claim A ingested (value="berlin").
///   - T3 = 2026-01-01: claim B ingested (value="munich"), conflicts with A (no valid_time).
///   - Query with as_of_tx_time = T2 = 2023-01-01 (strictly between T1 and T3).
///
/// Correct bi-temporal answer: at T2, only claim A exists (B's tx_time T3 > T2).
/// The fold sees only A → TimingUncertain (single live Functional claim, no valid_time).
/// Incorrect (pre-fix) answer: both A and B returned → Contested.
#[tokio::test]
async fn query_as_of_excludes_claims_ingested_after_cutoff() {
    let conn = connection::open_in_memory().expect("in-memory connection must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = AgentId("tx-vis-regression-agent".into());
    let config = EngineConfig::default();

    let ingest_uc = IngestClaimUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpOracle>>,
        None,
        config.clone(),
    );
    let query_uc = QueryMemoryUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpVector>>,
        config.clone(),
    );

    // T1 = 2020-01-01: claim A ingested (berlin)
    let t1 = dt(2020, 1, 1);
    // T2 = 2023-01-01: the as-of query point (between T1 and T3)
    let t2 = dt(2023, 1, 1);
    // T3 = 2026-01-01: claim B ingested (munich), AFTER T2
    let t3 = dt(2026, 1, 1);

    // Ingest claim A at T1 — should be CommittedCheap (first claim).
    let resp_a = ingest_uc
        .execute_with_time(make_request(agent.clone(), "berlin"), t1)
        .expect("ingest A (berlin) at T1 must succeed");
    assert_eq!(
        resp_a.disposition,
        mempill_types::Disposition::CommittedCheap,
        "claim A (berlin) ingested at T1 must be CommittedCheap"
    );

    // Ingest claim B at T3 — conflicts with A (no valid_time), should be Contested.
    let resp_b = ingest_uc
        .execute_with_time(make_request(agent.clone(), "munich"), t3)
        .expect("ingest B (munich) at T3 must succeed");
    assert_eq!(
        resp_b.disposition,
        mempill_types::Disposition::Contested,
        "claim B (munich) ingested at T3 must be Contested (no valid_time conflict with A)"
    );

    // Query at T2 (as_of_tx_time = 2023-01-01).
    // Correct: only claim A exists at T2 → belief is TimingUncertain or Resolved (single live claim).
    let query_at_t2 = QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "hq".into(),
        as_of_tx_time: Some(t2),
        valid_at: None,
    };
    let query_now = t3 + chrono::Duration::days(1);
    let resp = query_uc
        .execute_with_time(query_at_t2, query_now)
        .expect("query at T2 must succeed");

    // Claim B (tx_time=T3 > T2) must be invisible. Only A is present.
    // Single Functional claim with no valid_time → TimingUncertain.
    assert!(
        matches!(resp.belief.status, BeliefStatus::TimingUncertain | BeliefStatus::Resolved),
        "as_of=T2 must yield TimingUncertain or Resolved (only claim A visible); \
         got {:?} — check that load_subject_line applies AND tx_time <= as_of",
        resp.belief.status
    );

    let primary_val = resp.belief.primary.as_ref().map(|b| b.fact.value.clone());
    assert_eq!(
        primary_val,
        Some(serde_json::json!("berlin")),
        "as_of=T2 primary must be 'berlin' (claim A); claim B (munich, tx_time=T3) must be invisible"
    );

    // Sanity: current view (as_of=None) should still see both A and B → Contested.
    let query_current = QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "hq".into(),
        as_of_tx_time: None,
        valid_at: None,
    };
    let resp_current = query_uc
        .execute_with_time(query_current, query_now)
        .expect("current-view query must succeed");
    assert_eq!(
        resp_current.belief.status,
        BeliefStatus::Contested,
        "current-view (as_of=None) with two conflicting no-valid_time claims must be Contested"
    );
}

/// Regression: two claims at different tx-times are not both visible at a between-point as_of.
///
/// This is the no-valid_time variant of the succession_past_instant test. Without the fix,
/// querying at a tx-time between two claim ingestion points would surface Contested because
/// the persistence layer returned both claims regardless of their transaction_time.
///
/// With the fix, the later claim is excluded at the DB layer, so only the earlier claim
/// is visible — giving TimingUncertain (single live Functional claim, no valid_time).
#[tokio::test]
async fn no_valid_time_as_of_mid_point_is_not_contested() {
    let conn = connection::open_in_memory().expect("in-memory connection must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = AgentId("tx-vis-no-vt-agent".into());
    let config = EngineConfig::default();

    let ingest_uc = IngestClaimUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpOracle>>,
        None,
        config.clone(),
    );
    let query_uc = QueryMemoryUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpVector>>,
        config.clone(),
    );

    let t1 = dt(2020, 1, 1); // claim A ingested
    let t2 = dt(2022, 6, 1); // query point: between A (T1) and B (T3)
    let t3 = dt(2025, 1, 1); // claim B ingested

    // Ingest A at T1.
    ingest_uc
        .execute_with_time(make_request(agent.clone(), "value-a"), t1)
        .expect("ingest A at T1");

    // Ingest B at T3 (conflicts with A — same subject/predicate, no valid_time).
    ingest_uc
        .execute_with_time(make_request(agent.clone(), "value-b"), t3)
        .expect("ingest B at T3");

    // Query at T2 (strictly between T1 and T3).
    let resp = query_uc
        .execute_with_time(
            QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "hq".into(),
                as_of_tx_time: Some(t2),
                valid_at: None,
            },
            t3 + chrono::Duration::days(1),
        )
        .expect("query at T2 must succeed");

    // At T2, only claim A exists. Must NOT be Contested.
    assert_ne!(
        resp.belief.status,
        BeliefStatus::Contested,
        "as_of=T2 (between A's T1 and B's T3) must not be Contested: \
         B had not yet been ingested at T2; load_subject_line must exclude it"
    );
    assert_eq!(
        resp.belief.primary.as_ref().map(|b| b.fact.value.clone()),
        Some(serde_json::json!("value-a")),
        "only claim A (value-a) must be visible at as_of=T2"
    );
}
