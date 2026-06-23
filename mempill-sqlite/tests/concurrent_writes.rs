//! Integration tests for concurrent write correctness (DEFECT-THREAD-1 fix).
//!
//! Verifies that two concurrent writes with DIFFERENT agent_ids both succeed after
//! the store_write_lock fix in engine_handle.rs.
//!
//! Root cause of original defect: two concurrent writes with different agent_ids acquired
//! DIFFERENT per-agent locks but shared ONE SQLite connection. The second `begin_atomic`
//! call returned TxnAlreadyOpen because the first transaction was still open.
//!
//! Fix: store_write_lock (Arc<tokio::sync::Mutex<()>>) in EngineHandle is acquired
//! BEFORE the per-agent lock in both ingest_claim and reconcile. This serializes all
//! writes at the store level. Reads (query_memory, query_audit) remain lock-free.

use mempill_core::application::IngestClaimRequest;
use mempill_sqlite::open_default_in_memory;
use mempill_types::{AgentId, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel};

fn make_req(agent_id: &str, subject: &str, predicate: &str, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: AgentId(agent_id.into()),
        subject: subject.into(),
        predicate: predicate.into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence {
            value_confidence: 0.9,
            valid_time_confidence: 0.0,
        },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

/// DEFECT-THREAD-1 fix: concurrent writes with DIFFERENT agent_ids must both succeed.
///
/// Previously, the second tokio::join! branch would error with TxnAlreadyOpen
/// because both branches entered begin_atomic on the same SQLite connection before
/// either committed. After the store_write_lock fix they serialize correctly.
#[tokio::test]
async fn concurrent_writes_different_agent_ids_succeed() {
    let engine = open_default_in_memory().expect("in-memory engine must open");

    let engine_a = engine.clone();
    let engine_b = engine.clone();

    let req_a = make_req("agent-alpha", "user:alice", "city", "Berlin");
    let req_b = make_req("agent-beta", "user:bob", "city", "Tokyo");

    let (res_a, res_b) = tokio::join!(
        engine_a.ingest_claim(req_a),
        engine_b.ingest_claim(req_b),
    );

    assert!(
        res_a.is_ok(),
        "agent-alpha write must succeed, got: {:?}",
        res_a
    );
    assert!(
        res_b.is_ok(),
        "agent-beta write must succeed (was TxnAlreadyOpen before fix), got: {:?}",
        res_b
    );
}

/// Same-agent concurrent writes must also serialize and both succeed.
///
/// The per-agent lock already guaranteed same-agent serialization. This test
/// ensures the store_write_lock addition did not break that existing guarantee.
#[tokio::test]
async fn concurrent_writes_same_agent_id_succeed() {
    let engine = open_default_in_memory().expect("in-memory engine must open");

    let engine_a = engine.clone();
    let engine_b = engine.clone();

    // Different subjects/predicates so no conflict — purely testing lock correctness.
    let req_a = make_req("agent-same", "user:carol", "city", "Paris");
    let req_b = make_req("agent-same", "user:carol", "score", "42");

    let (res_a, res_b) = tokio::join!(
        engine_a.ingest_claim(req_a),
        engine_b.ingest_claim(req_b),
    );

    assert!(res_a.is_ok(), "same-agent write A must succeed, got: {:?}", res_a);
    assert!(res_b.is_ok(), "same-agent write B must succeed, got: {:?}", res_b);
}
