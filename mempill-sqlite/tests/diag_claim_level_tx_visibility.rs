//! DIAG: Claim-level transaction-time visibility probe.
//!
//! TASK_ID: DIAG-claim-level-tx-visibility
//!
//! ## Suspected Bug
//!
//! `load_subject_line` returns ALL claims for a subject-line with NO `tx_time <= as_of`
//! filter. The fold's `as_of_tx_time` parameter filters:
//!   (a) `ValidityAssertion::asserted_at > as_of` — skipped in `is_claim_live`
//!   (b) `ledger_entries.recorded_at > as_of`     — skipped via `load_ledger_for_claims(as_of)`
//!
//! But claim rows whose own `transaction_time > as_of` are never excluded.
//! A claim ingested AFTER the as-of point is still returned by `load_subject_line` and
//! handed to the fold as if it existed at the queried time — violating bi-temporal
//! transaction-time semantics ("as of T, that claim did not yet exist").
//!
//! ## Probe Scenario
//!
//! - Ingest claim A: subject="acme", predicate="hq", value="berlin"
//!                   at transaction-time T1 = 2020-01-01 (no valid_time).
//! - Ingest claim B: subject="acme", predicate="hq", value="munich"
//!                   at transaction-time T3 = 2026-01-01 (no valid_time, conflicts with A).
//! - Query with as_of_tx_time = T2 = 2023-01-01 (strictly between T1 and T3).
//!
//! ## Correct bi-temporal behavior
//!
//! At T2, only claim A existed (B's transaction_time T3 > T2).
//! The fold should see only A and surface "berlin" (Contested or TimingUncertain with A
//! as the sole live claim, since both have no valid_time — but crucially NOT reflecting B).
//!
//! ## What this test actually shows (BUG CONFIRMED — hence #[ignore])
//!
//! The query returns Contested with BOTH A and B visible, because `load_subject_line`
//! returns all claims regardless of tx_time. Claim B (tx_time T3 > T2) leaks through
//! the tx-time filter and is presented to the fold as if it existed at T2.
//!
//! ## Root cause
//!
//! `mempill-sqlite/src/store.rs` `load_subject_line` (L713-728):
//!
//! ```sql
//! SELECT ... FROM claims
//! WHERE agent_id = ?1 AND subject = ?2 AND predicate = ?3
//! ORDER BY tx_time ASC
//! ```
//!
//! No `AND tx_time <= ?4` clause.  The `PersistencePort::load_subject_line` trait signature
//! (`mempill-core/src/ports/persistence.rs` L56-61) also carries no `as_of` parameter —
//! so the filter cannot be applied without a signature change.
//!
//! The fold in `truth_engine::fold` (`mempill-core/src/engine/truth_engine.rs` L168-281)
//! calls `assertions_for(cref)` → `is_claim_live` for each claim.  `is_claim_live` (L108-142)
//! filters assertions by `assertion.asserted_at.0 > as_of_tx_time` but has no analogous check
//! on `claim.transaction_time().0 > as_of_tx_time`.
//!
//! `query_memory.rs` L53-55 calls `load_subject_line` without passing `as_of`; L64-69 calls
//! `load_ledger_for_claims` WITH `as_of` (disposition filter only — not a claim-level filter).
//!
//! ## No existing test that catches this
//!
//! `temporal_succession_task11.rs::succession_past_instant` explicitly documents the gap
//! (lines 153-156): "The fold does NOT filter claims by tx_time — the persistence layer
//! returns ALL stored claims regardless of as_of. So both Alice and Bob are visible to the
//! fold." That test passes only because it relies on valid-time instant-selection (Alice's
//! window [2020, 2024) covers 2022-06-01) — not because the claim-level tx filter is correct.
//! With no valid_time on both claims (the scenario here), the bug surfaces directly.
//!
//! ## Recommended fix (sketch — do NOT implement here)
//!
//! Add `as_of_tx_time: Option<DateTime<Utc>>` to `PersistencePort::load_subject_line`
//! (trait signature change) and apply `AND tx_time <= ?4` in the SQLite implementation
//! (and equivalently in the Postgres store). This mirrors the existing fix applied to
//! `load_ledger_for_claims`. `query_memory.rs` passes `Some(as_of)` through. The fold
//! itself remains pure and unchanged — the claim-level cutoff is enforced at the DB layer,
//! exactly where the ledger cutoff already lives.

#![allow(missing_docs)]

use chrono::{TimeZone, Utc};
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_core::application::ingest_claim::IngestClaimUseCase;
use mempill_core::application::query_memory::QueryMemoryUseCase;
use mempill_core::noop::{NoOpOracle, NoOpVector};
use mempill_core::config::EngineConfig;
use mempill_sqlite::store::SqlitePersistenceStore;
use mempill_sqlite::connection;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality,
    ExternalKind, ProvenanceLabel,
};
use std::sync::Arc;

fn dt(y: i32, m: u32, d: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
}

fn make_request(
    agent: AgentId,
    value: &str,
) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent,
        subject: "acme".into(),
        predicate: "hq".into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None, // no valid_time: relies purely on tx-time ordering
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

/// PROBE: Claim-level transaction-time visibility.
///
/// This test is marked `#[ignore]` because it DOCUMENTS A BUG:
/// the query with `as_of_tx_time = T2` incorrectly reflects claim B
/// (ingested at T3 > T2), returning Contested instead of treating B as
/// invisible (as required by bi-temporal tx-time semantics).
///
/// Expected (correct bi-temporal):  as_of=T2, only claim A (berlin) exists → A is the
///                                  single live claim (TimingUncertain since no valid_time).
/// Actual (buggy):                  as_of=T2, BOTH A and B are returned by load_subject_line
///                                  → fold sees two Functional claims → Contested.
///                                  B's transaction_time T3 (2026-01-01) > T2 (2023-01-01)
///                                  but is NOT filtered out.
#[tokio::test]
#[ignore = "BUG: documents claim-level tx-time visibility gap — B (tx_time T3 > T2) \
             is incorrectly visible when querying as_of=T2; fix: add tx_time <= as_of \
             filter to load_subject_line (see module-level doc for root cause + fix sketch)"]
async fn probe_claim_level_tx_time_visibility_bug() {
    // ── Setup: in-memory SQLite store + use-cases ─────────────────────────────
    let conn = connection::open_in_memory().expect("in-memory connection must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = AgentId("diag-tx-vis-agent".into());
    let config = EngineConfig::default();

    let ingest_uc = IngestClaimUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpOracle>>,
        None,
        config.clone(),
    );
    let query_uc = QueryMemoryUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpVector>>,
        config.clone(),
    );

    // T1 = 2020-01-01: claim A ingested (berlin)
    let t1 = dt(2020, 1, 1);
    // T2 = 2023-01-01: the as-of query point (between T1 and T3)
    let t2 = dt(2023, 1, 1);
    // T3 = 2026-01-01: claim B ingested (munich), AFTER T2
    let t3 = dt(2026, 1, 1);

    // ── Ingest claim A at T1 ──────────────────────────────────────────────────
    let resp_a = ingest_uc
        .execute_with_time(make_request(agent.clone(), "berlin"), t1)
        .expect("ingest A (berlin) at T1 must succeed");
    // First claim, no conflict: CommittedCheap.
    assert_eq!(
        resp_a.disposition,
        mempill_types::Disposition::CommittedCheap,
        "claim A (berlin) ingested at T1 must be CommittedCheap"
    );
    let claim_a_ref = resp_a.claim_ref.clone();

    // ── Ingest claim B at T3 ──────────────────────────────────────────────────
    // B is a Functional conflict with A (same subject+predicate, different value, no valid_time).
    let resp_b = ingest_uc
        .execute_with_time(make_request(agent.clone(), "munich"), t3)
        .expect("ingest B (munich) at T3 must succeed");
    // Second conflicting claim with no valid_time and no oracle → Contested.
    assert_eq!(
        resp_b.disposition,
        mempill_types::Disposition::Contested,
        "claim B (munich) ingested at T3 must be Contested (no valid_time conflict)"
    );
    let claim_b_ref = resp_b.claim_ref.clone();

    // Sanity-check that both claims are in the store and B's tx_time is T3.
    // (If load_subject_line filtered correctly, B would be invisible at T2.)
    println!(
        "[probe] claim_a_ref={:?}, claim_b_ref={:?}",
        claim_a_ref, claim_b_ref
    );

    // ── Query at T2 (as_of_tx_time = 2023-01-01) ─────────────────────────────
    // CORRECT bi-temporal answer: at T2, claim B does not yet exist (tx_time T3 > T2).
    // Only claim A exists. With no valid_time, A is TimingUncertain (single live claim).
    // INCORRECT (buggy) answer: both A and B are returned by load_subject_line,
    // fold sees two Functional claims → Contested. B "leaks" through the tx-time boundary.
    let query_at_t2 = QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "hq".into(),
        as_of_tx_time: Some(t2),
        valid_at: None,
    };
    // query_now is irrelevant for this test (only used for currency decay); use T3+1d.
    let query_now = t3 + chrono::Duration::days(1);
    let resp = query_uc
        .execute_with_time(query_at_t2, query_now)
        .expect("query at T2 must succeed");

    println!(
        "[probe] query as_of=T2 result: status={:?}, primary={:?}, alternatives={}",
        resp.belief.status,
        resp.belief.primary.as_ref().map(|b| &b.fact.value),
        resp.belief.alternatives.len()
    );

    // ── ASSERT: correct bi-temporal behavior ─────────────────────────────────
    // At T2, only A (berlin) existed. B (munich, tx_time=T3) must be invisible.
    //
    // BUG: this assertion FAILS — the actual status is Contested because both A and B
    // are returned by load_subject_line (no tx_time filter).
    assert!(
        matches!(resp.belief.status, BeliefStatus::TimingUncertain | BeliefStatus::Resolved),
        "CORRECT bi-temporal: as_of=T2 (between T1 and T3) → only claim A (berlin) exists \
         → belief must be TimingUncertain or Resolved (single live claim, no valid_time). \
         ACTUAL (BUG): status={:?}; B (munich, tx_time=T3 > T2) leaked through — \
         load_subject_line has no tx_time <= as_of filter.",
        resp.belief.status
    );

    let primary_val = resp
        .belief
        .primary
        .as_ref()
        .map(|b| b.fact.value.clone());
    assert_eq!(
        primary_val,
        Some(serde_json::json!("berlin")),
        "CORRECT: as_of=T2 primary must be 'berlin' (claim A). \
         ACTUAL (BUG): got {:?} — claim B (munich) is visible despite tx_time T3 > T2",
        primary_val
    );
}

/// ADDITIONAL PROBE: verifying claim B's transaction_time is indeed T3 > T2.
///
/// This sanity check confirms the setup is correct — the `execute_with_time` API
/// stamps the claim with the injected `now` as its `transaction_time`.
/// It also confirms that without `as_of_tx_time`, the current view correctly
/// shows Contested (both A and B visible at "now").
///
/// This test PASSES (it documents the "current view" behavior, not the tx-time travel bug).
#[tokio::test]
async fn probe_claim_level_tx_time_sanity_current_view() {
    let conn = connection::open_in_memory().expect("in-memory connection must open");
    let store = Arc::new(SqlitePersistenceStore::new(conn));
    let agent = AgentId("diag-tx-vis-sanity-agent".into());
    let config = EngineConfig::default();

    let ingest_uc = IngestClaimUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpOracle>>,
        None,
        config.clone(),
    );
    let query_uc = QueryMemoryUseCase::new(
        Arc::clone(&store),
        None::<Arc<NoOpVector>>,
        config.clone(),
    );

    let t1 = dt(2020, 1, 1);
    let t3 = dt(2026, 1, 1);

    // Ingest A at T1, B at T3.
    ingest_uc
        .execute_with_time(make_request(agent.clone(), "berlin"), t1)
        .expect("ingest A must succeed");
    ingest_uc
        .execute_with_time(make_request(agent.clone(), "munich"), t3)
        .expect("ingest B must succeed");

    // Query as_of=None (current view): both A and B are live from a tx-time perspective
    // (no as_of filter). With no valid_time, no oracle → Contested is expected.
    let query_current = QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "hq".into(),
        as_of_tx_time: None,
        valid_at: None,
    };
    let query_now = t3 + chrono::Duration::days(1);
    let resp_current = query_uc
        .execute_with_time(query_current, query_now)
        .expect("current-view query must succeed");

    println!(
        "[sanity] current-view result: status={:?}, primary={:?}",
        resp_current.belief.status,
        resp_current.belief.primary.as_ref().map(|b| &b.fact.value)
    );

    // Current view: both A (CommittedCheap) and B (Contested) are live.
    // Without a valid_time, two Functional claims in the fold → Contested.
    assert_eq!(
        resp_current.belief.status,
        BeliefStatus::Contested,
        "current-view (as_of=None) with two conflicting no-valid_time claims must be Contested"
    );

    // ── Also probe the disposition-filter path works correctly ────────────────
    // At T1+1d (after A, before B), load_ledger_for_claims with as_of=T1+1d should exclude B's
    // ledger entry (recorded at T3). This sub-path WORKS correctly (the ledger fix is in place).
    // The bug is specifically that load_subject_line itself has no such filter.
    let t2 = dt(2023, 1, 1); // between T1 and T3
    let query_t2 = QueryMemoryRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
        predicate: "hq".into(),
        as_of_tx_time: Some(t2),
        valid_at: None,
    };
    let resp_t2 = query_uc
        .execute_with_time(query_t2, query_now)
        .expect("T2 query must succeed");

    println!(
        "[sanity] T2 query result (SHOWS BUG): status={:?}, primary={:?}, alternatives={}",
        resp_t2.belief.status,
        resp_t2.belief.primary.as_ref().map(|b| &b.fact.value),
        resp_t2.belief.alternatives.len()
    );

    // Document the ACTUAL (buggy) behavior: Contested because B leaks through.
    // We assert the bug is present here so this passing test documents the concrete failure mode.
    assert_eq!(
        resp_t2.belief.status,
        BeliefStatus::Contested,
        "BUG DOCUMENTED: as_of=T2, B (tx_time T3 > T2) leaks through load_subject_line \
         → fold sees two live Functional claims → Contested. \
         CORRECT answer would be TimingUncertain (only A visible at T2)."
    );
}
