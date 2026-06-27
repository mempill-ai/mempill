//! W6 TTL/sweep/orphan-recovery QA tests (TASK-9 W6).
//!
//! These tests verify engine-enforced TTL expiry, the sweep routine, and orphan recovery.
//! All assertions go through `query_memory` to verify SURFACED BELIEF (Contested[both])
//! rather than raw dispositions — following the lesson from W4/W5-QA.
//!
//! # Tests
//!
//! 1. `ttl_expiry_via_submit_reverts_to_contested` — lazy expiry on submit_adjudication.
//! 2. `sweep_reverts_all_expired` — sweep returns correct count; non-expired untouched.
//! 3. `sweep_recovers_orphan_queued_claim` — orphan recovery via sweep.
//! 4. `non_expired_handle_still_resolvable` — TTL set but not elapsed → Affirm works.
//! 5. `expires_at_populated_at_ingest` — pending row has non-NULL expires_at when TTL configured.

use std::sync::Arc;
use std::time::Duration;

use mempill_core::{
    application::{IngestClaimRequest, QueryMemoryRequest},
    engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter},
    ports::OraclePort,
    EngineConfig, EngineHandle,
};
use mempill_sqlite::{
    connection::open_in_memory,
    store::SqlitePersistenceStore,
};
use mempill_types::{
    AgentId, AdjudicationResponse, AdjudicationVerdict, BeliefStatus, Cardinality,
    Confidence, Criticality, Disposition, ExternalKind, ProvenanceLabel,
};

// ── TestOracle (same pattern as oracle_belief_surfacing_qa.rs) ────────────────

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

// ── Engine builders ──────────────────────────────────────────────────────────

/// Build an engine with a specific EngineConfig (allows TTL configuration).
fn build_engine_with_config(
    oracle_uuid: uuid::Uuid,
    config: EngineConfig,
) -> EngineHandle<SqlitePersistenceStore, TestOracle, mempill_core::NoOpVector> {
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: oracle_uuid });

    EngineHandle::<_, _, mempill_core::NoOpVector>::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        config,
    )
}

/// Build an engine with a 1-hour TTL (so nothing expires by default).
fn build_engine_with_ttl(oracle_uuid: uuid::Uuid) -> EngineHandle<SqlitePersistenceStore, TestOracle, mempill_core::NoOpVector> {
    let config = EngineConfig {
        default_adjudication_ttl: Some(Duration::from_secs(3600)),
        ..EngineConfig::default()
    };
    build_engine_with_config(oracle_uuid, config)
}

fn ingest_req(agent: &AgentId, value: &str) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "acme".into(),
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
        subject: "acme".into(),
        predicate: "ceo".into(),
        as_of_tx_time: None,
    }
}

// ── Test 1: TTL expiry via submit_adjudication (lazy expiry) ─────────────────

/// Lazy expiry: when submit_adjudication is called on an expired handle,
/// the challenger is reverted to Contested + ledger entry written.
/// query_memory must surface Contested[both].
///
/// We simulate "already expired" by setting expires_at in the past via a config
/// with an extremely short TTL (1 millisecond), then waiting for a tick — or more
/// reliably by patching the pending row's expires_at via direct DB write.
///
/// Approach: ingest conflict (QueuedForAdjudication), then directly set expires_at
/// to a past time via the SQLite store's raw SQL (simulating elapsed TTL), then
/// call submit_adjudication which triggers lazy expiry.
#[tokio::test]
async fn ttl_expiry_via_submit_reverts_to_contested() {
    let handle_id = uuid::Uuid::new_v4();

    // Build engine with TTL configured (1 hour — we'll manually expire the row).
    let engine = build_engine_with_ttl(handle_id);
    let agent = AgentId("ttl-lazy-agent".into());

    // Ingest incumbent "alice".
    let resp_alice = engine.ingest_claim(ingest_req(&agent, "alice")).await
        .expect("ingest alice must succeed");
    assert_eq!(resp_alice.disposition, Disposition::CommittedCheap);

    // Ingest challenger "bob" → QueuedForAdjudication + pending row.
    let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await
        .expect("ingest bob must succeed");
    assert_eq!(resp_bob.disposition, Disposition::QueuedForAdjudication);

    // Manually expire the pending row by setting expires_at to the past.
    // We do this via the SQLite connection directly (simulating an elapsed TTL).
    // The engine holds an Arc<SqlitePersistenceStore>; we create a fresh connection
    // to the same in-memory DB isn't possible here — instead, create a second engine
    // sharing the same underlying connection isn't supported.
    //
    // Alternative approach: use a config with a 1-nanosecond TTL so that by the time
    // we call submit, it has elapsed. We need a separate engine with that TTL.
    //
    // Cleanest approach for this test: use the pending_store directly.
    // However, SqlitePendingStore doesn't expose an update method. Instead we
    // exercise the lazy expiry by building a fresh scenario with minimal TTL.

    // Build a new engine with a 1-nanosecond TTL so expires_at is set to just-past-now.
    let handle_id2 = uuid::Uuid::new_v4();
    let config_tiny_ttl = EngineConfig {
        default_adjudication_ttl: Some(Duration::from_nanos(1)),
        ..EngineConfig::default()
    };
    let engine2 = build_engine_with_config(handle_id2, config_tiny_ttl);
    let agent2 = AgentId("ttl-lazy-agent-2".into());

    // Ingest incumbent + challenger.
    let resp_alice2 = engine2.ingest_claim(ingest_req(&agent2, "alice")).await
        .expect("ingest alice must succeed");
    assert_eq!(resp_alice2.disposition, Disposition::CommittedCheap);

    let resp_bob2 = engine2.ingest_claim(ingest_req(&agent2, "bob")).await
        .expect("ingest bob must succeed");
    assert_eq!(resp_bob2.disposition, Disposition::QueuedForAdjudication,
        "bob must be QueuedForAdjudication with oracle present");

    // Sleep a tiny bit to ensure the 1ns TTL has elapsed.
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    // submit_adjudication on the now-expired handle → triggers lazy expiry.
    let result = engine2.submit_adjudication(
        handle_id2,
        AdjudicationResponse {
            handle_id: handle_id2,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await;

    // Must return AdjudicationHandleNotFound (handle expired).
    assert!(
        matches!(result, Err(mempill_core::MemError::AdjudicationHandleNotFound { .. })),
        "expired handle must return AdjudicationHandleNotFound; got: {result:?}"
    );

    // query_memory must surface Contested[both] after lazy expiry.
    let qr = engine2.query_memory(query_req(&agent2)).await
        .expect("query must succeed");
    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[TTL_LAZY_EXPIRY] status={status:?} primary={primary_val:?} alternatives={alt_vals:?}"
    );

    assert_eq!(
        *status, BeliefStatus::Contested,
        "After TTL expiry, query_memory MUST return Contested. Got {status:?}."
    );

    let surfaced_alice = primary_val == Some(serde_json::json!("alice"))
        || alt_vals.contains(&serde_json::json!("alice"));
    let surfaced_bob = primary_val == Some(serde_json::json!("bob"))
        || alt_vals.contains(&serde_json::json!("bob"));
    assert!(surfaced_alice, "alice (incumbent) must be visible after TTL expiry");
    assert!(surfaced_bob, "bob (challenger) must be visible as Contested after TTL expiry");
}

// ── Test 2: Sweep reverts all expired, leaves non-expired alone ───────────────

/// `sweep_expired_adjudications()` returns the count of reverted claims.
/// Expired rows → Contested. Non-expired rows → still resolvable by Affirm.
#[tokio::test]
async fn sweep_reverts_all_expired() {
    // Build an engine with a 1ns TTL so ingest rows expire immediately.
    let handle_expired_1 = uuid::Uuid::new_v4();

    // We need two oracles with different handle UUIDs. The engine fixture uses a single
    // TestOracle with a fixed UUID. For multiple conflicts we'd need separate engines.
    // Instead, use separate engines per subject to isolate each conflict.

    // Engine A: expires immediately (1ns TTL).
    let config_tiny = EngineConfig {
        default_adjudication_ttl: Some(Duration::from_nanos(1)),
        ..EngineConfig::default()
    };
    let engine_a = build_engine_with_config(handle_expired_1, config_tiny.clone());
    let agent_a = AgentId("sweep-agent-a".into());

    let _ = engine_a.ingest_claim(ingest_req(&agent_a, "alice")).await.expect("ingest alice-a");
    let _ = engine_a.ingest_claim(ingest_req(&agent_a, "bob")).await.expect("ingest bob-a");

    // Wait for TTL to elapse.
    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;

    // Sweep.
    let swept = engine_a.sweep_expired_adjudications().await.expect("sweep must succeed");
    assert_eq!(swept, 1, "sweep must revert exactly 1 expired pending row");

    // query_memory must surface Contested[both].
    let qr_a = engine_a.query_memory(query_req(&agent_a)).await.expect("query-a");
    let status_a = &qr_a.belief.status;
    println!("[SWEEP_EXPIRED] agent_a status={status_a:?}");
    assert_eq!(*status_a, BeliefStatus::Contested,
        "after sweep, agent_a subject must be Contested. Got {status_a:?}");

    // Engine B: non-expired (1 hour TTL). Sweep should NOT touch it.
    let handle_not_expired = uuid::Uuid::new_v4();
    let config_long = EngineConfig {
        default_adjudication_ttl: Some(Duration::from_secs(3600)),
        ..EngineConfig::default()
    };
    let engine_b = build_engine_with_config(handle_not_expired, config_long);
    let agent_b = AgentId("sweep-agent-b".into());

    let _ = engine_b.ingest_claim(ingest_req(&agent_b, "alice")).await.expect("ingest alice-b");
    let resp_b = engine_b.ingest_claim(ingest_req(&agent_b, "bob")).await.expect("ingest bob-b");
    assert_eq!(resp_b.disposition, Disposition::QueuedForAdjudication);

    // Sweep on engine_b: 0 expired rows.
    let swept_b = engine_b.sweep_expired_adjudications().await.expect("sweep-b");
    assert_eq!(swept_b, 0, "non-expired pending row must not be swept");

    // The non-expired handle must still be resolvable by Affirm.
    let outcome = engine_b.submit_adjudication(
        handle_not_expired,
        AdjudicationResponse {
            handle_id: handle_not_expired,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await.expect("Affirm on non-expired handle must succeed");
    assert_eq!(outcome.disposition, Disposition::CommittedCheap,
        "non-expired pending row must be resolvable by Affirm after sweep");
}

// ── Test 3: Sweep recovers orphan QueuedForAdjudication claim ────────────────

/// Orphan recovery: a claim with QueuedForAdjudication disposition but no pending row
/// (simulates the crash window between claim-commit and pending-insert).
///
/// We simulate this by:
///   1. Ingesting incumbent + challenger (challenger → QueuedForAdjudication + pending row).
///   2. Manually deleting the pending row (via direct SQLite write).
///   3. Calling sweep → it detects the orphan and reverts to Contested.
///   4. query_memory → Contested[both].
///
/// The "manual delete" is done by building a second engine against the same in-memory DB
/// isn't feasible. Instead, we use the pending_store's mark_resolved to mimic the row being
/// "consumed" — but that would also cause the sweep to skip it (mark_resolved sets status='resolved').
///
/// The cleanest simulation: insert a QueuedForAdjudication ledger entry directly via
/// the persistence port, without a corresponding pending row.
#[tokio::test]
async fn sweep_recovers_orphan_queued_claim() {
    use mempill_core::ports::PersistencePort;
    use mempill_sqlite::store::SqlitePersistenceStore;
    use mempill_types::{
        ClaimRef, Claim, ExternalAnchor, Fact, LedgerEntry, LedgerEventKind, TransactionTime, ValidTime,
    };

    let agent = AgentId("orphan-sweep-agent".into());

    // Build a raw persistence store (no engine, direct DB access to seed the orphan).
    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));

    let now = chrono::Utc::now();

    // Seed the incumbent claim (CommittedCheap).
    let incumbent_ref = ClaimRef(uuid::Uuid::new_v4());
    let incumbent_claim = Claim::new(
        incumbent_ref.clone(),
        agent.clone(),
        Fact { subject: "acme".into(), predicate: "ceo".into(), value: serde_json::json!("alice") },
        mempill_types::Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(now - chrono::Duration::seconds(10)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::High,
        vec![],
        None,
        None,
    );
    let mut txn_inc = persistence.begin_atomic(&agent).expect("begin txn for incumbent");
    persistence.append_claim(&mut txn_inc, &incumbent_claim).expect("insert incumbent claim");
    persistence.append_ledger_entry(&mut txn_inc, &LedgerEntry {
        entry_id: uuid::Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: incumbent_ref.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(now - chrono::Duration::seconds(10)),
    }).expect("insert incumbent ledger");
    persistence.commit(txn_inc).expect("commit incumbent");

    // Seed the orphaned challenger claim (QueuedForAdjudication, NO pending row).
    let challenger_ref = ClaimRef(uuid::Uuid::new_v4());
    let challenger_claim = Claim::new(
        challenger_ref.clone(),
        agent.clone(),
        Fact { subject: "acme".into(), predicate: "ceo".into(), value: serde_json::json!("bob") },
        mempill_types::Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(now),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::High,
        vec![],
        None,
        None,
    );
    let mut txn_ch = persistence.begin_atomic(&agent).expect("begin txn for challenger");
    persistence.append_claim(&mut txn_ch, &challenger_claim).expect("insert challenger claim");
    persistence.append_ledger_entry(&mut txn_ch, &LedgerEntry {
        entry_id: uuid::Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: challenger_ref.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::QueuedForAdjudication,
        rationale: None,
        recorded_at: TransactionTime(now),
    }).expect("insert challenger ledger");
    persistence.commit(txn_ch).expect("commit challenger");

    // Build an engine on top of the seeded persistence (no oracle UUID matters — no ingest).
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let dummy_handle = uuid::Uuid::new_v4();
    let oracle = Arc::new(TestOracle { fixed_uuid: dummy_handle });

    let engine = EngineHandle::<_, _, mempill_core::NoOpVector>::new_with_pending_store::<()>(
        persistence,
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        EngineConfig::default(),
    );

    // Confirm no pending rows exist (pure orphan state).
    // The sweep should detect challenger_ref as orphaned.
    let swept = engine.sweep_expired_adjudications().await.expect("sweep must succeed");
    assert_eq!(swept, 1, "sweep must recover exactly 1 orphaned claim");

    // query_memory must surface Contested[both].
    let qr = engine.query_memory(query_req(&agent)).await.expect("query must succeed");
    let status = &qr.belief.status;
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());
    let alt_vals: Vec<_> = qr.belief.alternatives.iter().map(|b| b.fact.value.clone()).collect();

    println!(
        "[ORPHAN_RECOVERY] status={status:?} primary={primary_val:?} alternatives={alt_vals:?}"
    );

    assert_eq!(
        *status, BeliefStatus::Contested,
        "After orphan recovery, query_memory MUST return Contested. Got {status:?}."
    );

    let surfaced_alice = primary_val == Some(serde_json::json!("alice"))
        || alt_vals.contains(&serde_json::json!("alice"));
    let surfaced_bob = primary_val == Some(serde_json::json!("bob"))
        || alt_vals.contains(&serde_json::json!("bob"));
    assert!(surfaced_alice, "alice (incumbent) must be visible after orphan recovery");
    assert!(surfaced_bob, "bob (orphaned challenger) must be visible as Contested");
}

// ── Test 4: Non-expired handle is still resolvable ───────────────────────────

/// TTL configured but not elapsed → submit Affirm still resolves.
/// query_memory surfaces challenger as the winner.
#[tokio::test]
async fn non_expired_handle_still_resolvable() {
    let handle_id = uuid::Uuid::new_v4();
    let engine = build_engine_with_ttl(handle_id); // 1-hour TTL — won't expire
    let agent = AgentId("ttl-non-expired-agent".into());

    let _ = engine.ingest_claim(ingest_req(&agent, "alice")).await.expect("ingest alice");
    let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await.expect("ingest bob");
    assert_eq!(resp_bob.disposition, Disposition::QueuedForAdjudication);

    // Submit Affirm on the non-expired handle.
    let outcome = engine.submit_adjudication(
        handle_id,
        AdjudicationResponse {
            handle_id,
            verdict: AdjudicationVerdict::Affirm,
            evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        },
    ).await.expect("Affirm must succeed on non-expired handle");

    assert_eq!(outcome.disposition, Disposition::CommittedCheap,
        "non-expired Affirm must resolve to CommittedCheap");

    let qr = engine.query_memory(query_req(&agent)).await.expect("query must succeed");
    let primary_val = qr.belief.primary.as_ref().map(|b| b.fact.value.clone());

    println!("[NON_EXPIRED] primary={primary_val:?}");

    // "bob" (challenger) wins after Affirm.
    assert_eq!(primary_val, Some(serde_json::json!("bob")),
        "after non-expired Affirm, challenger 'bob' must be the surfaced belief");
    assert_ne!(qr.belief.status, BeliefStatus::Contested,
        "after non-expired Affirm, must NOT be Contested");
}

// ── Test 5: expires_at is populated at ingest when TTL configured ─────────────

/// With TTL configured, the persisted pending row must have a non-NULL expires_at
/// that is approximately `queued_at + ttl`.
#[tokio::test]
async fn expires_at_populated_at_ingest() {
    use mempill_core::ports::PendingAdjudicationPort;

    let handle_id = uuid::Uuid::new_v4();
    let ttl_secs = 3600u64;
    let config = EngineConfig {
        default_adjudication_ttl: Some(Duration::from_secs(ttl_secs)),
        ..EngineConfig::default()
    };

    let conn = open_in_memory().expect("in-memory SQLite must open");
    let persistence = Arc::new(SqlitePersistenceStore::new(conn));
    let pending_store_raw = persistence.pending_store();
    let pending_adapter = ErasedPendingStoreAdapter::new(persistence.pending_store());
    let pending_store: Arc<dyn ErasedPendingStore> = Arc::new(pending_adapter);
    let oracle = Arc::new(TestOracle { fixed_uuid: handle_id });

    let engine = EngineHandle::<_, _, mempill_core::NoOpVector>::new_with_pending_store::<()>(
        Arc::clone(&persistence),
        Some(oracle),
        None::<Arc<mempill_core::NoOpVector>>,
        pending_store,
        config,
    );

    let agent = AgentId("ttl-expires-at-agent".into());

    let before_ingest = chrono::Utc::now();

    let _ = engine.ingest_claim(ingest_req(&agent, "alice")).await.expect("ingest alice");
    let resp_bob = engine.ingest_claim(ingest_req(&agent, "bob")).await.expect("ingest bob");
    assert_eq!(resp_bob.disposition, Disposition::QueuedForAdjudication);

    let after_ingest = chrono::Utc::now();

    // Retrieve the pending row.
    let row = pending_store_raw
        .get_pending(handle_id)
        .expect("get_pending must succeed")
        .expect("pending row must exist");

    println!(
        "[EXPIRES_AT] queued_at={} expires_at={:?}",
        row.queued_at, row.expires_at
    );

    let expires_at = row.expires_at.expect("expires_at must be non-NULL when TTL configured");

    // expires_at must be within [queued_at + ttl - 1s, queued_at + ttl + 2s] accounting for
    // timing jitter between before_ingest and the actual queued_at stamped by the engine.
    let min_expected = before_ingest + chrono::Duration::seconds(ttl_secs as i64 - 1);
    let max_expected = after_ingest + chrono::Duration::seconds(ttl_secs as i64 + 2);

    assert!(
        expires_at >= min_expected && expires_at <= max_expected,
        "expires_at ({expires_at}) must be approx queued_at + {ttl_secs}s (expected [{min_expected}, {max_expected}])"
    );
}
