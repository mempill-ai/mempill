//! # as-of / bi-temporal correctness benchmark
//!
//! A reproducible, assertion-based benchmark of mempill's as-of query correctness.
//!
//! This is a CORRECTNESS benchmark, not a performance benchmark: it does not measure
//! latency or throughput, and publishes no timing numbers. Every scenario asserts an
//! exact, documented expected outcome using the real SQLite adapter end-to-end (no
//! mocks) against the public `mempill` crate API — the same surface a third-party
//! integrator uses.
//!
//! ## What is measured
//!
//! Two independent time axes, queried in isolation and in combination:
//!
//! - **Valid-time axis** (`valid_at`) — "what was true in the world at instant T?"
//! - **Transaction-time axis** (`as_of_tx_time`) — "what did the system believe as of
//!   the moment it learned things up to instant T?"
//!
//! Scenario groups:
//!
//! 1. Valid-time as-of correctness — point-in-time recall across a claim's history.
//! 2. Transaction-time as-of correctness — independent axis, held constant across
//!    valid-time.
//! 3. Succession — a 3+ window non-overlapping valid-time chain, confirming correct
//!    claim selection at multiple query instants (exercises the reconciler, TASK-11).
//! 4. Contested (genuine conflict) — included deliberately so this benchmark does not
//!    only show the case where mempill resolves cleanly. A benchmark that only
//!    demonstrated clean resolution, with no honest look at the case mempill
//!    surfaces as contested-and-unresolved, would itself be a form of the fabricated
//!    precision mempill's Contested-first design explicitly exists to reject.
//!
//! Every scenario is a `#[test]`-style function with hard assertions (`assert_eq!` /
//! `assert!`), not a hand-inspected print statement — a future reconciler change that
//! breaks a scenario fails this binary with a non-zero exit code, no manual
//! re-authoring required.
//!
//! ## How to run
//!
//! ```sh
//! cargo run --release --example asof_correctness_benchmark -p mempill
//! ```
//!
//! No external data, network access, or proprietary fixtures are required — every
//! claim ingested by this benchmark is constructed inline with fixed, deterministic
//! timestamps. A third party can clone the public mempill repository and run the
//! command above to reproduce identical PASS/FAIL results.
//!
//! ## Known, documented current behavior
//!
//! A point-in-time claim whose valid-time window has `start == end` resolves to
//! `NoBelief` when queried exactly at that instant, because mempill's valid-time
//! windows are half-open `[start, end)`: no instant satisfies `instant < end` when
//! `end == start`. This is intentional, existing behavior (see
//! `mempill-sqlite/tests/temporal_succession_task11.rs::succession_point_claim`), not
//! a defect — this benchmark asserts that actual behavior rather than an idealized one.

use mempill::engine::{IngestClaimRequest, QueryMemoryRequest};
use mempill::types::{
    BeliefStatus, Cardinality, Confidence, Criticality, Disposition, ExternalKind,
    ProvenanceLabel, ValidTime,
};
use mempill::{AgentId, open_default_in_memory};
use mempill_core::application::ingest_claim::IngestClaimUseCase;
use mempill_core::application::query_memory::QueryMemoryUseCase;
use mempill_core::config::EngineConfig;
use mempill_core::noop::{NoOpOracle, NoOpVector};
use mempill_sqlite::{connection, store::SqlitePersistenceStore};
use std::sync::Arc;

/// Tally of scenario outcomes for the closing summary report.
#[derive(Default)]
struct Report {
    passed: Vec<&'static str>,
    failed: Vec<(&'static str, String)>,
}

impl Report {
    fn record(&mut self, name: &'static str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                println!("[PASS] {name}");
                self.passed.push(name);
            }
            Err(msg) => {
                println!("[FAIL] {name}: {msg}");
                self.failed.push((name, msg));
            }
        }
    }
}

// ── Shared fixtures ─────────────────────────────────────────────────────────────

fn dt(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap_or_else(|e| panic!("fixture timestamp {rfc3339:?} must parse: {e}"))
        .with_timezone(&chrono::Utc)
}

/// A trusted, high-confidence valid-time window (above the 0.7 succession threshold).
fn vt(start: &str, end: Option<&str>) -> ValidTime {
    ValidTime {
        start: Some(dt(start)),
        end: end.map(dt),
        valid_time_confidence: 0.9,
        start_granularity: None,
        end_granularity: None,
    }
}

fn confident() -> Confidence {
    Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 }
}

fn ingest_req(
    agent: &AgentId,
    subject: &str,
    predicate: &str,
    value: &str,
    valid_time: Option<ValidTime>,
) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent.clone(),
        subject: subject.into(),
        predicate: predicate.into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time,
        confidence: confident(),
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

macro_rules! check_eq {
    ($actual:expr, $expected:expr, $ctx:expr) => {
        if $actual != $expected {
            return Err(format!(
                "{}: expected {:?}, got {:?}",
                $ctx, $expected, $actual
            ));
        }
    };
}

// ── Group 1: valid-time as-of correctness ───────────────────────────────────────

/// Point-in-time recall across a claim's history: three sequential facts about the
/// same subject/predicate, each holding true over a distinct window. Querying with
/// `valid_at` set inside each window must return exactly that window's value.
async fn scenario_valid_time_point_in_time_recall() -> Result<(), String> {
    let engine = open_default_in_memory().map_err(|e| e.to_string())?;
    let agent = AgentId("bench-valid-time".into());

    engine
        .ingest_claim(ingest_req(
            &agent,
            "acme",
            "ceo",
            "alice",
            Some(vt("2018-01-01T00:00:00Z", Some("2021-01-01T00:00:00Z"))),
        ))
        .await
        .map_err(|e| e.to_string())?;
    engine
        .ingest_claim(ingest_req(
            &agent,
            "acme",
            "ceo",
            "bob",
            Some(vt("2021-01-01T00:00:00Z", Some("2023-06-01T00:00:00Z"))),
        ))
        .await
        .map_err(|e| e.to_string())?;
    engine
        .ingest_claim(ingest_req(
            &agent,
            "acme",
            "ceo",
            "carol",
            Some(vt("2023-06-01T00:00:00Z", None)),
        ))
        .await
        .map_err(|e| e.to_string())?;

    let cases: &[(&str, &str)] = &[
        ("2019-06-01T00:00:00Z", "alice"),
        ("2022-01-01T00:00:00Z", "bob"),
        ("2024-01-01T00:00:00Z", "carol"),
    ];

    for (instant, expected_value) in cases {
        let qr = engine
            .query_memory(QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "acme".into(),
                predicate: "ceo".into(),
                as_of_tx_time: None,
                valid_at: Some(dt(instant)),
            })
            .await
            .map_err(|e| e.to_string())?;

        check_eq!(qr.belief.status, BeliefStatus::Resolved, format!("valid_at={instant}"));
        let got = qr
            .belief
            .primary
            .as_ref()
            .map(|b| b.fact.value.clone())
            .ok_or_else(|| format!("valid_at={instant}: expected a primary belief"))?;
        check_eq!(got, serde_json::json!(expected_value), format!("valid_at={instant} value"));
    }
    Ok(())
}

/// A query instant that falls strictly in the gap between two non-overlapping
/// windows must return no belief, not a contested status.
async fn scenario_valid_time_gap_returns_no_belief() -> Result<(), String> {
    let engine = open_default_in_memory().map_err(|e| e.to_string())?;
    let agent = AgentId("bench-gap".into());

    engine
        .ingest_claim(ingest_req(
            &agent,
            "firm",
            "cfo",
            "alice",
            Some(vt("2020-01-01T00:00:00Z", Some("2021-01-01T00:00:00Z"))),
        ))
        .await
        .map_err(|e| e.to_string())?;
    engine
        .ingest_claim(ingest_req(
            &agent,
            "firm",
            "cfo",
            "bob",
            Some(vt("2022-01-01T00:00:00Z", None)),
        ))
        .await
        .map_err(|e| e.to_string())?;

    let qr = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "firm".into(),
            predicate: "cfo".into(),
            as_of_tx_time: None,
            valid_at: Some(dt("2021-06-01T00:00:00Z")), // strictly between the two windows
        })
        .await
        .map_err(|e| e.to_string())?;

    check_eq!(qr.belief.status, BeliefStatus::NoBelief, "query instant in the gap between windows");
    Ok(())
}

// ── Group 2: transaction-time as-of correctness ─────────────────────────────────

/// "What did we believe as of tx-time T?" — independent of valid-time. Two claims are
/// ingested at different, controlled transaction times; querying `as_of_tx_time` set
/// between the two ingests must see only the first claim, even though both claims'
/// valid-time windows would otherwise both be relevant at query time.
async fn scenario_tx_time_independent_of_valid_time() -> Result<(), String> {
    let conn = connection::open_in_memory().map_err(|e| e.to_string())?;
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = AgentId("bench-tx-time".into());
    let config = EngineConfig::default();

    let ingest_uc =
        IngestClaimUseCase::new(Arc::clone(&store), None::<Arc<NoOpOracle>>, None, config.clone());
    let query_uc = QueryMemoryUseCase::new(Arc::clone(&store), None::<Arc<NoOpVector>>, config);

    let t_alice_tx = dt("2020-06-01T00:00:00Z");
    let t_bob_tx = dt("2024-07-01T00:00:00Z");
    let as_of_between = dt("2022-06-01T00:00:00Z"); // after Alice's ingest, before Bob's

    ingest_uc
        .execute_with_time(
            ingest_req(
                &agent,
                "widget",
                "owner",
                "alice",
                Some(vt("2020-01-01T00:00:00Z", Some("2024-06-01T00:00:00Z"))),
            ),
            t_alice_tx,
        )
        .map_err(|e| e.to_string())?;
    ingest_uc
        .execute_with_time(
            ingest_req(&agent, "widget", "owner", "bob", Some(vt("2024-06-01T00:00:00Z", None))),
            t_bob_tx,
        )
        .map_err(|e| e.to_string())?;

    let query_now = t_bob_tx + chrono::Duration::days(30);

    // as_of_tx_time between the two ingests: Bob's claim is invisible (learned later).
    let qr_between = query_uc
        .execute_with_time(
            QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "widget".into(),
                predicate: "owner".into(),
                as_of_tx_time: Some(as_of_between),
                valid_at: None,
            },
            query_now,
        )
        .map_err(|e| e.to_string())?;
    check_eq!(qr_between.belief.status, BeliefStatus::Resolved, "as_of_tx_time between ingests");
    check_eq!(
        qr_between.belief.primary.as_ref().map(|b| b.fact.value.clone()),
        Some(serde_json::json!("alice")),
        "as_of_tx_time between ingests: primary"
    );

    // as_of_tx_time after both ingests: both claims are visible; current-belief fold applies.
    let qr_after = query_uc
        .execute_with_time(
            QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "widget".into(),
                predicate: "owner".into(),
                as_of_tx_time: Some(t_bob_tx + chrono::Duration::seconds(1)),
                valid_at: None,
            },
            query_now,
        )
        .map_err(|e| e.to_string())?;
    check_eq!(qr_after.belief.status, BeliefStatus::Resolved, "as_of_tx_time after both ingests");
    check_eq!(
        qr_after.belief.primary.as_ref().map(|b| b.fact.value.clone()),
        Some(serde_json::json!("bob")),
        "as_of_tx_time after both ingests: primary"
    );

    Ok(())
}

/// Combined case: `valid_at` and `as_of_tx_time` set independently at once (the full
/// D2-independence case) — confirms the two axes compose correctly rather than one
/// silently overriding the other.
async fn scenario_combined_valid_time_and_tx_time() -> Result<(), String> {
    let conn = connection::open_in_memory().map_err(|e| e.to_string())?;
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = AgentId("bench-combined".into());
    let config = EngineConfig::default();

    let ingest_uc =
        IngestClaimUseCase::new(Arc::clone(&store), None::<Arc<NoOpOracle>>, None, config.clone());
    let query_uc = QueryMemoryUseCase::new(Arc::clone(&store), None::<Arc<NoOpVector>>, config);

    let t_alice_tx = dt("2020-06-01T00:00:00Z");
    let t_bob_tx = dt("2021-06-01T00:00:00Z");

    ingest_uc
        .execute_with_time(
            ingest_req(
                &agent,
                "org",
                "lead",
                "alice",
                Some(vt("2019-01-01T00:00:00Z", Some("2021-01-01T00:00:00Z"))),
            ),
            t_alice_tx,
        )
        .map_err(|e| e.to_string())?;
    ingest_uc
        .execute_with_time(
            ingest_req(&agent, "org", "lead", "bob", Some(vt("2021-01-01T00:00:00Z", None))),
            t_bob_tx,
        )
        .map_err(|e| e.to_string())?;

    let query_now = t_bob_tx + chrono::Duration::days(365);

    // valid_at inside Alice's window, as_of_tx_time after both ingests (both claims visible)
    // -> the valid-time axis selects Alice regardless of tx-time visibility of Bob.
    let qr = query_uc
        .execute_with_time(
            QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "org".into(),
                predicate: "lead".into(),
                as_of_tx_time: Some(t_bob_tx + chrono::Duration::seconds(1)),
                valid_at: Some(dt("2019-06-01T00:00:00Z")),
            },
            query_now,
        )
        .map_err(|e| e.to_string())?;
    check_eq!(qr.belief.status, BeliefStatus::Resolved, "combined axes: valid_at inside Alice's window");
    check_eq!(
        qr.belief.primary.as_ref().map(|b| b.fact.value.clone()),
        Some(serde_json::json!("alice")),
        "combined axes: primary must be Alice (valid-time axis governs claim selection)"
    );

    Ok(())
}

// ── Group 3: succession (3+ window non-overlapping chain) ──────────────────────

/// A 3+ window non-overlapping succession chain. Confirms correct claim selection at
/// multiple distinct query instants, and that every non-terminal ingest in the chain
/// is classified `CommittedCheap` (succession), never `Contested`.
async fn scenario_succession_three_way_chain() -> Result<(), String> {
    let engine = open_default_in_memory().map_err(|e| e.to_string())?;
    let agent = AgentId("bench-succession".into());

    let windows: &[(&str, &str, Option<&str>)] = &[
        ("alpha", "2019-01-01T00:00:00Z", Some("2021-01-01T00:00:00Z")),
        ("beta", "2021-01-01T00:00:00Z", Some("2023-01-01T00:00:00Z")),
        ("gamma", "2023-01-01T00:00:00Z", None),
    ];

    for (value, start, end) in windows {
        let resp = engine
            .ingest_claim(ingest_req(&agent, "org", "cto", value, Some(vt(start, *end))))
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(
            resp.disposition,
            Disposition::CommittedCheap,
            format!("succession ingest of {value} must be CommittedCheap, never Contested")
        );
    }

    let cases: &[(&str, &str)] = &[
        ("2020-01-01T00:00:00Z", "alpha"),
        ("2022-01-01T00:00:00Z", "beta"),
        ("2024-01-01T00:00:00Z", "gamma"),
        // exact boundary instants: start-inclusive / prior-end-exclusive
        ("2021-01-01T00:00:00Z", "beta"),
        ("2023-01-01T00:00:00Z", "gamma"),
    ];

    for (instant, expected_value) in cases {
        let qr = engine
            .query_memory(QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "org".into(),
                predicate: "cto".into(),
                as_of_tx_time: None,
                valid_at: Some(dt(instant)),
            })
            .await
            .map_err(|e| e.to_string())?;
        check_eq!(qr.belief.status, BeliefStatus::Resolved, format!("3-way chain, valid_at={instant}"));
        check_eq!(
            qr.belief.primary.as_ref().map(|b| b.fact.value.clone()),
            Some(serde_json::json!(expected_value)),
            format!("3-way chain, valid_at={instant}: primary")
        );
    }

    Ok(())
}

// ── Group 4: Contested (genuine conflict) — the honest counter-case ────────────

/// Two trusted claims whose valid-time windows genuinely OVERLAP must surface as
/// Contested, not be silently resolved into a false succession. Included so this
/// benchmark does not only show clean resolution — a benchmark that never shows the
/// honest Contested outcome would misrepresent what mempill actually guarantees.
async fn scenario_genuine_conflict_is_contested() -> Result<(), String> {
    let engine = open_default_in_memory().map_err(|e| e.to_string())?;
    let agent = AgentId("bench-contested".into());

    let r_alice = engine
        .ingest_claim(ingest_req(
            &agent,
            "bank",
            "ceo",
            "alice",
            Some(vt("2020-01-01T00:00:00Z", Some("2025-01-01T00:00:00Z"))),
        ))
        .await
        .map_err(|e| e.to_string())?;
    check_eq!(r_alice.disposition, Disposition::CommittedCheap, "first claim on a line is always CommittedCheap");

    let r_bob = engine
        .ingest_claim(ingest_req(
            &agent,
            "bank",
            "ceo",
            "bob",
            // Overlaps Alice's window in [2024-01-01, 2025-01-01)
            Some(vt("2024-01-01T00:00:00Z", None)),
        ))
        .await
        .map_err(|e| e.to_string())?;
    check_eq!(
        r_bob.disposition,
        Disposition::Contested,
        "overlapping, trusted valid-time windows must be Contested, not silently resolved"
    );

    let qr = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "bank".into(),
            predicate: "ceo".into(),
            as_of_tx_time: None,
            valid_at: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    check_eq!(qr.belief.status, BeliefStatus::Contested, "query over overlapping windows must surface Contested");
    check_eq!(qr.belief.primary.is_none(), true, "Contested belief must not expose a false single primary");
    check_eq!(qr.belief.alternatives.len(), 2, "Contested belief must expose both candidates");

    Ok(())
}

/// A point-in-time claim (`start == end`) does not resolve at its own exact instant —
/// documented, existing half-open-interval behavior. Included to demonstrate this
/// benchmark reflects mempill's actual current behavior rather than an idealized one.
async fn scenario_point_claim_documented_no_belief() -> Result<(), String> {
    let engine = open_default_in_memory().map_err(|e| e.to_string())?;
    let agent = AgentId("bench-point".into());
    let instant = "2020-01-01T00:00:00Z";

    engine
        .ingest_claim(ingest_req(&agent, "event", "status", "alice-point", Some(vt(instant, Some(instant)))))
        .await
        .map_err(|e| e.to_string())?;
    engine
        .ingest_claim(ingest_req(&agent, "event", "status", "bob", Some(vt("2020-01-02T00:00:00Z", None))))
        .await
        .map_err(|e| e.to_string())?;

    let qr = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "event".into(),
            predicate: "status".into(),
            as_of_tx_time: None,
            valid_at: Some(dt(instant)),
        })
        .await
        .map_err(|e| e.to_string())?;
    check_eq!(
        qr.belief.status,
        BeliefStatus::NoBelief,
        "point-in-time claim (start==end) queried at its own instant is documented as NoBelief under half-open [start,end) semantics"
    );

    Ok(())
}

// ── Entry point ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    println!("mempill as-of / bi-temporal correctness benchmark");
    println!("===================================================\n");

    let mut report = Report::default();

    report.record(
        "valid_time::point_in_time_recall",
        scenario_valid_time_point_in_time_recall().await,
    );
    report.record("valid_time::gap_returns_no_belief", scenario_valid_time_gap_returns_no_belief().await);
    report.record("tx_time::independent_of_valid_time", scenario_tx_time_independent_of_valid_time().await);
    report.record("combined::valid_time_and_tx_time", scenario_combined_valid_time_and_tx_time().await);
    report.record("succession::three_way_chain", scenario_succession_three_way_chain().await);
    report.record("contested::genuine_conflict_not_silently_resolved", scenario_genuine_conflict_is_contested().await);
    report.record("point_claim::documented_no_belief_at_instant", scenario_point_claim_documented_no_belief().await);

    println!("\n===================================================");
    println!(
        "Result: {}/{} scenarios passed",
        report.passed.len(),
        report.passed.len() + report.failed.len()
    );
    if !report.failed.is_empty() {
        println!("\nFailures:");
        for (name, msg) in &report.failed {
            println!("  - {name}: {msg}");
        }
        std::process::exit(1);
    }
}
