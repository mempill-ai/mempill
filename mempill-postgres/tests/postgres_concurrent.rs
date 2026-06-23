//! Postgres-only concurrency proofs (topology-b, A40–A42).
//!
//! These tests exercise concurrent write paths that are impossible with the SQLite adapter
//! (single-connection serialization). They prove that the Postgres pool enables genuine
//! parallel writes across distinct agent_ids, while the advisory lock correctly serializes
//! same-agent writes with no race on `stream_seq`.
//!
//! Two tests, each using its own container (per-test isolation):
//!
//! 1. `concurrent_cross_agent_writes_both_succeed` — two threads, two distinct agent_ids,
//!    concurrent `begin_atomic + append_claim + commit`. Both must succeed (topology-b proof).
//!
//! 2. `advisory_lock_same_agent_serializes` — two threads, same agent_id, concurrent
//!    `begin_atomic + append_claim + append_ledger_entry + commit`. Both must succeed;
//!    the agent must have exactly 2 claims and `stream_seq` values {1, 2} with no duplicate
//!    or gap (advisory lock + MAX+1 serialization proof).

use std::collections::HashSet;
use std::sync::Arc;

use mempill_postgres::PostgresPersistenceStore;
use mempill_core::ports::persistence::PersistencePort;
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    disposition::Disposition,
    identity::{AgentId, ClaimRef},
    ledger::{LedgerEntry, LedgerEventKind},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
};
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::postgres::Postgres;
use chrono::Utc;
use uuid::Uuid;

// ── Shared helpers ─────────────────────────────────────────────────────────────

fn make_claim(agent_id: &AgentId, subject: &str, predicate: &str) -> Claim {
    Claim::new(
        ClaimRef::new_random(),
        agent_id.clone(),
        Fact {
            subject: subject.to_owned(),
            predicate: predicate.to_owned(),
            value: serde_json::json!("concurrent-test-value"),
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

fn make_ledger_entry(agent_id: &AgentId, claim_ref: &ClaimRef) -> LedgerEntry {
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

// ── Test 1: concurrent cross-agent writes (topology-b proof) ──────────────────

/// Spawn two threads with distinct `agent_id`s; each does `begin_atomic → append_claim → commit`
/// concurrently on the same shared `PostgresPersistenceStore`.
///
/// Both threads MUST succeed (no deadlock, no pool exhaustion, no serialization error).
/// This is impossible with SQLite's single-connection mutex — it proves topology-b.
#[test]
fn concurrent_cross_agent_writes_both_succeed() {
    let node = Postgres::default()
        .with_tag("16")
        .start()
        .expect("testcontainers: start postgres:16");

    let host = node.get_host().expect("get_host");
    let port = node.get_host_port_ipv4(5432).expect("get_host_port_ipv4(5432)");
    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");

    let store = Arc::new(
        PostgresPersistenceStore::new(&conn_str)
            .expect("PostgresPersistenceStore::new"),
    );

    let agent_alpha = AgentId("agent-alpha".into());
    let agent_beta = AgentId("agent-beta".into());

    let s1: Arc<PostgresPersistenceStore> = Arc::clone(&store);
    let s2: Arc<PostgresPersistenceStore> = Arc::clone(&store);

    let a1 = agent_alpha.clone();
    let a2 = agent_beta.clone();

    let h1 = std::thread::spawn(move || {
        let claim = make_claim(&a1, "cross-agent", "alpha-predicate");
        let claim_ref = claim.claim_ref().clone();
        let mut txn = s1.begin_atomic(&a1).expect("h1: begin_atomic");
        s1.append_claim(&mut txn, &claim).expect("h1: append_claim");
        s1.commit(txn).expect("h1: commit");
        claim_ref
    });

    let h2 = std::thread::spawn(move || {
        let claim = make_claim(&a2, "cross-agent", "beta-predicate");
        let claim_ref = claim.claim_ref().clone();
        let mut txn = s2.begin_atomic(&a2).expect("h2: begin_atomic");
        s2.append_claim(&mut txn, &claim).expect("h2: append_claim");
        s2.commit(txn).expect("h2: commit");
        claim_ref
    });

    let ref_alpha = h1.join().expect("h1: thread must not panic");
    let ref_beta = h2.join().expect("h2: thread must not panic");

    // Verify: each agent can load their claim.
    let loaded_alpha = store
        .load_claim(&agent_alpha, &ref_alpha)
        .expect("load_claim alpha: must not error")
        .expect("load_claim alpha: must return Some — cross-agent write must be durable");

    let loaded_beta = store
        .load_claim(&agent_beta, &ref_beta)
        .expect("load_claim beta: must not error")
        .expect("load_claim beta: must return Some — cross-agent write must be durable");

    assert_eq!(
        loaded_alpha.claim_ref(), &ref_alpha,
        "agent-alpha claim_ref must round-trip"
    );
    assert_eq!(
        loaded_beta.claim_ref(), &ref_beta,
        "agent-beta claim_ref must round-trip"
    );
}

// ── Test 2: same-agent concurrent writes — advisory lock + stream_seq proof ──

/// Spawn two threads with the SAME `agent_id`; each does `begin_atomic → append_claim →
/// append_ledger_entry → commit` concurrently.
///
/// Expected outcome:
/// - Both threads succeed (no deadlock — advisory lock BLOCKS then releases, not fails).
/// - The agent has exactly 2 claims.
/// - The `stream_seq` values in `ledger_entries` are {1, 2} — no duplicate, no gap.
///   This proves the advisory lock serialized the MAX+1 computation correctly.
/// - Completes without deadlock (advisory lock is blocking, not failing).
#[test]
fn advisory_lock_same_agent_serializes() {
    let node = Postgres::default()
        .with_tag("16")
        .start()
        .expect("testcontainers: start postgres:16");

    let host = node.get_host().expect("get_host");
    let port = node.get_host_port_ipv4(5432).expect("get_host_port_ipv4(5432)");
    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");

    let store = Arc::new(
        PostgresPersistenceStore::new(&conn_str)
            .expect("PostgresPersistenceStore::new"),
    );

    let agent = AgentId("agent-same".into());

    let s1: Arc<PostgresPersistenceStore> = Arc::clone(&store);
    let s2: Arc<PostgresPersistenceStore> = Arc::clone(&store);

    let a1 = agent.clone();
    let a2 = agent.clone();

    // Use a barrier to maximize the chance of true concurrent execution.
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let b1 = Arc::clone(&barrier);
    let b2 = Arc::clone(&barrier);

    let h1 = std::thread::spawn(move || {
        b1.wait(); // synchronize start
        let claim = make_claim(&a1, "same-agent", "predicate-t1");
        let claim_ref = claim.claim_ref().clone();
        let entry = make_ledger_entry(&a1, &claim_ref);
        let mut txn = s1.begin_atomic(&a1).expect("h1: begin_atomic");
        s1.append_claim(&mut txn, &claim).expect("h1: append_claim");
        s1.append_ledger_entry(&mut txn, &entry).expect("h1: append_ledger_entry");
        s1.commit(txn).expect("h1: commit");
        claim_ref
    });

    let h2 = std::thread::spawn(move || {
        b2.wait(); // synchronize start
        let claim = make_claim(&a2, "same-agent", "predicate-t2");
        let claim_ref = claim.claim_ref().clone();
        let entry = make_ledger_entry(&a2, &claim_ref);
        let mut txn = s2.begin_atomic(&a2).expect("h2: begin_atomic");
        s2.append_claim(&mut txn, &claim).expect("h2: append_claim");
        s2.append_ledger_entry(&mut txn, &entry).expect("h2: append_ledger_entry");
        s2.commit(txn).expect("h2: commit");
        claim_ref
    });

    // Both threads must complete without deadlock (advisory lock blocks, then releases).
    let ref1 = h1
        .join()
        .expect("h1: must not deadlock or panic");
    let ref2 = h2
        .join()
        .expect("h2: must not deadlock or panic");

    // Verify: the agent has exactly 2 claims (one per thread).
    let claims_t1 = store
        .load_subject_line(&agent, "same-agent", "predicate-t1")
        .expect("load_subject_line t1");
    let claims_t2 = store
        .load_subject_line(&agent, "same-agent", "predicate-t2")
        .expect("load_subject_line t2");

    assert_eq!(
        claims_t1.len(), 1,
        "same-agent: predicate-t1 must have exactly 1 claim"
    );
    assert_eq!(
        claims_t2.len(), 1,
        "same-agent: predicate-t2 must have exactly 1 claim"
    );
    assert_eq!(
        claims_t1[0].claim_ref(), &ref1,
        "same-agent: claim ref1 must round-trip"
    );
    assert_eq!(
        claims_t2[0].claim_ref(), &ref2,
        "same-agent: claim ref2 must round-trip"
    );

    // Verify: ledger has exactly 2 entries for this agent.
    let ledger = store
        .load_ledger(&agent, None, 100)
        .expect("load_ledger for same-agent");

    assert_eq!(
        ledger.len(), 2,
        "same-agent: ledger must have exactly 2 entries (one per thread)"
    );

    // Query stream_seq values directly via an independent postgres::Client
    // (pool field is pub(crate); use a fresh client against the same DB).
    let mut verification_client = postgres::Client::connect(&conn_str, postgres::NoTls)
        .expect("verification client: connect");

    let rows = verification_client
        .query(
            "SELECT stream_seq FROM ledger_entries WHERE agent_id = $1 ORDER BY stream_seq ASC",
            &[&"agent-same"],
        )
        .expect("SELECT stream_seq");

    assert_eq!(rows.len(), 2, "must have exactly 2 ledger rows for agent-same");

    let seq_values: HashSet<i64> = rows.iter().map(|r| r.get::<_, i64>(0)).collect();
    let expected: HashSet<i64> = [1i64, 2i64].iter().cloned().collect();

    assert_eq!(
        seq_values, expected,
        "stream_seq values must be {{1, 2}} — no duplicate, no gap. \
         Advisory lock + MAX+1 assignment must have serialized correctly. \
         Actual: {seq_values:?}"
    );
}
