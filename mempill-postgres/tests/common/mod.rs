//! Shared test helpers for mempill-postgres integration tests.
//!
//! Provides `with_pg(tag, body)` — starts a pinned `postgres:<tag>` container,
//! builds a `PostgresPersistenceStore`, and runs the caller's closure against it.
//! The container is dropped at the end of the closure (RAII via testcontainers).

use std::sync::Arc;

use mempill_postgres::PostgresPersistenceStore;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::ImageExt;

/// Start a `postgres:<tag>` container, construct an `Arc<PostgresPersistenceStore>`,
/// and invoke `body`. The container lives for the duration of `body`.
///
/// Panics with a clear message if Docker is unavailable or the image is missing.
#[allow(dead_code)] // shared helper — not every test binary calls this one (Rust dead-code analysis is per-binary)
pub fn with_pg<F>(tag: &str, body: F)
where
    F: FnOnce(Arc<PostgresPersistenceStore>),
{
    let node = Postgres::default()
        .with_tag(tag)
        .start()
        .unwrap_or_else(|e| panic!("testcontainers: failed to start postgres:{tag} — {e}"));

    let host = node
        .get_host()
        .unwrap_or_else(|e| panic!("testcontainers: get_host for postgres:{tag} — {e}"));
    let port = node
        .get_host_port_ipv4(5432)
        .unwrap_or_else(|e| panic!("testcontainers: get_host_port_ipv4 for postgres:{tag} — {e}"));

    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");

    let store = Arc::new(PostgresPersistenceStore::new(&conn_str).unwrap_or_else(|e| {
        panic!(
            "PostgresPersistenceStore::new failed for postgres:{tag} — \
             migration or pool error: {e}"
        )
    }));

    body(store);
    // `node` drops here, stopping the container.
}

/// Same as `with_pg` but also provides the connection string to the closure
/// (needed for tests that must open an independent `postgres::Client` for verification).
// Used by the postgres_concurrent test binary but not postgres_conformance; Rust's
// per-binary dead-code analysis flags it in the latter, so allow it here.
#[allow(dead_code)]
pub fn with_pg_and_conn<F>(tag: &str, body: F)
where
    F: FnOnce(Arc<PostgresPersistenceStore>, String),
{
    let node = Postgres::default()
        .with_tag(tag)
        .start()
        .unwrap_or_else(|e| panic!("testcontainers: failed to start postgres:{tag} — {e}"));

    let host = node
        .get_host()
        .unwrap_or_else(|e| panic!("testcontainers: get_host for postgres:{tag} — {e}"));
    let port = node
        .get_host_port_ipv4(5432)
        .unwrap_or_else(|e| panic!("testcontainers: get_host_port_ipv4 for postgres:{tag} — {e}"));

    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");

    let store = Arc::new(PostgresPersistenceStore::new(&conn_str).unwrap_or_else(|e| {
        panic!(
            "PostgresPersistenceStore::new failed for postgres:{tag} — \
             migration or pool error: {e}"
        )
    }));

    body(store, conn_str);
    // `node` drops here, stopping the container.
}
