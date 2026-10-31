//! Shared test helpers for mempill-postgres integration tests.
//!
//! Provides `with_pg(tag, body)` — starts a pinned `postgres:<tag>` container,
//! builds a `PostgresPersistenceStore`, and runs the caller's closure against it.
//! The container is dropped at the end of the closure (RAII via testcontainers).

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use mempill_postgres::PostgresPersistenceStore;
use postgres::{Client, NoTls};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::SyncRunner;
use testcontainers_modules::testcontainers::{Container, ImageExt, TestcontainersError};

const READY_DEADLINE: Duration = Duration::from_secs(30);
const READY_BACKOFF: Duration = Duration::from_millis(250);
const PORT_DEADLINE: Duration = Duration::from_secs(10);
const PORT_BACKOFF: Duration = Duration::from_millis(200);

/// Wait until the container's DB accepts a TCP connection, then build the store.
///
/// The postgres image's entrypoint runs a temporary server during initdb and
/// restarts the real one, so a container can be "started" before it accepts
/// connections. Probing with a plain client first keeps the retry out of
/// `PostgresPersistenceStore` itself (pool-timeout tests depend on its behaviour).
///
/// Panics with a clear message if the DB is not ready by the deadline.
fn connect_with_retry(conn_str: &str, tag: &str) -> Arc<PostgresPersistenceStore> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        match Client::connect(conn_str, NoTls) {
            Ok(_) => break,
            Err(e) => {
                if Instant::now() >= deadline {
                    panic!("postgres:{tag} did not accept connections within {READY_DEADLINE:?}: {e}");
                }
                thread::sleep(READY_BACKOFF);
            }
        }
    }

    Arc::new(PostgresPersistenceStore::new(conn_str).unwrap_or_else(|e| {
        panic!(
            "PostgresPersistenceStore::new failed for postgres:{tag} — \
             migration or pool error: {e}"
        )
    }))
}

/// Read the host port mapped to 5432, waiting for Docker to publish it.
///
/// Under Docker Desktop, with many containers starting in parallel, the port map
/// can lag behind `start()` returning, so `PortNotExposed` is retried until the
/// deadline. Any other error fails immediately.
fn host_port_with_retry(node: &Container<Postgres>, tag: &str) -> u16 {
    let deadline = Instant::now() + PORT_DEADLINE;
    loop {
        match node.get_host_port_ipv4(5432) {
            Ok(port) => return port,
            Err(e @ TestcontainersError::PortNotExposed { .. }) => {
                if Instant::now() >= deadline {
                    panic!("postgres:{tag} did not publish port 5432 within {PORT_DEADLINE:?}: {e}");
                }
                thread::sleep(PORT_BACKOFF);
            }
            Err(e) => panic!("testcontainers: get_host_port_ipv4 for postgres:{tag} — {e}"),
        }
    }
}

/// Start a `postgres:<tag>` container and return it with its connection string.
fn start_pg(tag: &str) -> (Container<Postgres>, String) {
    let node = Postgres::default()
        .with_tag(tag)
        .start()
        .unwrap_or_else(|e| panic!("testcontainers: failed to start postgres:{tag} — {e}"));

    let host = node
        .get_host()
        .unwrap_or_else(|e| panic!("testcontainers: get_host for postgres:{tag} — {e}"));
    let port = host_port_with_retry(&node, tag);

    let conn_str = format!("postgresql://postgres:postgres@{host}:{port}/postgres");
    (node, conn_str)
}

/// Start a `postgres:<tag>` container, construct an `Arc<PostgresPersistenceStore>`,
/// and invoke `body`. The container lives for the duration of `body`.
///
/// Panics with a clear message if Docker is unavailable or the image is missing.
#[allow(dead_code)] // shared helper — not every test binary calls this one (Rust dead-code analysis is per-binary)
pub fn with_pg<F>(tag: &str, body: F)
where
    F: FnOnce(Arc<PostgresPersistenceStore>),
{
    let (_node, conn_str) = start_pg(tag);
    let store = connect_with_retry(&conn_str, tag);

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
    let (_node, conn_str) = start_pg(tag);
    let store = connect_with_retry(&conn_str, tag);

    body(store, conn_str);
    // `node` drops here, stopping the container.
}
