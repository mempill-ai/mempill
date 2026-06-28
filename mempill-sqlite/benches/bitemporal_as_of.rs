//! Bi-temporal as-of micro-benchmark — internal regression and overhead tool.
//!
//! # What this bench measures
//!
//! mempill supports two independent time axes: the `valid_at` axis
//! (point-in-time on the valid-time dimension: "who was CEO in 2021?") and
//! the `as_of_tx_time` axis (transaction-time: "what did we believe
//! as of a given transaction instant?"). This benchmark exercises the query path
//! for each axis in isolation and in combination, to observe how read cost scales
//! with history depth N.
//!
//! ## Scenarios
//!
//! All scenarios build a succession corpus of N non-overlapping claims on a single
//! `(agent, subject, predicate)` line via the real SQLite adapter (end-to-end, no mocks).
//! The corpus is built OUTSIDE the measured closure; only the query call is timed.
//!
//! - **Bench A** — `valid_at` point query: query with an explicit `valid_at` instant
//!   that falls inside one of the succession windows. Measures the cost of independent
//!   valid-time axis selection.
//!
//! - **Bench B** — `as_of_tx_time` query: query with an explicit `as_of_tx_time` that
//!   rewinds transaction time to just after the N-th ingest. Measures tx-time filtering cost.
//!
//! - **Bench C** — combined `valid_at + as_of_tx_time`: both axes set independently.
//!   This is the full D2-independence case.
//!
//! - **Bench D** — baseline (neither axis set): `query_memory` with current belief, no
//!   time-travel. Reference point for overhead comparison.
//!
//! All queries use `execute_with_time(req, fixed_now)` with deterministic fixed timestamps —
//! no `Utc::now()` inside the measured loop.
//!
//! ## How to run
//!
//! ```sh
//! cargo bench -p mempill-sqlite
//! # Or a specific bench group:
//! cargo bench -p mempill-sqlite -- bench_a_valid_at
//! ```
//!
//! HTML reports land in `target/criterion/`.

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use mempill_core::{
    EngineConfig, NoOpOracle, NoOpVector,
    application::{
        IngestClaimRequest, IngestClaimResponse, IngestClaimUseCase, QueryMemoryRequest,
        QueryMemoryUseCase,
    },
};
use mempill_sqlite::SqlitePersistenceStore;
use mempill_types::{AgentId, Cardinality, Confidence, Criticality, ExternalKind, ProvenanceLabel, ValidTime};

// ── Fixed deterministic timestamps ───────────────────────────────────────────
//
// Epoch 2000-01-01 is the base. Each succession window spans exactly one year.
// Tx-times are set to 2050-01-01 (far future relative to all valid windows) so the
// tx-time filter never excludes any claim by default.

/// Start of the succession epoch (2000-01-01 UTC).
fn epoch() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap()
}

/// The fixed "now" injected into all query calls — far future, never clips the corpus.
fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap()
}

/// Tx-time used when ingesting claims — after all valid windows so B7 gate passes.
fn ingest_tx_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2050, 1, 1, 0, 0, 0).unwrap()
}

// ── Corpus builder ────────────────────────────────────────────────────────────

struct Corpus {
    store: Arc<SqlitePersistenceStore>,
    agent_id: AgentId,
    subject: String,
    predicate: String,
    /// tx-time at which each claim was ingested (deterministic).
    ingest_tx_times: Vec<DateTime<Utc>>,
    /// valid-time start for each slot i (= epoch + i years).
    valid_starts: Vec<DateTime<Utc>>,
    /// Responses from each ingest call (claim refs, dispositions).
    _responses: Vec<IngestClaimResponse>,
}

/// Build a succession corpus of `n` non-overlapping claims.
///
/// Each claim is ingested with `execute_with_time` using a fixed tx-time so the corpus
/// is fully deterministic. The ingest calls themselves are NOT benchmarked.
fn build_corpus(n: usize) -> Corpus {
    use mempill_sqlite::connection::open_in_memory;

    let conn = open_in_memory().expect("in-memory SQLite must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent_id = AgentId("bench-agent".into());
    let subject = "acme".to_string();
    let predicate = "ceo".to_string();
    let config = EngineConfig::default();

    let ingest_uc = IngestClaimUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpOracle>>,
        None,
        config.clone(),
    );

    let tx_now = ingest_tx_time();
    let mut ingest_tx_times = Vec::with_capacity(n);
    let mut valid_starts = Vec::with_capacity(n);
    let mut responses = Vec::with_capacity(n);

    for i in 0..n {
        // Each window: [epoch + i years, epoch + (i+1) years)
        let start = epoch() + chrono::Duration::days(i as i64 * 365);
        let end = epoch() + chrono::Duration::days((i + 1) as i64 * 365);

        // Stagger tx-times by 1 second per claim so the tx-time axis is separable.
        let claim_tx = tx_now + chrono::Duration::seconds(i as i64);

        valid_starts.push(start);
        ingest_tx_times.push(claim_tx);

        let req = IngestClaimRequest {
            agent_id: agent_id.clone(),
            subject: subject.clone(),
            predicate: predicate.clone(),
            value: serde_json::json!(format!("ceo-{i}")),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: Some(ValidTime {
                start: Some(start),
                end: Some(end),
                valid_time_confidence: 0.9,
                granularity: None,
            }),
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        };
        let resp = ingest_uc.execute_with_time(req, claim_tx)
            .expect("corpus ingest must succeed");
        responses.push(resp);
    }

    Corpus {
        store,
        agent_id,
        subject,
        predicate,
        ingest_tx_times,
        valid_starts,
        _responses: responses,
    }
}

// ── Bench A — valid_at point query ───────────────────────────────────────────

/// Bench A: query with `valid_at` set to the mid-point of window index `window_idx`.
///
/// Measures independent valid-time axis selection cost as N grows.
fn bench_a_valid_at(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_a_valid_at");

    for &n in &[1usize, 5, 20, 50] {
        let corpus = build_corpus(n);
        let store = Arc::clone(&corpus.store);
        let config = EngineConfig::default();
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            config,
        );
        let now = fixed_now();

        // Query at the mid-point of the first window (index 0 always exists).
        let window_idx = 0;
        let valid_at = corpus.valid_starts[window_idx]
            + chrono::Duration::days(180); // mid-year

        let req = QueryMemoryRequest {
            agent_id: corpus.agent_id.clone(),
            subject: corpus.subject.clone(),
            predicate: corpus.predicate.clone(),
            as_of_tx_time: None,
            valid_at: Some(valid_at),
        };

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                query_uc.execute_with_time(req.clone(), now)
                    .expect("bench_a query must succeed")
            });
        });
    }

    group.finish();
}

// ── Bench B — as_of_tx_time query ────────────────────────────────────────────

/// Bench B: query with `as_of_tx_time` set to just after the last ingest tx-time.
///
/// Measures transaction-time filtering cost as N grows.
fn bench_b_as_of_tx_time(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_b_as_of_tx_time");

    for &n in &[1usize, 5, 20, 50] {
        let corpus = build_corpus(n);
        let store = Arc::clone(&corpus.store);
        let config = EngineConfig::default();
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            config,
        );
        let now = fixed_now();

        // as_of = just after the last claim's tx-time (sees all N claims).
        let last_tx = corpus.ingest_tx_times[n - 1];
        let as_of = last_tx + chrono::Duration::seconds(1);

        let req = QueryMemoryRequest {
            agent_id: corpus.agent_id.clone(),
            subject: corpus.subject.clone(),
            predicate: corpus.predicate.clone(),
            as_of_tx_time: Some(as_of),
            valid_at: None,
        };

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                query_uc.execute_with_time(req.clone(), now)
                    .expect("bench_b query must succeed")
            });
        });
    }

    group.finish();
}

// ── Bench C — combined valid_at + as_of_tx_time (D2 independence) ────────────

/// Bench C: both axes set independently — the full D2-independence case.
///
/// Measures the combined cost of tx-time filtering AND valid-time axis selection.
fn bench_c_combined(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_c_combined_valid_at_and_as_of");

    for &n in &[1usize, 5, 20, 50] {
        let corpus = build_corpus(n);
        let store = Arc::clone(&corpus.store);
        let config = EngineConfig::default();
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            config,
        );
        let now = fixed_now();

        // valid_at: mid-point of window 0.
        let valid_at = corpus.valid_starts[0] + chrono::Duration::days(180);
        // as_of_tx_time: after all ingests.
        let last_tx = corpus.ingest_tx_times[n - 1];
        let as_of = last_tx + chrono::Duration::seconds(1);

        let req = QueryMemoryRequest {
            agent_id: corpus.agent_id.clone(),
            subject: corpus.subject.clone(),
            predicate: corpus.predicate.clone(),
            as_of_tx_time: Some(as_of),
            valid_at: Some(valid_at),
        };

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                query_uc.execute_with_time(req.clone(), now)
                    .expect("bench_c query must succeed")
            });
        });
    }

    group.finish();
}

// ── Bench D — baseline (current belief, no time-travel) ──────────────────────

/// Bench D: neither axis set — `query_memory` with current-belief semantics.
///
/// Reference: shows the overhead floor for the query path without any time-travel.
fn bench_d_baseline(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_d_baseline_current_belief");

    for &n in &[1usize, 5, 20, 50] {
        let corpus = build_corpus(n);
        let store = Arc::clone(&corpus.store);
        let config = EngineConfig::default();
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&store),
            None::<Arc<NoOpVector>>,
            config,
        );
        let now = fixed_now();

        let req = QueryMemoryRequest {
            agent_id: corpus.agent_id.clone(),
            subject: corpus.subject.clone(),
            predicate: corpus.predicate.clone(),
            as_of_tx_time: None,
            valid_at: None,
        };

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                query_uc.execute_with_time(req.clone(), now)
                    .expect("bench_d query must succeed")
            });
        });
    }

    group.finish();
}

// ── Criterion wiring ──────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_a_valid_at,
    bench_b_as_of_tx_time,
    bench_c_combined,
    bench_d_baseline,
);
criterion_main!(benches);
