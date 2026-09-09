//! Regression test for F-16: `apply_migrations` TOCTOU race on the FIRST concurrent
//! `open_for_agent` of a brand-new per-agent file.
//!
//! Root cause (pre-fix): `apply_migrations` was check-then-act — read `user_version`,
//! then run whichever `apply_vN` steps were needed, with no lock held across the two. Two
//! threads racing the very first open of a brand-new file could both read `user_version = 0`,
//! both decide v3's `ALTER TABLE claims ADD COLUMN valid_time_start_granularity` was needed,
//! and the second racer would fail with `duplicate column name`.
//!
//! Fix: `apply_migrations` now wraps the version check, all pending DDL, and the
//! `user_version` write in a single `BEGIN IMMEDIATE` transaction, so the second racer
//! blocks at `BEGIN IMMEDIATE` (bounded by the `busy_timeout` set in
//! `connection.rs::apply_pragmas`) until the first commits, then observes the version
//! already current and performs a correct no-op.

use std::sync::{Arc, Barrier};
use std::thread;

use mempill_sqlite::connection::open_for_agent;
use mempill_sqlite::migrations::CURRENT_SCHEMA_VERSION;

const THREADS: usize = 6;
const ITERATIONS: usize = 15;

/// Race `THREADS` OS threads through the FIRST `open_for_agent` of a brand-new per-agent
/// file. Every racer must succeed; the resulting schema must be at
/// `CURRENT_SCHEMA_VERSION` with no duplicate-column error and no duplicate columns.
///
/// Run several iterations (fresh temp dir each time) to make the race window likely to be
/// hit — a single iteration can pass by luck even with the pre-fix race present.
#[test]
fn concurrent_first_open_for_agent_never_duplicates_migration_ddl() {
    for iteration in 0..ITERATIONS {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let base_dir = dir.path().to_path_buf();

        // Barrier releases all threads at (as close as possible to) the same instant, to
        // maximize the odds of hitting the pre-fix TOCTOU window.
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let base_dir = base_dir.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || -> Result<(), String> {
                    barrier.wait();
                    open_for_agent(&base_dir, "race-agent")
                        .map(|_conn| ())
                        .map_err(|e| format!("{e}"))
                })
            })
            .collect();

        let mut errors = Vec::new();
        for h in handles {
            match h.join().expect("racer thread must not panic") {
                Ok(()) => {}
                Err(e) => errors.push(e),
            }
        }
        assert!(
            errors.is_empty(),
            "iteration {iteration}: all {THREADS} concurrent first-open racers must succeed, \
             got errors: {errors:?}"
        );

        // Verify the resulting schema: final version reached, no duplicate columns.
        let path = base_dir.join("agent_race-agent.db");
        let conn = rusqlite::Connection::open(&path)
            .expect("re-opening the migrated file for verification must succeed");

        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version must be readable");
        assert_eq!(
            version, CURRENT_SCHEMA_VERSION,
            "iteration {iteration}: schema must land at CURRENT_SCHEMA_VERSION"
        );

        let mut stmt = conn
            .prepare("PRAGMA table_info(claims)")
            .expect("table_info(claims) must prepare");
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table_info(claims) must query")
            .map(|r| r.expect("column name row must decode"))
            .collect();

        for expected in ["valid_time_start_granularity", "valid_time_end_granularity"] {
            let occurrences = cols.iter().filter(|c| c.as_str() == expected).count();
            assert_eq!(
                occurrences, 1,
                "iteration {iteration}: column {expected} must appear exactly once, found {occurrences} \
                 (>1 would mean the v3 ALTER TABLE double-applied)"
            );
        }
    }
}

/// A second `open_for_agent` of an already-fully-migrated file is a correct no-op: it
/// succeeds, does not error, and leaves the schema version unchanged.
#[test]
fn second_open_of_already_migrated_file_is_a_noop() {
    let dir = tempfile::tempdir().expect("tempdir should create");

    let conn1 = open_for_agent(dir.path(), "agent-solo")
        .expect("first open_for_agent must succeed and run all migrations");
    let version_after_first: u32 = conn1
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version must be readable");
    assert_eq!(version_after_first, CURRENT_SCHEMA_VERSION);
    drop(conn1);

    // Second open of the same, now-fully-migrated file must succeed as a no-op.
    let conn2 = open_for_agent(dir.path(), "agent-solo")
        .expect("second open_for_agent of an already-migrated file must succeed");
    let version_after_second: u32 = conn2
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version must be readable");
    assert_eq!(
        version_after_second, CURRENT_SCHEMA_VERSION,
        "re-opening an already-migrated file must not change the schema version"
    );

    let mut stmt = conn2
        .prepare("PRAGMA table_info(claims)")
        .expect("table_info(claims) must prepare");
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("table_info(claims) must query")
        .map(|r| r.expect("column name row must decode"))
        .collect();
    for expected in ["valid_time_start_granularity", "valid_time_end_granularity"] {
        assert_eq!(
            cols.iter().filter(|c| c.as_str() == expected).count(),
            1,
            "column {expected} must appear exactly once after the no-op re-open"
        );
    }
}
