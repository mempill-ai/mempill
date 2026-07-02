//! US-003 (W2): Postgres connection pool configurability.
//!
//! Proves, against a live Postgres container, that:
//!   1. `PostgresPersistenceStore::new` still applies the v0.3 default pool
//!      settings unchanged (max_size = 20, connection_timeout = 5s).
//!   2. `PostgresPersistenceStore::with_pool_config` actually applies a custom
//!      `PoolConfig` to the underlying r2d2 pool (verified via the pool's own
//!      reported `max_size()` / `connection_timeout()`, not just successful
//!      construction).
//!   3. `PoolConfig { max_size: 0, .. }` is rejected at construction time with
//!      a clear `PostgresStoreError::Config`, without ever touching the network.

use std::time::Duration;

use mempill_postgres::{PoolConfig, PostgresPersistenceStore, PostgresStoreError};
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
        .unwrap_or_else(|e| panic!("testcontainers: get_host for postgres:{PG_TAG} — {e}"));
    let port = node
        .get_host_port_ipv4(5432)
        .unwrap_or_else(|e| panic!("testcontainers: get_host_port_ipv4 for postgres:{PG_TAG} — {e}"));

    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");
    (node, conn_str)
}

/// AC-003-2: omitting pool config preserves current default behavior.
#[test]
fn new_uses_default_pool_settings_matching_v0_3_hardcoded_values() {
    let (_node, conn_str) = start_pg();

    let store = PostgresPersistenceStore::new(&conn_str)
        .unwrap_or_else(|e| panic!("PostgresPersistenceStore::new failed — {e}"));

    assert_eq!(store.pool_max_size(), 20);
    assert_eq!(store.pool_connection_timeout(), Duration::from_secs(5));
}

/// AC-003-1: supplying a custom pool-size and timeout causes the system to use
/// the supplied values instead of the hardcoded defaults — verified via the
/// pool's own reported config, not just that construction succeeded.
#[test]
fn with_pool_config_applies_custom_max_size_and_timeout_to_the_pool() {
    let (_node, conn_str) = start_pg();

    let custom = PoolConfig {
        max_size: 7,
        connection_timeout: Duration::from_secs(13),
    };

    let store = PostgresPersistenceStore::with_pool_config(&conn_str, custom)
        .unwrap_or_else(|e| panic!("PostgresPersistenceStore::with_pool_config failed — {e}"));

    assert_eq!(store.pool_max_size(), 7);
    assert_eq!(store.pool_connection_timeout(), Duration::from_secs(13));
}

/// Edge case: pool size of 0 must be rejected with a clear configuration error,
/// not passed through to the r2d2 pool builder. No live DB should be needed for
/// this to fail — the check runs before the migration connection is attempted.
#[test]
fn with_pool_config_rejects_zero_max_size_without_touching_the_network() {
    let bogus_conn_str = "host=127.0.0.1 port=1 user=nobody dbname=nowhere";

    let result = PostgresPersistenceStore::with_pool_config(
        bogus_conn_str,
        PoolConfig {
            max_size: 0,
            connection_timeout: Duration::from_secs(5),
        },
    );

    match result {
        Err(PostgresStoreError::Config(msg)) => {
            assert!(msg.contains("max_size"), "error message should mention max_size: {msg}");
        }
        Err(other) => panic!("expected PostgresStoreError::Config for max_size=0, got a different error: {other}"),
        Ok(_) => panic!("expected PostgresStoreError::Config for max_size=0, got Ok(..)"),
    }
}
