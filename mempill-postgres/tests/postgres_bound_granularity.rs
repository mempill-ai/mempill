//! postgres_bound_granularity — Postgres store-level round-trip + legacy-row tests for
//! `AssertionKind::Bound::bound_at_granularity` (TASK-33-W5-LIB-R2, DIAG-5).
//!
//! Requires Docker; panics (does not skip) if testcontainers cannot start postgres.
//! Run with:
//!   cargo test -p mempill-postgres --test postgres_bound_granularity

mod common;

use mempill_core::ports::persistence::PersistencePort;
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    identity::{AgentId, ClaimRef},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
    AssertionKind, DateGranularity, ValidityAssertion,
};

fn make_claim(agent: AgentId, subject: &str, predicate: &str) -> Claim {
    Claim::new(
        ClaimRef::new_random(),
        agent,
        Fact { subject: subject.into(), predicate: predicate.into(), value: serde_json::json!("v") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(chrono::Utc::now()),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.8 },
        Criticality::Medium,
        vec![],
        None,
        None,
    )
}

/// `bound_at_granularity` round-trips through the real Postgres store.
fn run_roundtrip(store: &mempill_postgres::PostgresPersistenceStore) {
    let agent = AgentId("pg-bound-gran-rt-agent".into());
    let claim = make_claim(agent.clone(), "pg-bound:1", "closed_at");
    let claim_ref = claim.claim_ref().clone();

    let bound_at = chrono::Utc::now();
    let assertion = ValidityAssertion {
        assertion_ref: uuid::Uuid::new_v4(),
        agent_id: agent.clone(),
        target_claim: claim_ref.clone(),
        kind: AssertionKind::Bound { bound_at, bound_at_granularity: Some(DateGranularity::Month) },
        provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
        asserted_at: TransactionTime(bound_at),
    };

    {
        let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
        store.append_claim(&mut txn, &claim).expect("append_claim must succeed");
        store.append_validity_assertion(&mut txn, &assertion).expect("append_validity_assertion must succeed");
        store.commit(txn).expect("commit must succeed");
    }

    let loaded = store
        .load_validity_assertions_for(&agent, &claim_ref)
        .expect("load_validity_assertions_for must succeed");
    assert_eq!(loaded.len(), 1);
    match loaded[0].kind {
        AssertionKind::Bound { bound_at_granularity, .. } => {
            assert_eq!(
                bound_at_granularity,
                Some(DateGranularity::Month),
                "bound_at_granularity must round-trip as Month on Postgres; got {bound_at_granularity:?}"
            );
        }
        ref other => panic!("expected Bound, got {other:?}"),
    }
}

/// A `Bound` row written directly via SQL (simulating a row from BEFORE this field existed —
/// no `bound_at_granularity` value supplied, defaulting to the column's NULL) must still
/// deserialize cleanly via `load_validity_assertions_for`, with `bound_at_granularity = None`.
fn run_legacy_row_proof(store: &mempill_postgres::PostgresPersistenceStore, conn_str: &str) {
    let agent = AgentId("pg-bound-gran-legacy-agent".into());
    let claim = make_claim(agent.clone(), "pg-bound:legacy", "closed_at");
    let claim_ref = claim.claim_ref().clone();

    {
        let mut txn = store.begin_atomic(&agent).expect("begin_atomic must succeed");
        store.append_claim(&mut txn, &claim).expect("append_claim must succeed");
        store.commit(txn).expect("commit must succeed");
    }

    // Write the validity_assertions row directly via SQL, omitting bound_at_granularity
    // entirely — exactly the shape of a pre-v4 row (the column exists post-migration, but
    // this INSERT never references it, so it lands as NULL).
    let mut raw = postgres::Client::connect(conn_str, postgres::NoTls)
        .expect("raw postgres::Client::connect must succeed");
    let bound_at = chrono::Utc::now();
    raw.execute(
        "INSERT INTO validity_assertions (
            assertion_id, agent_id, target_claim_id, assertion_kind, bound_at, reopen_at,
            provenance_label, value_confidence, valid_time_confidence, asserted_at
        ) VALUES ($1, $2, $3, 'Bound', $4, NULL, 'External_ExternalFirstHand', 1.0, 1.0, $5)",
        &[
            &uuid::Uuid::new_v4().to_string(),
            &agent.0.as_str(),
            &claim_ref.0.to_string(),
            &bound_at.to_rfc3339(),
            &bound_at.to_rfc3339(),
        ],
    ).expect("legacy-shaped Bound row insert (no bound_at_granularity) must succeed");

    let loaded = store
        .load_validity_assertions_for(&agent, &claim_ref)
        .expect("load_validity_assertions_for must succeed on a legacy row");
    assert_eq!(loaded.len(), 1);
    match loaded[0].kind {
        AssertionKind::Bound { bound_at_granularity, .. } => {
            assert_eq!(bound_at_granularity, None, "legacy row (no bound_at_granularity) must read back as None, not fail to parse");
        }
        ref other => panic!("expected Bound, got {other:?}"),
    }
}

// ── Test matrix ───────────────────────────────────────────────────────────────

#[test]
fn postgres_bound_granularity_roundtrip_pg16() {
    common::with_pg("16", |store| {
        run_roundtrip(&store);
    });
}

#[test]
fn postgres_bound_granularity_legacy_row_pg16() {
    common::with_pg_and_conn("16", |store, conn_str| {
        run_legacy_row_proof(&store, &conn_str);
    });
}
