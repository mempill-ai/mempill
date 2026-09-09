//! TASK-33 / QA-A (🟡8): Postgres connection-pool exhaustion.
//!
//! With `max_size=2` and a short `connection_timeout`, holds BOTH pooled connections
//! open via two long-running (uncommitted) transactions, then proves a third operation
//! that needs a connection times out cleanly with a typed pool error within roughly the
//! configured timeout — not a hang and not a panic.

use std::time::{Duration, Instant};

use mempill_core::ports::persistence::PersistencePort;
use mempill_postgres::{PoolConfig, PostgresPersistenceStore, PostgresStoreError};
use mempill_types::identity::{AgentId, ClaimRef};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::ImageExt;

const PG_TAG: &str = "16";

fn start_pg() -> (
    testcontainers_modules::testcontainers::Container<Postgres>,
    String,
) {
    let node = Postgres::default()
        .with_tag(PG_TAG)
        .start()
        .unwrap_or_else(|e| panic!("testcontainers: failed to start postgres:{PG_TAG} — {e}"));
    let host = node
        .get_host()
        .unwrap_or_else(|e| panic!("testcontainers: get_host — {e}"));
    let port = node
        .get_host_port_ipv4(5432)
        .unwrap_or_else(|e| panic!("testcontainers: get_host_port_ipv4 — {e}"));
    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");
    (node, conn_str)
}

#[test]
fn pool_exhaustion_third_operation_times_out_with_clean_typed_error() {
    let (_node, conn_str) = start_pg();

    const MAX_SIZE: u32 = 2;
    let timeout = Duration::from_secs(2);

    let store = PostgresPersistenceStore::with_pool_config(
        &conn_str,
        PoolConfig { max_size: MAX_SIZE, connection_timeout: timeout },
    )
    .unwrap_or_else(|e| panic!("with_pool_config must succeed — {e}"));

    assert_eq!(store.pool_max_size(), MAX_SIZE);
    assert_eq!(store.pool_connection_timeout(), timeout);

    // Acquire BOTH pooled connections via two long-running (uncommitted) transactions.
    // begin_atomic pulls one connection from the pool and holds it until commit/rollback.
    let agent1 = AgentId("pool-exhaustion-agent-1".into());
    let agent2 = AgentId("pool-exhaustion-agent-2".into());
    let txn1 = store.begin_atomic(&agent1).expect("txn1 begin_atomic must succeed (pool has capacity)");
    let txn2 = store.begin_atomic(&agent2).expect("txn2 begin_atomic must succeed (pool has capacity)");

    // A THIRD operation needing a connection (a plain read) must block until the
    // configured timeout elapses, then return a clean, typed pool error — not hang,
    // not panic.
    let agent3 = AgentId("pool-exhaustion-agent-3".into());
    let probe_ref = ClaimRef::new_random();
    let started = Instant::now();
    let result = store.load_claim(&agent3, &probe_ref);
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "the third operation must fail once the pool is exhausted (max_size={MAX_SIZE}, both slots held), got Ok"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, PostgresStoreError::Pool(_)),
        "expected a clean PostgresStoreError::Pool timeout, got a different error variant: {err:?}"
    );

    // Elapsed time must be roughly the configured timeout: not near-instant (that would
    // mean the pool didn't actually wait / some other fast-fail path was hit), and not
    // wildly over (that would mean a hang beyond the configured bound).
    assert!(
        elapsed >= timeout.mul_f32(0.5),
        "pool-exhaustion timeout fired too early: elapsed={elapsed:?}, configured timeout={timeout:?} \
         — expected the call to actually wait roughly the configured duration"
    );
    assert!(
        elapsed < timeout + Duration::from_secs(5),
        "pool-exhaustion timeout took far longer than configured: elapsed={elapsed:?}, \
         configured timeout={timeout:?} — possible hang beyond the configured bound"
    );

    // Clean up: release both held connections back to the pool.
    store.rollback(txn1).expect("txn1 rollback must succeed");
    store.rollback(txn2).expect("txn2 rollback must succeed");

    // Pool must recover once slots are freed — sanity check, not the primary assertion.
    let post_release = store.load_claim(&agent3, &probe_ref);
    assert!(post_release.is_ok(), "pool must recover once held connections are released; got {post_release:?}");
}
