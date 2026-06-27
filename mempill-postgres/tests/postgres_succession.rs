//! TASK-11 (Resolution #3): Postgres cross-adapter succession + TIMESTAMPTZ boundary precision.
//!
//! Verifies that:
//!   1. Temporal succession (non-overlapping trusted valid-time windows) works on real PG16+PG18.
//!   2. TIMESTAMPTZ boundary precision is preserved: Alice [Jan, Mar) and Bob [Mar, ∞) — a query
//!      at exactly the boundary instant (Mar 1 00:00:00.000000000Z) selects Bob (start inclusive,
//!      Alice's end exclusive). Postgres stores TIMESTAMPTZ at microsecond precision; we use
//!      second-boundary instants to avoid nanosecond rounding traps.
//!   3. Low-confidence valid_time → Contested (I2 fallback) on Postgres.
//!
//! Each test creates its own container for full isolation (PG16 + PG18 matrix).
//!
//! ## Runtime-safety note
//!
//! The `postgres` sync crate calls `block_on` in `Client::drop`. All tests follow the
//! pattern from `postgres_oracle_e2e.rs`:
//!   - Engine is created BEFORE `block_on`.
//!   - `block_on` closure borrows (not moves) the engine via an `Arc` clone.
//!   - `drop(engine)` is called AFTER `block_on` returns but BEFORE the `rt` drops.
//!   - All checks are communicated out as `Result<(), String>` to avoid engine capture
//!     surviving into the async block's drop sequence.

mod common;

use std::sync::Arc;

use chrono::Utc;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_core::noop::{NoOpOracle, NoOpVector};
use mempill_core::EngineConfig;
use mempill_postgres::open_postgres;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality,
    Disposition, ExternalKind, ProvenanceLabel, ValidTime,
};

// ── Test helpers ──────────────────────────────────────────────────────────────

fn dt(rfc3339: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .with_timezone(&Utc)
}

fn vt_high(start: &str, end: Option<&str>) -> ValidTime {
    ValidTime {
        start: Some(dt(start)),
        end: end.map(dt),
        valid_time_confidence: 0.9,
        granularity: None,
    }
}

fn vt_low(start: &str, end: Option<&str>) -> ValidTime {
    ValidTime {
        start: Some(dt(start)),
        end: end.map(dt),
        valid_time_confidence: 0.5, // below 0.7 threshold
        granularity: None,
    }
}

fn confident() -> Confidence {
    Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 }
}

// ── Core test scenario ────────────────────────────────────────────────────────

/// Run the full succession + boundary scenario on a real Postgres connection.
///
/// Schema:
///   Alice CEO: valid [2020-01-01, 2024-03-01)  confidence=0.9
///   Bob   CEO: valid [2024-03-01, ∞)           confidence=0.9
///
/// Assertions:
///   1. Alice ingest → CommittedCheap.
///   2. Bob ingest → CommittedCheap (succession, NOT Contested).
///   3. Query NOW → Resolved(Bob).
///   4. Query as_of 2022-06-01 (in Alice's window) → Resolved(Alice).
///   5. TIMESTAMPTZ boundary: query at exactly 2024-03-01 → Resolved(Bob).
fn run_succession_scenario(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        let engine = open_postgres(
            &conn_str,
            None::<Arc<NoOpOracle>>,
            None::<Arc<NoOpVector>>,
            EngineConfig::default(),
        )
        .expect("open_postgres must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        // engine is borrowed (not moved) into block_on via a reference captured by the async
        // block. The engine lives in the thread (not the async scope) so it drops outside
        // block_on, avoiding the postgres Client::drop-inside-tokio-runtime panic.
        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-succession-agent".into());

            // ── 1. Alice ingest ─────────────────────────────────────────────────
            let r_alice = engine.ingest_claim(IngestClaimRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "ceo".into(),
                value: serde_json::json!("alice"),
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                cardinality: Cardinality::Functional,
                valid_time: Some(vt_high("2020-01-01T00:00:00Z", Some("2024-03-01T00:00:00Z"))),
                confidence: confident(),
                criticality: Criticality::Medium,
                derived_from: vec![],
            }).await.map_err(|e| format!("alice ingest failed: {e}"))?;

            println!("[PG SUCC] alice ingest → {:?}", r_alice.disposition);
            if r_alice.disposition != Disposition::CommittedCheap {
                return Err(format!(
                    "alice MUST be CommittedCheap; got {:?}", r_alice.disposition
                ));
            }

            // ── 2. Bob ingest → Succession (NOT Contested) ─────────────────────
            let r_bob = engine.ingest_claim(IngestClaimRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "ceo".into(),
                value: serde_json::json!("bob"),
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                cardinality: Cardinality::Functional,
                valid_time: Some(vt_high("2024-03-01T00:00:00Z", None)),
                confidence: confident(),
                criticality: Criticality::Medium,
                derived_from: vec![],
            }).await.map_err(|e| format!("bob ingest failed: {e}"))?;

            println!("[PG SUCC] bob ingest → {:?}", r_bob.disposition);
            if r_bob.disposition != Disposition::CommittedCheap {
                return Err(format!(
                    "TASK-11: trusted non-overlapping succession MUST be CommittedCheap, not Contested; got {:?}",
                    r_bob.disposition
                ));
            }

            // ── 3. Query NOW → Bob ────────────────────────────────────────────
            let qr_now = engine.query_memory(QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "ceo".into(),
                as_of_tx_time: None,
        valid_at: None,
            }).await.map_err(|e| format!("query NOW failed: {e}"))?;

            println!("[PG SUCC] query NOW → status={:?}, primary={:?}",
                qr_now.belief.status,
                qr_now.belief.primary.as_ref().map(|b| &b.fact.value));

            if qr_now.belief.status != BeliefStatus::Resolved {
                return Err(format!(
                    "Postgres: NOW query MUST be Resolved (succession selects Bob); got {:?}",
                    qr_now.belief.status
                ));
            }
            let primary_now = qr_now.belief.primary
                .ok_or_else(|| "primary must exist at NOW".to_string())?;
            if primary_now.fact.value != serde_json::json!("bob") {
                return Err(format!(
                    "Postgres: primary at NOW MUST be Bob; got {:?}", primary_now.fact.value
                ));
            }
            if !qr_now.belief.alternatives.is_empty() {
                return Err(format!(
                    "Postgres: no alternatives expected; got {:?}", qr_now.belief.alternatives
                ));
            }

            // ── 4. Query as_of 2022-06-01 → Alice ────────────────────────────
            let qr_past = engine.query_memory(QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "ceo".into(),
                as_of_tx_time: Some(dt("2022-06-01T00:00:00Z")),
        valid_at: None,
            }).await.map_err(|e| format!("query as_of 2022 failed: {e}"))?;

            println!("[PG SUCC] query as_of 2022-06-01 → status={:?}, primary={:?}",
                qr_past.belief.status,
                qr_past.belief.primary.as_ref().map(|b| &b.fact.value));

            if qr_past.belief.status != BeliefStatus::Resolved {
                return Err(format!(
                    "Postgres: instant in Alice's window MUST be Resolved; got {:?}",
                    qr_past.belief.status
                ));
            }
            let primary_past = qr_past.belief.primary
                .ok_or_else(|| "primary at 2022 must exist".to_string())?;
            if primary_past.fact.value != serde_json::json!("alice") {
                return Err(format!(
                    "Postgres: instant in Alice's window → primary MUST be Alice; got {:?}",
                    primary_past.fact.value
                ));
            }

            // ── 5. TIMESTAMPTZ boundary: query at exactly 2024-03-01 → Bob ────
            //
            // instant=2024-03-01 ≥ alice_end=2024-03-01 → Alice NOT selected (end exclusive).
            // instant=2024-03-01 ≥ bob_start=2024-03-01 AND end=None → Bob selected (start inclusive).
            // This tests TIMESTAMPTZ round-trip through Postgres at second-boundary precision.
            let boundary = dt("2024-03-01T00:00:00Z");
            let qr_boundary = engine.query_memory(QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "ceo".into(),
                as_of_tx_time: Some(boundary),
        valid_at: None,
            }).await.map_err(|e| format!("query at boundary failed: {e}"))?;

            println!("[PG SUCC] query at boundary 2024-03-01 → status={:?}, primary={:?}",
                qr_boundary.belief.status,
                qr_boundary.belief.primary.as_ref().map(|b| &b.fact.value));

            if qr_boundary.belief.status != BeliefStatus::Resolved {
                return Err(format!(
                    "Postgres TIMESTAMPTZ boundary: 2024-03-01 MUST be Resolved; got {:?}",
                    qr_boundary.belief.status
                ));
            }
            let primary_boundary = qr_boundary.belief.primary
                .ok_or_else(|| "primary at boundary must exist".to_string())?;
            if primary_boundary.fact.value != serde_json::json!("bob") {
                return Err(format!(
                    "Postgres TIMESTAMPTZ boundary: 2024-03-01 == Bob's start → MUST select Bob; got {:?}",
                    primary_boundary.fact.value
                ));
            }

            Ok(())
        });

        // Drop engine OUTSIDE block_on to avoid postgres Client::drop inside tokio executor.
        drop(engine);
        result
    });

    join.join().expect("succession scenario thread must not panic")
        .expect("PG succession + boundary scenario must pass");
}

/// Run the I2 fallback scenario: low-confidence valid_time → Contested on Postgres.
fn run_low_confidence_contested(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        let engine = open_postgres(
            &conn_str,
            None::<Arc<NoOpOracle>>,
            None::<Arc<NoOpVector>>,
            EngineConfig::default(),
        )
        .expect("open_postgres must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-lowconf-agent".into());

            engine.ingest_claim(IngestClaimRequest {
                agent_id: agent.clone(),
                subject: "corp".into(),
                predicate: "ceo".into(),
                value: serde_json::json!("alice"),
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                cardinality: Cardinality::Functional,
                valid_time: Some(vt_low("2020-01-01T00:00:00Z", Some("2024-03-01T00:00:00Z"))),
                confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.5 },
                criticality: Criticality::Medium,
                derived_from: vec![],
            }).await.map_err(|e| format!("alice ingest failed: {e}"))?;

            let r_bob = engine.ingest_claim(IngestClaimRequest {
                agent_id: agent.clone(),
                subject: "corp".into(),
                predicate: "ceo".into(),
                value: serde_json::json!("bob"),
                provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                cardinality: Cardinality::Functional,
                valid_time: Some(vt_low("2024-03-01T00:00:00Z", None)),
                confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.5 },
                criticality: Criticality::Medium,
                derived_from: vec![],
            }).await.map_err(|e| format!("bob ingest failed: {e}"))?;

            println!("[PG LOWCONF] bob ingest → {:?}", r_bob.disposition);
            if r_bob.disposition != Disposition::Contested {
                return Err(format!(
                    "Postgres I2 fallback: confidence 0.5 < 0.7 MUST be Contested; got {:?}",
                    r_bob.disposition
                ));
            }

            Ok(())
        });

        drop(engine);
        result
    });

    join.join().expect("low-confidence thread must not panic")
        .expect("PG low-confidence contested scenario must pass");
}

// ── PG16 tests ────────────────────────────────────────────────────────────────

#[test]
fn pg16_succession_and_boundary() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_succession_scenario(&conn_str);
    });
}

#[test]
fn pg16_low_confidence_is_contested() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_low_confidence_contested(&conn_str);
    });
}

// ── PG18 tests ────────────────────────────────────────────────────────────────

#[test]
fn pg18_succession_and_boundary() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_succession_scenario(&conn_str);
    });
}

#[test]
fn pg18_low_confidence_is_contested() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_low_confidence_contested(&conn_str);
    });
}
