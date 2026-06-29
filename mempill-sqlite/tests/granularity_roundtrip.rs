//! granularity_roundtrip — SQLite store-level round-trip test for `DateGranularity`.
//!
//! Verifies that:
//! 1. A claim with `start_granularity = Some(Month)` and an open (None) end persists
//!    and reads back with the identical granularity values.
//! 2. A claim with both granularities set (`start = Day`, `end = Year`) round-trips.
//! 3. A claim with no granularities (`None`/`None`) round-trips without any granularity
//!    columns being set (legacy / instant-precise dates).
//! 4. The migration-upgrade invariant: old rows (inserted before v3) read back as
//!    `start_granularity = None` and `end_granularity = None`.

use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_sqlite::open_default_in_memory;
use mempill_types::{
    AgentId, Cardinality, Confidence, Criticality, DateGranularity, ExternalKind, ProvenanceLabel,
    ValidTime,
};

// ── Helper ────────────────────────────────────────────────────────────────────

/// Build an `IngestClaimRequest` with the supplied `ValidTime`.
fn make_ingest_req(
    agent: AgentId,
    subject: &str,
    predicate: &str,
    valid_time: ValidTime,
) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent,
        subject: subject.into(),
        predicate: predicate.into(),
        value: serde_json::json!("test-value"),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: Some(valid_time),
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.8 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

// ── Test 1: Month start, open end ─────────────────────────────────────────────

/// Ingest a claim with `start_granularity = Month` and no end; read it back and
/// assert the granularity is preserved.
#[tokio::test]
async fn sqlite_granularity_month_start_open_end_round_trips() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("gran-rt-agent-1".into());

    // "2020-03" → Month granularity, start-of-month midnight UTC.
    let (start_dt, gran) = mempill_types::parse_valid_time_date("2020-03").unwrap();
    assert_eq!(gran, DateGranularity::Month);

    let valid_time = ValidTime {
        start: Some(start_dt),
        end: None,
        valid_time_confidence: 0.9,
        start_granularity: Some(DateGranularity::Month),
        end_granularity: None,
    };

    let ingest_resp = engine
        .ingest_claim(make_ingest_req(agent.clone(), "person:42", "birth_month", valid_time))
        .await
        .expect("ingest must succeed");

    // Read back via query_memory.
    let query_resp = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "person:42".into(),
            predicate: "birth_month".into(),
            as_of_tx_time: None,
            valid_at: None,
        })
        .await
        .expect("query must succeed");

    let primary = query_resp.belief.primary.expect("primary belief must be present");
    assert_eq!(primary.claim_ref, ingest_resp.claim_ref, "claim_ref must match");

    let vt = primary.valid_time;
    assert_eq!(
        vt.start_granularity,
        Some(DateGranularity::Month),
        "start_granularity must round-trip as Month; got {:?}",
        vt.start_granularity
    );
    assert_eq!(
        vt.end_granularity, None,
        "end_granularity must be None for open end; got {:?}",
        vt.end_granularity
    );
    assert_eq!(vt.start, Some(start_dt), "start datetime must be preserved");
}

// ── Test 2: Day start + Year end ─────────────────────────────────────────────

/// Ingest a claim with `start_granularity = Day` and `end_granularity = Year`;
/// read it back and assert both granularities are preserved.
#[tokio::test]
async fn sqlite_granularity_day_start_year_end_round_trips() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("gran-rt-agent-2".into());

    let (start_dt, _) = mempill_types::parse_valid_time_date("2019-06-15").unwrap();
    let (end_dt, _) = mempill_types::parse_valid_time_date("2023").unwrap();

    let valid_time = ValidTime {
        start: Some(start_dt),
        end: Some(end_dt),
        valid_time_confidence: 0.8,
        start_granularity: Some(DateGranularity::Day),
        end_granularity: Some(DateGranularity::Year),
    };

    let ingest_resp = engine
        .ingest_claim(make_ingest_req(agent.clone(), "project:99", "active_window", valid_time))
        .await
        .expect("ingest must succeed");

    let query_resp = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "project:99".into(),
            predicate: "active_window".into(),
            as_of_tx_time: None,
            valid_at: None,
        })
        .await
        .expect("query must succeed");

    let primary = query_resp.belief.primary.expect("primary belief must be present");
    assert_eq!(primary.claim_ref, ingest_resp.claim_ref);

    let vt = primary.valid_time;
    assert_eq!(
        vt.start_granularity,
        Some(DateGranularity::Day),
        "start_granularity must round-trip as Day; got {:?}",
        vt.start_granularity
    );
    assert_eq!(
        vt.end_granularity,
        Some(DateGranularity::Year),
        "end_granularity must round-trip as Year; got {:?}",
        vt.end_granularity
    );
}

// ── Test 3: No granularity (instant-precise or absent) ──────────────────────

/// A claim with both granularities `None` round-trips without acquiring any granularity.
#[tokio::test]
async fn sqlite_granularity_none_round_trips() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("gran-rt-agent-3".into());

    let valid_time = ValidTime {
        start: None,
        end: None,
        valid_time_confidence: 0.0,
        start_granularity: None,
        end_granularity: None,
    };

    engine
        .ingest_claim(make_ingest_req(agent.clone(), "event:1", "timestamp", valid_time))
        .await
        .expect("ingest must succeed");

    let query_resp = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "event:1".into(),
            predicate: "timestamp".into(),
            as_of_tx_time: None,
            valid_at: None,
        })
        .await
        .expect("query must succeed");

    let primary = query_resp.belief.primary.expect("primary belief must be present");
    let vt = primary.valid_time;
    assert_eq!(vt.start_granularity, None, "start_granularity must remain None");
    assert_eq!(vt.end_granularity, None, "end_granularity must remain None");
}

// ── Test 4: Migration-upgrade invariant — old rows with NULL columns ─────────

/// Simulate an "old row" by writing directly to the DB before the granularity columns
/// existed (i.e. inserting NULL into both columns) and verifying the read path returns None.
///
/// In the test database (in-memory, always fresh), this scenario is produced by inserting
/// with explicitly NULL granularity columns.  The v3 migration adds the columns as nullable,
/// so old rows are exactly rows with NULLs in those columns.
#[tokio::test]
async fn sqlite_granularity_null_columns_read_as_none() {
    use mempill_sqlite::connection::open_in_memory;
    use mempill_sqlite::SqlitePersistenceStore;
    use mempill_core::ports::persistence::PersistencePort as _;
    use mempill_types::{
        claim::{Claim, Confidence, Criticality, Cardinality, Fact},
        identity::ClaimRef,
        provenance::{ExternalAnchor, ProvenanceLabel, ExternalKind},
        time::{TransactionTime, ValidTime},
    };

    let conn = open_in_memory().expect("in-memory connection must open");
    let store = SqlitePersistenceStore::new(conn);
    let agent = AgentId("gran-upgrade-agent".into());

    // Craft a claim with no granularity (simulates old rows inserted before v3).
    let now = chrono::Utc::now();
    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact {
            subject: "legacy:subject".into(),
            predicate: "legacy:predicate".into(),
            value: serde_json::json!("legacy-value"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(now),
        ValidTime {
            start: Some(now - chrono::Duration::days(30)),
            end: None,
            valid_time_confidence: 0.7,
            start_granularity: None,
            end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.7 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );

    let claim_ref = claim.claim_ref().clone();

    // Insert via the store (granularity columns will be NULL since both are None).
    let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
    store.append_claim(&mut txn, &claim).expect("append_claim must succeed");
    store.commit(txn).expect("commit must succeed");

    // Read back and verify None is returned for both granularity fields.
    let loaded = store
        .load_claim(&agent, &claim_ref)
        .expect("load_claim must succeed")
        .expect("claim must be present");

    assert_eq!(
        loaded.valid_time().start_granularity,
        None,
        "legacy row with NULL start_granularity must read back as None"
    );
    assert_eq!(
        loaded.valid_time().end_granularity,
        None,
        "legacy row with NULL end_granularity must read back as None"
    );
}
