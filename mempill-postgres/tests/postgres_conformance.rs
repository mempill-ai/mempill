//! Conformance proof: run the shared `PersistencePort` harness against the Postgres adapter.
//!
//! Proves behavioral parity with SQLite (A43): the SAME 12 sub-tests that pass against
//! `SqlitePersistenceStore` must also pass against `PostgresPersistenceStore` on a real
//! PG16 instance started via testcontainers.
//!
//! Container: postgres:16 (pinned via `ImageExt::with_tag("16")`).
//! Default image creds: user=postgres, password=postgres, db=postgres.
//! Migration (V1__initial_schema.sql) is applied by `PostgresPersistenceStore::new`.

use mempill_core::testing::conformance::run_persistence_conformance;
use mempill_postgres::PostgresPersistenceStore;
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::postgres::Postgres;

#[test]
fn postgres_passes_conformance() {
    // Start a postgres:16 container. Fails loudly (panic) if Docker is unavailable.
    let node = Postgres::default()
        .with_tag("16")
        .start()
        .expect("testcontainers: failed to start postgres:16 container — is Docker running?");

    let host = node
        .get_host()
        .expect("testcontainers: failed to get container host");
    let port = node
        .get_host_port_ipv4(5432)
        .expect("testcontainers: failed to get mapped port for 5432");

    // URI form: postgresql://user:password@host:port/dbname
    // Default image creds: user=postgres, password=postgres, db=postgres.
    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");

    // Bootstrap: runs V1__initial_schema.sql via refinery, then builds the r2d2 pool.
    let store = PostgresPersistenceStore::new(&conn_str)
        .expect("PostgresPersistenceStore::new must succeed: migration applied + pool built");

    // Run all 12 conformance sub-tests against real PG16.
    // Any sub-test failure panics with a descriptive message indicating which sub-test failed.
    run_persistence_conformance(&store);
}
