//! Regression test: `query_memory` with `as_of_tx_time = Some(T)` on Postgres.
//!
//! # Bug being guarded
//!
//! Prior to this fix, `load_subject_line` bound the `as_of_tx_time` cutoff as a
//! `chrono::DateTime<Utc>` parameter against the `tx_time TEXT` column. The `postgres` crate
//! serializes `DateTime<Utc>` as TIMESTAMPTZ — a type mismatch against the TEXT column —
//! producing the runtime error:
//!
//!   error serializing parameter N: cannot convert between the Rust type `chrono::DateTime<Utc>`
//!   and the Postgres type `text`
//!
//! The fix: bind `.to_rfc3339()` (a `&str`) so the comparison is string-vs-string, identical
//! to the stored format written on INSERT.
//!
//! # What this test verifies
//!
//! 1. Two claims are ingested: one in the past (valid 2020), one in the present (valid 2025).
//! 2. `query_memory` with `as_of_tx_time = Some(T)` returns a belief WITHOUT a serialization
//!    error — confirming the TEXT binding fix works end-to-end.
//! 3. `query_memory` with BOTH `as_of_tx_time = Some(T)` AND `valid_at = Some(V)` also works.
//! 4. SQLite is unaffected (not exercised here — rusqlite serializes DateTime to a comparable
//!    RFC-3339 string natively).
//!
//! # Postgres versions
//!
//! PG16 and PG18 — mirrors the version matrix used by all other integration suites.

mod common;

use std::sync::Arc;

use chrono::Utc;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_core::noop::{NoOpOracle, NoOpVector};
use mempill_core::EngineConfig;
use mempill_postgres::open_postgres;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel,
    ValidTime,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn dt(rfc3339: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("test fixture datetime must parse")
        .with_timezone(&Utc)
}

fn vt(start: &str, end: Option<&str>) -> ValidTime {
    ValidTime {
        start: Some(dt(start)),
        end: end.map(dt),
        valid_time_confidence: 0.9,
        start_granularity: None,
        end_granularity: None,
    }
}

fn high_confidence() -> Confidence {
    Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 }
}

// ── Core scenario ─────────────────────────────────────────────────────────────

/// Run the as-of query regression scenario on a real Postgres connection.
///
/// Schema:
///   alice CEO: valid [2020-01-01, 2023-01-01)   — "old" claim
///   bob   CEO: valid [2023-01-01, ∞)             — "current" claim
///
/// Assertions:
///   A. `query_memory` with `as_of_tx_time = Some(now)` and `valid_at = None`
///      returns a Resolved belief (Bob) — no serialization error on the as-of path.
///   B. `query_memory` with `as_of_tx_time = Some(now)` and `valid_at = Some(2021-06-01)`
///      returns a Resolved belief (Alice) — both temporal axes set simultaneously.
///   C. `query_memory` with `as_of_tx_time = None` and `valid_at = Some(now)` also works.
fn run_as_of_query_scenario(conn_str: &str) {
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
            let agent = AgentId("pg-as-of-regression-agent".into());

            // ── Ingest: alice (old) ─────────────────────────────────────────────
            engine
                .ingest_claim(IngestClaimRequest {
                    agent_id: agent.clone(),
                    subject: "acme-corp".into(),
                    predicate: "ceo".into(),
                    value: serde_json::json!("alice"),
                    provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                    cardinality: Cardinality::Functional,
                    valid_time: Some(vt("2020-01-01T00:00:00Z", Some("2023-01-01T00:00:00Z"))),
                    confidence: high_confidence(),
                    criticality: Criticality::Medium,
                    derived_from: vec![],
                })
                .await
                .map_err(|e| format!("alice ingest failed: {e}"))?;

            // ── Ingest: bob (current) ────────────────────────────────────────────
            engine
                .ingest_claim(IngestClaimRequest {
                    agent_id: agent.clone(),
                    subject: "acme-corp".into(),
                    predicate: "ceo".into(),
                    value: serde_json::json!("bob"),
                    provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
                    cardinality: Cardinality::Functional,
                    valid_time: Some(vt("2023-01-01T00:00:00Z", None)),
                    confidence: high_confidence(),
                    criticality: Criticality::Medium,
                    derived_from: vec![],
                })
                .await
                .map_err(|e| format!("bob ingest failed: {e}"))?;

            let now = Utc::now();

            // ── Assertion A: as_of_tx_time = Some(now), valid_at = None → Bob ───
            //
            // This is the previously-broken path: load_subject_line bound DateTime<Utc>
            // against the TEXT tx_time column → serialization error. After the fix it
            // must return a Resolved belief without error.
            let qr_a = engine
                .query_memory(QueryMemoryRequest {
                    agent_id: agent.clone(),
                    subject: "acme-corp".into(),
                    predicate: "ceo".into(),
                    as_of_tx_time: Some(now),
                    valid_at: None,
                })
                .await
                .map_err(|e| format!("Assertion A — as_of_tx_time query failed: {e}"))?;

            if qr_a.belief.status != BeliefStatus::Resolved {
                return Err(format!(
                    "Assertion A: expected Resolved; got {:?}",
                    qr_a.belief.status
                ));
            }
            let primary_a = qr_a
                .belief
                .primary
                .ok_or_else(|| "Assertion A: primary must exist".to_string())?;
            if primary_a.fact.value != serde_json::json!("bob") {
                return Err(format!(
                    "Assertion A: expected Bob as primary; got {:?}",
                    primary_a.fact.value
                ));
            }

            // ── Assertion B: as_of_tx_time = Some(now) + valid_at = Some(2021-06-01) → Alice ──
            //
            // Both temporal axes set simultaneously. valid_at=2021 falls in Alice's window.
            let qr_b = engine
                .query_memory(QueryMemoryRequest {
                    agent_id: agent.clone(),
                    subject: "acme-corp".into(),
                    predicate: "ceo".into(),
                    as_of_tx_time: Some(now),
                    valid_at: Some(dt("2021-06-01T00:00:00Z")),
                })
                .await
                .map_err(|e| format!("Assertion B — as_of + valid_at query failed: {e}"))?;

            if qr_b.belief.status != BeliefStatus::Resolved {
                return Err(format!(
                    "Assertion B: expected Resolved; got {:?}",
                    qr_b.belief.status
                ));
            }
            let primary_b = qr_b
                .belief
                .primary
                .ok_or_else(|| "Assertion B: primary must exist".to_string())?;
            if primary_b.fact.value != serde_json::json!("alice") {
                return Err(format!(
                    "Assertion B: valid_at=2021 is in Alice's window → expected Alice; got {:?}",
                    primary_b.fact.value
                ));
            }

            // ── Assertion C: as_of_tx_time = None + valid_at = Some(now) → Bob ──
            //
            // Confirm the non-as-of path combined with valid_at still works.
            let qr_c = engine
                .query_memory(QueryMemoryRequest {
                    agent_id: agent.clone(),
                    subject: "acme-corp".into(),
                    predicate: "ceo".into(),
                    as_of_tx_time: None,
                    valid_at: Some(now),
                })
                .await
                .map_err(|e| format!("Assertion C — valid_at-only query failed: {e}"))?;

            if qr_c.belief.status != BeliefStatus::Resolved {
                return Err(format!(
                    "Assertion C: expected Resolved; got {:?}",
                    qr_c.belief.status
                ));
            }
            let primary_c = qr_c
                .belief
                .primary
                .ok_or_else(|| "Assertion C: primary must exist".to_string())?;
            if primary_c.fact.value != serde_json::json!("bob") {
                return Err(format!(
                    "Assertion C: valid_at=now is in Bob's window → expected Bob; got {:?}",
                    primary_c.fact.value
                ));
            }

            Ok(())
        });

        // Drop engine OUTSIDE block_on to avoid postgres Client::drop inside tokio executor.
        drop(engine);
        result
    });

    join.join()
        .expect("as-of query regression thread must not panic")
        .expect("all three as-of query assertions must pass");
}

// ── PG16 ──────────────────────────────────────────────────────────────────────

/// Regression: as-of query path against PG16 must not produce a serialization error.
#[test]
fn pg16_as_of_query_no_serialization_error() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_as_of_query_scenario(&conn_str);
    });
}

// ── PG18 ──────────────────────────────────────────────────────────────────────

/// Regression: as-of query path against PG18 must not produce a serialization error.
#[test]
fn pg18_as_of_query_no_serialization_error() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_as_of_query_scenario(&conn_str);
    });
}
