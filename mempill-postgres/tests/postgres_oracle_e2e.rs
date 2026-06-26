//! End-to-end oracle resolution tests for the W7b/W7-FIX `open_postgres_with_oracle` constructor.
//!
//! These tests run against real Postgres containers (PG16 + PG18) via testcontainers and
//! assert on `query_memory` surfaced beliefs after each oracle verdict.
//!
//! # Lifecycle verified (TASK-9 W7-FIX — full resolution loop on Postgres)
//!
//! 1. Construct engine via `open_postgres_with_oracle(conn, oracle, None, config)`.
//! 2. Ingest incumbent → CommittedCheap.
//! 3. Ingest challenger → QueuedForAdjudication (oracle present + Functional conflict).
//! 4. `query_memory` → Contested[both] (alice + challenger visible).
//! 5. `submit_adjudication(Affirm)` → challenger becomes CommittedCheap; query_memory surfaces challenger.
//! 6. `submit_adjudication(Deny)` → challenger becomes Superseded; query_memory surfaces incumbent.
//! 7. `submit_adjudication(Unknown)` → both Contested; query_memory shows Contested[both].
//! 8. `sweep_expired_adjudications` → expired row reverted to Contested; query_memory shows Contested[both].
//!
//! # Async-runtime safety (W7-FIX)
//!
//! The postgres sync crate (`postgres 0.19`) calls `block_on` in `Client::drop`. After the
//! W7-FIX, ALL pending-store reads in `submit_adjudication` and `sweep_expired_adjudications`
//! are performed inside `spawn_blocking`, so no `postgres::Client` is ever created or dropped
//! on the tokio executor thread. The full submit cycle is now safe on Postgres.
//!
//! The thread-isolation pattern (fresh OS thread containing its own tokio runtime) is still
//! used so that engine + connection drops happen outside any tokio context — belt-and-suspenders.

mod common;

use std::sync::Arc;
use std::time::Duration;

use mempill_core::ports::OraclePort;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_core::EngineConfig;
use mempill_postgres::open_postgres_with_oracle;
use mempill_types::{
    AgentId, AdjudicationResponse, AdjudicationVerdict, BeliefStatus, Cardinality, Confidence,
    Criticality, Disposition, ExternalKind, ProvenanceLabel,
};

// ── TestOracle — deterministic, same pattern as sqlite tests ─────────────────

struct TestOracle {
    fixed_uuid: uuid::Uuid,
}

impl OraclePort for TestOracle {
    type Error = mempill_core::noop::NoOpError;
    type Handle = uuid::Uuid;

    fn request_adjudication(
        &self,
        _agent_id: &AgentId,
        _request: mempill_types::AdjudicationRequest,
    ) -> Result<Self::Handle, Self::Error> {
        Ok(self.fixed_uuid)
    }

    fn handle_to_uuid(handle: &Self::Handle) -> uuid::Uuid {
        *handle
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn ingest_req(agent: &AgentId, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "org".into(),
        predicate: "ceo".into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::High,
        derived_from: vec![],
    }
}

fn query_req(agent: &AgentId) -> QueryMemoryRequest {
    QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "org".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None,
    }
}

fn adj_response(handle_id: uuid::Uuid, verdict: AdjudicationVerdict) -> AdjudicationResponse {
    AdjudicationResponse {
        handle_id,
        verdict,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    }
}

// ── Core scenario: constructor wires oracle; conflict → Contested[both] ───────

/// Run the live-Postgres oracle e2e scenario (pre-submit assertion: Contested[both]).
///
/// Used as a sanity check that the constructor wires oracle + pending store correctly.
fn run_pg_oracle_contested_only(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        let handle_id = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

        let engine = open_postgres_with_oracle(
            &conn_str,
            oracle,
            None::<Arc<mempill_core::NoOpVector>>,
            EngineConfig::default(),
        )
        .expect("open_postgres_with_oracle must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-oracle-e2e-agent".into());

            let resp_alice = engine.ingest_claim(ingest_req(&agent, "alice")).await
                .map_err(|e| format!("ingest alice failed: {e}"))?;
            if resp_alice.disposition != Disposition::CommittedCheap {
                return Err(format!(
                    "alice must be CommittedCheap; got {:?}", resp_alice.disposition
                ));
            }

            let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
                .map_err(|e| format!("ingest bob failed: {e}"))?;
            if resp_bob.disposition != Disposition::QueuedForAdjudication {
                return Err(format!(
                    "bob must be QueuedForAdjudication; got {:?}", resp_bob.disposition
                ));
            }

            let qr = engine.query_memory(query_req(&agent)).await
                .map_err(|e| format!("query_memory failed: {e}"))?;

            let status = qr.belief.status.clone();
            let primary = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
            let alts: Vec<_> = qr.belief.alternatives.iter()
                .map(|b| b.fact.value.clone()).collect();

            println!(
                "[PG-ORACLE E2E] status={:?} primary={:?} alternatives={:?}",
                status, primary, alts
            );

            if status != BeliefStatus::Contested {
                return Err(format!(
                    "query_memory MUST return Contested. Got {:?} (primary={:?}, alts={:?}).",
                    status, primary, alts
                ));
            }

            let all_vals: Vec<_> = primary.iter().chain(alts.iter()).cloned().collect();
            if !all_vals.contains(&serde_json::json!("alice")) {
                return Err(format!("Contested must include 'alice'; got {:?}", all_vals));
            }
            if !all_vals.contains(&serde_json::json!("bob")) {
                return Err(format!("Contested must include 'bob'; got {:?}", all_vals));
            }

            Ok(())
        });

        drop(engine);
        result
    });

    join.join().expect("test thread must not panic")
        .expect("PG oracle e2e contested scenario must pass");
}

// ── Full resolution: Affirm on Postgres ──────────────────────────────────────

/// Live-Postgres Affirm resolution:
/// - Ingest alice (incumbent, CommittedCheap) + bob (challenger, QueuedForAdjudication).
/// - submit_adjudication(Affirm) → bob becomes CommittedCheap, alice Superseded.
/// - query_memory must surface bob (Resolved / TimingUncertain), NOT alice.
fn run_pg_submit_affirm(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        let handle_id = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

        let engine = open_postgres_with_oracle(
            &conn_str,
            oracle,
            None::<Arc<mempill_core::NoOpVector>>,
            EngineConfig::default(),
        )
        .expect("open_postgres_with_oracle must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-affirm-agent".into());

            let resp_alice = engine.ingest_claim(ingest_req(&agent, "alice")).await
                .map_err(|e| format!("ingest alice failed: {e}"))?;
            assert_eq_str(resp_alice.disposition, Disposition::CommittedCheap, "alice disposition")?;

            let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
                .map_err(|e| format!("ingest bob failed: {e}"))?;
            assert_eq_str(resp_bob.disposition, Disposition::QueuedForAdjudication, "bob disposition")?;
            let bob_ref = resp_bob.claim_ref.clone();

            // BEFORE submit: Contested[alice, bob].
            let qr_before = engine.query_memory(query_req(&agent)).await
                .map_err(|e| format!("query before submit failed: {e}"))?;
            println!("[PG-AFFIRM] BEFORE: status={:?}", qr_before.belief.status);
            assert_eq_str(qr_before.belief.status, BeliefStatus::Contested, "before-submit status")?;

            // submit_adjudication(Affirm) — NOW runs safely on Postgres (W7-FIX).
            let outcome = engine.submit_adjudication(
                handle_id,
                adj_response(handle_id, AdjudicationVerdict::Affirm),
            ).await.map_err(|e| format!("submit Affirm failed: {e}"))?;

            assert_eq_str(outcome.disposition, Disposition::CommittedCheap, "Affirm outcome.disposition")?;
            if outcome.claim_ref != bob_ref {
                return Err(format!(
                    "Affirm outcome.claim_ref must be bob's ref; got {:?}", outcome.claim_ref
                ));
            }

            // AFTER Affirm: query_memory must surface bob (challenger).
            let qr_after = engine.query_memory(query_req(&agent)).await
                .map_err(|e| format!("query after Affirm failed: {e}"))?;
            println!(
                "[PG-AFFIRM] AFTER: status={:?} primary={:?}",
                qr_after.belief.status,
                qr_after.belief.primary.as_ref().map(|b| &b.fact.value)
            );

            if qr_after.belief.status == BeliefStatus::Contested {
                return Err(format!(
                    "AFTER Affirm: must NOT be Contested; got {:?}", qr_after.belief.status
                ));
            }
            if qr_after.belief.status == BeliefStatus::NoBelief {
                return Err(format!(
                    "AFTER Affirm: must NOT be NoBelief; got {:?}", qr_after.belief.status
                ));
            }
            let primary_val = qr_after.belief.primary.as_ref()
                .map(|b| b.fact.value.clone())
                .unwrap_or(serde_json::Value::Null);
            if primary_val != serde_json::json!("bob") {
                return Err(format!(
                    "AFTER Affirm: primary must be 'bob'; got {:?}", primary_val
                ));
            }

            Ok(())
        });

        drop(engine);
        result
    });

    join.join().expect("test thread must not panic")
        .expect("PG Affirm resolution must pass");
}

// ── Full resolution: Deny on Postgres ────────────────────────────────────────

/// Live-Postgres Deny resolution:
/// - Ingest alice (incumbent, CommittedCheap) + bob (challenger, QueuedForAdjudication).
/// - submit_adjudication(Deny) → bob Superseded, alice stays CommittedCheap.
/// - query_memory must surface alice (Resolved / TimingUncertain), NOT bob.
fn run_pg_submit_deny(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        let handle_id = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

        let engine = open_postgres_with_oracle(
            &conn_str,
            oracle,
            None::<Arc<mempill_core::NoOpVector>>,
            EngineConfig::default(),
        )
        .expect("open_postgres_with_oracle must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-deny-agent".into());

            let resp_alice = engine.ingest_claim(ingest_req(&agent, "alice")).await
                .map_err(|e| format!("ingest alice failed: {e}"))?;
            assert_eq_str(resp_alice.disposition, Disposition::CommittedCheap, "alice")?;

            let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
                .map_err(|e| format!("ingest bob failed: {e}"))?;
            assert_eq_str(resp_bob.disposition, Disposition::QueuedForAdjudication, "bob")?;

            // submit_adjudication(Deny).
            let outcome = engine.submit_adjudication(
                handle_id,
                adj_response(handle_id, AdjudicationVerdict::Deny),
            ).await.map_err(|e| format!("submit Deny failed: {e}"))?;

            assert_eq_str(outcome.disposition, Disposition::Superseded, "Deny outcome.disposition")?;

            // AFTER Deny: query_memory must surface alice (incumbent stands).
            let qr_after = engine.query_memory(query_req(&agent)).await
                .map_err(|e| format!("query after Deny failed: {e}"))?;
            println!(
                "[PG-DENY] AFTER: status={:?} primary={:?}",
                qr_after.belief.status,
                qr_after.belief.primary.as_ref().map(|b| &b.fact.value)
            );

            if qr_after.belief.status == BeliefStatus::Contested {
                return Err(format!(
                    "AFTER Deny: must NOT be Contested; got {:?}", qr_after.belief.status
                ));
            }
            let primary_val = qr_after.belief.primary.as_ref()
                .map(|b| b.fact.value.clone())
                .unwrap_or(serde_json::Value::Null);
            if primary_val != serde_json::json!("alice") {
                return Err(format!(
                    "AFTER Deny: primary must be 'alice' (incumbent); got {:?}", primary_val
                ));
            }

            Ok(())
        });

        drop(engine);
        result
    });

    join.join().expect("test thread must not panic")
        .expect("PG Deny resolution must pass");
}

// ── Full resolution: Unknown on Postgres ─────────────────────────────────────

/// Live-Postgres Unknown resolution:
/// - Ingest alice + bob.
/// - submit_adjudication(Unknown) → both Contested.
/// - query_memory must return Contested[alice, bob].
fn run_pg_submit_unknown(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        let handle_id = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

        let engine = open_postgres_with_oracle(
            &conn_str,
            oracle,
            None::<Arc<mempill_core::NoOpVector>>,
            EngineConfig::default(),
        )
        .expect("open_postgres_with_oracle must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-unknown-agent".into());

            engine.ingest_claim(ingest_req(&agent, "alice")).await
                .map_err(|e| format!("ingest alice failed: {e}"))?;
            engine.ingest_claim(ingest_req(&agent, "bob")).await
                .map_err(|e| format!("ingest bob failed: {e}"))?;

            // submit_adjudication(Unknown).
            let outcome = engine.submit_adjudication(
                handle_id,
                adj_response(handle_id, AdjudicationVerdict::Unknown),
            ).await.map_err(|e| format!("submit Unknown failed: {e}"))?;

            assert_eq_str(outcome.disposition, Disposition::Contested, "Unknown outcome.disposition")?;

            // AFTER Unknown: query_memory must surface Contested[both].
            let qr_after = engine.query_memory(query_req(&agent)).await
                .map_err(|e| format!("query after Unknown failed: {e}"))?;
            let status = qr_after.belief.status.clone();
            let primary = qr_after.belief.primary.as_ref().map(|b| b.fact.value.clone());
            let alts: Vec<_> = qr_after.belief.alternatives.iter()
                .map(|b| b.fact.value.clone()).collect();
            println!(
                "[PG-UNKNOWN] AFTER: status={:?} primary={:?} alts={:?}",
                status, primary, alts
            );

            if status != BeliefStatus::Contested {
                return Err(format!(
                    "AFTER Unknown: must be Contested[both]; got {:?}", status
                ));
            }
            let all_vals: Vec<_> = primary.iter().chain(alts.iter()).cloned().collect();
            if !all_vals.contains(&serde_json::json!("alice")) {
                return Err(format!("Unknown must keep alice visible; got {:?}", all_vals));
            }
            if !all_vals.contains(&serde_json::json!("bob")) {
                return Err(format!("Unknown must keep bob visible; got {:?}", all_vals));
            }

            Ok(())
        });

        drop(engine);
        result
    });

    join.join().expect("test thread must not panic")
        .expect("PG Unknown resolution must pass");
}

// ── Sweep: expired pending row on Postgres ────────────────────────────────────

/// Live-Postgres sweep (TTL expiry):
/// - Ingest alice + bob with an already-past TTL.
/// - sweep_expired_adjudications() → challenger reverted to Contested.
/// - query_memory must return Contested[both].
fn run_pg_sweep_expired(conn_str: &str) {
    let conn_str = conn_str.to_owned();

    let join = std::thread::spawn(move || {
        // Use a TTL of 0 seconds so the row expires immediately.
        let config = EngineConfig {
            default_adjudication_ttl: Some(Duration::from_secs(0)),
            ..EngineConfig::default()
        };

        let handle_id = uuid::Uuid::new_v4();
        let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

        let engine = open_postgres_with_oracle(
            &conn_str,
            oracle,
            None::<Arc<mempill_core::NoOpVector>>,
            config,
        )
        .expect("open_postgres_with_oracle must succeed");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        let result: Result<(), String> = rt.block_on(async {
            let agent = AgentId("pg-sweep-agent".into());

            engine.ingest_claim(ingest_req(&agent, "alice")).await
                .map_err(|e| format!("ingest alice failed: {e}"))?;

            let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
                .map_err(|e| format!("ingest bob failed: {e}"))?;
            // Bob should be QueuedForAdjudication with an immediately-expiring TTL.
            if resp_bob.disposition != Disposition::QueuedForAdjudication {
                return Err(format!(
                    "bob must be QueuedForAdjudication for sweep test; got {:?}",
                    resp_bob.disposition
                ));
            }

            // Give the expiry timestamp time to be strictly in the past (1 ms buffer).
            tokio::time::sleep(Duration::from_millis(10)).await;

            // sweep_expired_adjudications — NOW runs safely on Postgres (W7-FIX).
            let swept = engine.sweep_expired_adjudications().await
                .map_err(|e| format!("sweep failed: {e}"))?;

            println!("[PG-SWEEP] swept={swept}");
            if swept == 0 {
                return Err("sweep must revert at least 1 expired row".to_string());
            }

            // AFTER sweep: query_memory must surface Contested[both].
            let qr = engine.query_memory(query_req(&agent)).await
                .map_err(|e| format!("query after sweep failed: {e}"))?;
            let status = qr.belief.status.clone();
            let primary = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
            let alts: Vec<_> = qr.belief.alternatives.iter()
                .map(|b| b.fact.value.clone()).collect();
            println!(
                "[PG-SWEEP] AFTER: status={:?} primary={:?} alts={:?}",
                status, primary, alts
            );

            if status != BeliefStatus::Contested {
                return Err(format!(
                    "AFTER sweep: must be Contested[both]; got {:?}", status
                ));
            }
            let all_vals: Vec<_> = primary.iter().chain(alts.iter()).cloned().collect();
            if !all_vals.contains(&serde_json::json!("alice")) {
                return Err(format!("post-sweep must keep alice visible; got {:?}", all_vals));
            }
            if !all_vals.contains(&serde_json::json!("bob")) {
                return Err(format!("post-sweep must keep bob visible; got {:?}", all_vals));
            }

            Ok(())
        });

        drop(engine);
        result
    });

    join.join().expect("test thread must not panic")
        .expect("PG sweep expired must pass");
}

// ── Helper ────────────────────────────────────────────────────────────────────

fn assert_eq_str<T: std::fmt::Debug + PartialEq>(
    actual: T,
    expected: T,
    label: &str,
) -> Result<(), String> {
    if actual != expected {
        Err(format!("{label}: expected {:?}, got {:?}", expected, actual))
    } else {
        Ok(())
    }
}

// ── PG16 tests ────────────────────────────────────────────────────────────────

/// Live oracle e2e test (pre-submit Contested[both] assertion) against `postgres:16`.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_oracle_e2e_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_pg_oracle_contested_only(&conn_str);
    });
}

/// Live Affirm resolution test against `postgres:16`.
///
/// Proves `submit_adjudication(Affirm)` works on Postgres after W7-FIX:
/// challenger (bob) surfaces as the belief after Affirm.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_submit_affirm_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_pg_submit_affirm(&conn_str);
    });
}

/// Live Deny resolution test against `postgres:16`.
///
/// Proves `submit_adjudication(Deny)` works on Postgres after W7-FIX:
/// incumbent (alice) surfaces after Deny.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_submit_deny_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_pg_submit_deny(&conn_str);
    });
}

/// Live Unknown resolution test against `postgres:16`.
///
/// Proves `submit_adjudication(Unknown)` works on Postgres after W7-FIX:
/// Contested[alice, bob] after Unknown.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_submit_unknown_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_pg_submit_unknown(&conn_str);
    });
}

/// Live sweep (TTL expiry) test against `postgres:16`.
///
/// Proves `sweep_expired_adjudications` works on Postgres after W7-FIX:
/// expired pending row → Contested[alice, bob].
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_sweep_expired_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        run_pg_sweep_expired(&conn_str);
    });
}

// ── PG18 tests ────────────────────────────────────────────────────────────────

/// Live oracle e2e test against `postgres:18`.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_oracle_e2e_pg18() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_pg_oracle_contested_only(&conn_str);
    });
}

/// Live Affirm resolution test against `postgres:18`.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_submit_affirm_pg18() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_pg_submit_affirm(&conn_str);
    });
}

/// Live Deny resolution test against `postgres:18`.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_submit_deny_pg18() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_pg_submit_deny(&conn_str);
    });
}

/// Live Unknown resolution test against `postgres:18`.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_submit_unknown_pg18() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_pg_submit_unknown(&conn_str);
    });
}

/// Live sweep (TTL expiry) test against `postgres:18`.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_sweep_expired_pg18() {
    common::with_pg_and_conn("18", |_store, conn_str| {
        run_pg_sweep_expired(&conn_str);
    });
}

// ── Regression: open_postgres (no-oracle) unchanged ──────────────────────────

/// Regression guard: `open_postgres` (no oracle) must still compile and run correctly.
#[test]
#[ignore = "requires Docker (testcontainers); run with: cargo test -p mempill-postgres -- --ignored"]
fn postgres_no_oracle_constructor_unchanged_pg16() {
    common::with_pg_and_conn("16", |_store, conn_str| {
        use mempill_postgres::open_postgres;
        use mempill_core::{NoOpOracle, NoOpVector};

        let join = std::thread::spawn(move || {
            let engine = open_postgres::<NoOpOracle, NoOpVector>(
                &conn_str,
                None,
                None,
                EngineConfig::default(),
            )
            .expect("open_postgres (no-oracle) must succeed");

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("runtime must build");

            let result: Result<(), String> = rt.block_on(async {
                let agent = AgentId("pg-regression-agent".into());
                let resp = engine.ingest_claim(ingest_req(&agent, "TestValue")).await
                    .map_err(|e| format!("ingest failed: {e}"))?;
                if resp.disposition != Disposition::CommittedCheap {
                    return Err(format!(
                        "expected CommittedCheap; got {:?}", resp.disposition
                    ));
                }
                Ok(())
            });

            drop(engine);
            result
        });

        join.join().expect("regression thread must not panic")
            .expect("open_postgres regression must pass");
    });
}
