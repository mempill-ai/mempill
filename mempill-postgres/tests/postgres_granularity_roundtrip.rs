//! postgres_granularity_roundtrip — Postgres store-level round-trip tests for `DateGranularity`.
//!
//! Tests the persistence adapter directly (not via EngineHandle) to avoid the pre-existing
//! `DateTime<Utc>` TEXT-column serialization issue in `query_memory` → `load_subject_line`.
//!
//! Strategy: use `append_claim` + `load_claim` directly on the `PostgresPersistenceStore`
//! to verify that the two granularity columns are stored and retrieved correctly.
//!
//! Requires Docker; panics (does not skip) if testcontainers cannot start postgres.
//! Run with:
//!   cargo test -p mempill-postgres --test postgres_granularity_roundtrip

mod common;

use mempill_core::ports::persistence::PersistencePort;
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    identity::{AgentId, ClaimRef},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
    DateGranularity,
};

// ── Internal helpers ──────────────────────────────────────────────────────────

fn make_claim(agent: AgentId, subject: &str, predicate: &str, valid_time: ValidTime) -> Claim {
    Claim::new(
        ClaimRef::new_random(),
        agent,
        Fact {
            subject: subject.into(),
            predicate: predicate.into(),
            value: serde_json::json!("pg-gran-test-value"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(chrono::Utc::now()),
        valid_time,
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.8 },
        Criticality::Medium,
        vec![],
        None,
        None,
    )
}

/// Run the granularity round-trip assertions against a live Postgres store.
///
/// Uses `append_claim` + `load_claim` directly to avoid the pre-existing
/// `DateTime<Utc>` TEXT-column issue in the query-path `load_subject_line`.
fn run_roundtrip(store: &mempill_postgres::PostgresPersistenceStore) {
    let agent = AgentId("pg-gran-rt-agent".into());

    // ── 1. Month start, open end ──────────────────────────────────────────────

    let (start_month, _) = mempill_types::parse_valid_time_date("2020-03").unwrap();
    let vt_month = ValidTime {
        start: Some(start_month),
        end: None,
        valid_time_confidence: 0.9,
        start_granularity: Some(DateGranularity::Month),
        end_granularity: None,
    };

    let claim_month = make_claim(agent.clone(), "pg-person:1", "birth_month", vt_month);
    let claim_ref_month = claim_month.claim_ref().clone();

    {
        let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
        store.append_claim(&mut txn, &claim_month).expect("append_claim must succeed");
        store.commit(txn).expect("commit must succeed");
    }

    let loaded_month = store
        .load_claim(&agent, &claim_ref_month)
        .expect("load_claim must succeed")
        .expect("claim must be present");

    assert_eq!(
        loaded_month.valid_time().start_granularity,
        Some(DateGranularity::Month),
        "start_granularity must round-trip as Month on Postgres; got {:?}",
        loaded_month.valid_time().start_granularity
    );
    assert_eq!(
        loaded_month.valid_time().end_granularity,
        None,
        "end_granularity must be None for open end on Postgres; got {:?}",
        loaded_month.valid_time().end_granularity
    );
    assert_eq!(
        loaded_month.valid_time().start,
        Some(start_month),
        "start datetime must be preserved on Postgres"
    );

    // ── 2. Day start + Year end ───────────────────────────────────────────────

    let (start_day, _) = mempill_types::parse_valid_time_date("2019-06-15").unwrap();
    let (end_year, _) = mempill_types::parse_valid_time_date("2023").unwrap();
    let vt_day_year = ValidTime {
        start: Some(start_day),
        end: Some(end_year),
        valid_time_confidence: 0.8,
        start_granularity: Some(DateGranularity::Day),
        end_granularity: Some(DateGranularity::Year),
    };

    let claim_day = make_claim(agent.clone(), "pg-project:1", "active_window", vt_day_year);
    let claim_ref_day = claim_day.claim_ref().clone();

    {
        let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
        store.append_claim(&mut txn, &claim_day).expect("append_claim must succeed");
        store.commit(txn).expect("commit must succeed");
    }

    let loaded_day = store
        .load_claim(&agent, &claim_ref_day)
        .expect("load_claim must succeed")
        .expect("claim must be present");

    assert_eq!(
        loaded_day.valid_time().start_granularity,
        Some(DateGranularity::Day),
        "start_granularity must round-trip as Day on Postgres; got {:?}",
        loaded_day.valid_time().start_granularity
    );
    assert_eq!(
        loaded_day.valid_time().end_granularity,
        Some(DateGranularity::Year),
        "end_granularity must round-trip as Year on Postgres; got {:?}",
        loaded_day.valid_time().end_granularity
    );

    // ── 3. No granularity (None/None) ─────────────────────────────────────────

    let vt_none = ValidTime {
        start: None,
        end: None,
        valid_time_confidence: 0.0,
        start_granularity: None,
        end_granularity: None,
    };

    let claim_none = make_claim(agent.clone(), "pg-event:1", "timestamp", vt_none);
    let claim_ref_none = claim_none.claim_ref().clone();

    {
        let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
        store.append_claim(&mut txn, &claim_none).expect("append_claim must succeed");
        store.commit(txn).expect("commit must succeed");
    }

    let loaded_none = store
        .load_claim(&agent, &claim_ref_none)
        .expect("load_claim must succeed")
        .expect("claim must be present");

    assert_eq!(
        loaded_none.valid_time().start_granularity,
        None,
        "start_granularity must remain None when not set on Postgres"
    );
    assert_eq!(
        loaded_none.valid_time().end_granularity,
        None,
        "end_granularity must remain None when not set on Postgres"
    );

    // ── 4. Instant granularity ────────────────────────────────────────────────

    let (start_instant, gran) = mempill_types::parse_valid_time_date("2024-05-15T10:30:00Z").unwrap();
    assert_eq!(gran, DateGranularity::Instant);
    let vt_instant = ValidTime {
        start: Some(start_instant),
        end: None,
        valid_time_confidence: 1.0,
        start_granularity: Some(DateGranularity::Instant),
        end_granularity: None,
    };

    let claim_instant = make_claim(agent.clone(), "pg-event:2", "occurred_at", vt_instant);
    let claim_ref_instant = claim_instant.claim_ref().clone();

    {
        let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
        store.append_claim(&mut txn, &claim_instant).expect("append_claim must succeed");
        store.commit(txn).expect("commit must succeed");
    }

    let loaded_instant = store
        .load_claim(&agent, &claim_ref_instant)
        .expect("load_claim must succeed")
        .expect("claim must be present");

    assert_eq!(
        loaded_instant.valid_time().start_granularity,
        Some(DateGranularity::Instant),
        "start_granularity must round-trip as Instant on Postgres; got {:?}",
        loaded_instant.valid_time().start_granularity
    );
}

// ── Test matrix ───────────────────────────────────────────────────────────────

/// Granularity round-trip on postgres:16.
#[test]
fn postgres_granularity_roundtrip_pg16() {
    common::with_pg("16", |store| {
        run_roundtrip(&store);
    });
}

/// Granularity round-trip on postgres:18.
#[test]
fn postgres_granularity_roundtrip_pg18() {
    common::with_pg("18", |store| {
        run_roundtrip(&store);
    });
}
