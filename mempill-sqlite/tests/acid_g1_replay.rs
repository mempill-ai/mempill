//! ACID test G1 — Deterministic belief from persisted state (TECHNICAL_DESIGN.md §9).
//!
//! G1 SCOPE IN v0.1:
//! True ledger-replay (re-running adjudication from the event log) is NOT implemented
//! in v0.1. The adjudication gate (C7) is a pure deterministic function (G1 invariant),
//! but the engine does not expose a "replay from ledger" API. The persisted state IS the
//! beliefs: claims + validity_assertions.
//!
//! WHAT WE CAN PROVE IN v0.1 (and DO prove here):
//!   G1a — Same persisted claims → identical derived belief across FRESH PROCESS INSTANCES
//!         (two separate `DefaultEngine` instances opened on the same file-backed DB).
//!   G1b — Same belief across two READS within one engine instance (read-time-canonical, I8).
//!   G1c — Multiple non-conflicting claims on different predicates persist correctly
//!         across fresh engine instances.
//!   G1d — Independent predicates are queried deterministically (per-subject-line isolation).
//!   G1e — Supersession determinism: a sequence containing a supersession produces IDENTICAL
//!         belief across two fresh engine instances on the same file-backed DB.
//!         (DEFECT-1 FIXED: HeavyPath is now reachable end-to-end via DefaultEngine.)
//!
//! These together prove: "same persisted facts → same read-time canonical belief" — the
//! achievable G1 guarantee in v0.1, including through the supersession (HeavyPath) path.
//!
//! TECHNIQUE: use `tempfile::NamedTempFile` for a file-backed SQLite DB. Open Engine1,
//! ingest claims, query → capture belief. Open Engine2 on SAME file, query → assert identical.

use mempill_sqlite::open_default;
use mempill_core::application::{IngestClaimRequest, QueryMemoryRequest};
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality,
    Disposition, ExternalKind, ProvenanceLabel,
};
use tempfile::NamedTempFile;

// ── helpers ───────────────────────────────────────────────────────────────────

fn agent() -> AgentId {
    AgentId("g1-agent".into())
}

/// Ingest a non-conflicting Functional claim (unique predicate).
fn ingest_req(
    agent_id: AgentId,
    subject: &str,
    predicate: &str,
    value: &str,
) -> IngestClaimRequest {
    IngestClaimRequest {
        agent_id,
        subject: subject.into(),
        predicate: predicate.into(),
        value: serde_json::json!(value),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    }
}

fn query_req(agent_id: AgentId, subject: &str, predicate: &str) -> QueryMemoryRequest {
    QueryMemoryRequest {
        agent_id,
        subject: subject.into(),
        predicate: predicate.into(),
        as_of_tx_time: None,
    }
}

// ── G1a: same file → same belief across two fresh engine instances ────────────

/// G1a: ingest a claim, open a SECOND engine on the same file, query → same belief.
/// Proves the persisted state is canonical and deterministic across process restarts.
#[tokio::test]
async fn g1a_fresh_engine_same_file_same_belief() {
    let tmp = NamedTempFile::new().expect("tempfile must be created");
    let db_path = tmp.path().to_str().expect("path must be UTF-8").to_string();

    let claim_ref_ingested;
    let value_ingested: serde_json::Value;

    // ── Engine 1: ingest a claim ──────────────────────────────────────────────
    {
        let engine1 = open_default(&db_path).expect("file-backed engine1 must open");
        let resp = engine1
            .ingest_claim(ingest_req(agent(), "user", "home_city", "Berlin"))
            .await
            .expect("ingest must succeed on engine1");

        assert_eq!(resp.disposition, Disposition::CommittedCheap, "first claim must be CommittedCheap");
        claim_ref_ingested = resp.claim_ref.clone();

        // Query on engine1 and capture belief.
        let q1 = engine1.query_memory(query_req(agent(), "user", "home_city")).await
            .expect("query on engine1 must succeed");

        assert!(
            matches!(q1.belief.status, BeliefStatus::Resolved | BeliefStatus::TimingUncertain),
            "engine1 query must yield Resolved or TimingUncertain, got {:?}",
            q1.belief.status
        );
        let primary = q1.belief.primary.as_ref()
            .expect("engine1 must return a primary belief");
        value_ingested = primary.fact.value.clone();
        assert_eq!(primary.claim_ref, claim_ref_ingested, "engine1 primary claim_ref must match ingested");
        // Engine1 drops here — connection closed.
    }

    // ── Engine 2: fresh instance on SAME file — must read identical belief ────
    {
        let engine2 = open_default(&db_path).expect("file-backed engine2 must open (same file)");
        let q2 = engine2.query_memory(query_req(agent(), "user", "home_city")).await
            .expect("query on engine2 must succeed");

        assert!(
            matches!(q2.belief.status, BeliefStatus::Resolved | BeliefStatus::TimingUncertain),
            "G1a: engine2 (fresh instance, same file) must return Resolved or TimingUncertain, got {:?}",
            q2.belief.status
        );

        let primary2 = q2.belief.primary.as_ref()
            .expect("G1a: engine2 must return a primary belief (same persisted data)");

        assert_eq!(
            primary2.fact.value, value_ingested,
            "G1a: fact value MUST be identical across fresh engine instances on same file"
        );
        assert_eq!(
            primary2.claim_ref, claim_ref_ingested,
            "G1a: claim_ref MUST be identical across fresh engine instances on same file"
        );
        assert_eq!(primary2.fact.subject, "user");
        assert_eq!(primary2.fact.predicate, "home_city");
    }
}

// ── G1b: two reads within one engine → identical belief (I8 read-canonical) ──

/// G1b: query the same subject-line twice within one engine instance → identical belief.
/// This proves read-time-canonical determinism (I8) — no stochastic variation on read.
#[tokio::test]
async fn g1b_two_reads_same_engine_identical_belief() {
    let engine = mempill_sqlite::open_default_in_memory()
        .expect("in-memory engine must open");
    let agent = agent();

    let ingest = engine.ingest_claim(ingest_req(
        agent.clone(), "user", "work_city", "Vienna",
    )).await.expect("ingest must succeed");
    assert_eq!(ingest.disposition, Disposition::CommittedCheap);

    // Query twice.
    let resp1 = engine.query_memory(query_req(agent.clone(), "user", "work_city")).await
        .expect("first query must succeed");
    let resp2 = engine.query_memory(query_req(agent.clone(), "user", "work_city")).await
        .expect("second query must succeed");

    // Status must be identical.
    assert_eq!(
        resp1.belief.status, resp2.belief.status,
        "G1b/I8: belief status must be identical on repeated reads"
    );

    let primary1 = resp1.belief.primary.as_ref().expect("first query must have primary");
    let primary2 = resp2.belief.primary.as_ref().expect("second query must have primary");

    assert_eq!(
        primary1.claim_ref, primary2.claim_ref,
        "G1b/I8: claim_ref must be identical on repeated reads"
    );
    assert_eq!(
        primary1.fact.value, primary2.fact.value,
        "G1b/I8: fact value must be identical on repeated reads"
    );
    assert_eq!(
        primary1.provenance, primary2.provenance,
        "G1b/I8: provenance must be identical on repeated reads"
    );
}

// ── G1c: multiple non-conflicting claims (different predicates) persist ────────

/// G1c: ingest two claims on DIFFERENT predicates (no conflict possible), then verify
/// a fresh engine on the same file sees both claims correctly.
#[tokio::test]
async fn g1c_multiple_predicates_persist_across_fresh_engine_instances() {
    let tmp = NamedTempFile::new().expect("tempfile must be created");
    let db_path = tmp.path().to_str().expect("path must be UTF-8").to_string();

    let ref_city;
    let ref_job;

    // ── Engine 1: ingest two claims on different predicates ───────────────────
    {
        let engine1 = open_default(&db_path).expect("engine1 must open");
        let agent = agent();

        let r_city = engine1.ingest_claim(ingest_req(
            agent.clone(), "user", "city_g1c", "Munich",
        )).await.expect("ingest city must succeed");
        assert_eq!(r_city.disposition, Disposition::CommittedCheap);
        ref_city = r_city.claim_ref.clone();

        let r_job = engine1.ingest_claim(ingest_req(
            agent.clone(), "user", "job_g1c", "Engineer",
        )).await.expect("ingest job must succeed");
        assert_eq!(r_job.disposition, Disposition::CommittedCheap);
        ref_job = r_job.claim_ref.clone();

        // Verify both retrievable on engine1.
        let qc = engine1.query_memory(query_req(agent.clone(), "user", "city_g1c")).await
            .expect("engine1 city query must succeed");
        assert_eq!(
            qc.belief.primary.as_ref().map(|b| &b.claim_ref),
            Some(&ref_city),
            "engine1 city claim_ref must match"
        );

        let qj = engine1.query_memory(query_req(agent.clone(), "user", "job_g1c")).await
            .expect("engine1 job query must succeed");
        assert_eq!(
            qj.belief.primary.as_ref().map(|b| &b.claim_ref),
            Some(&ref_job),
            "engine1 job claim_ref must match"
        );
        // Engine1 drops here.
    }

    // ── Engine 2: same file, fresh instance — both claims must still be present ─
    {
        let engine2 = open_default(&db_path).expect("engine2 must open (same file)");
        let agent = agent();

        // city claim must be identical.
        let qc2 = engine2.query_memory(query_req(agent.clone(), "user", "city_g1c")).await
            .expect("engine2 city query must succeed");
        assert_eq!(
            qc2.belief.primary.as_ref().map(|b| &b.claim_ref),
            Some(&ref_city),
            "G1c: city claim_ref must be identical on fresh engine"
        );
        assert_eq!(
            qc2.belief.primary.as_ref().map(|b| &b.fact.value),
            Some(&serde_json::json!("Munich")),
            "G1c: city fact value must be identical on fresh engine"
        );

        // job claim must be identical.
        let qj2 = engine2.query_memory(query_req(agent.clone(), "user", "job_g1c")).await
            .expect("engine2 job query must succeed");
        assert_eq!(
            qj2.belief.primary.as_ref().map(|b| &b.claim_ref),
            Some(&ref_job),
            "G1c: job claim_ref must be identical on fresh engine"
        );
        assert_eq!(
            qj2.belief.primary.as_ref().map(|b| &b.fact.value),
            Some(&serde_json::json!("Engineer")),
            "G1c: job fact value must be identical on fresh engine"
        );
    }
}

// ── G1d: determinism with multiple independent predicates ────────────────────

/// G1d: ingest claims on DIFFERENT predicates, query each → deterministic independent results.
/// This verifies per-subject-line canonical fold is isolated and deterministic.
#[tokio::test]
async fn g1d_independent_predicates_deterministic_read() {
    let engine = mempill_sqlite::open_default_in_memory()
        .expect("in-memory engine must open");
    let agent = agent();

    // Ingest two claims on different predicates.
    let r_city = engine.ingest_claim(ingest_req(
        agent.clone(), "user", "city_g1d", "Munich",
    )).await.expect("ingest city must succeed");

    let r_job = engine.ingest_claim(ingest_req(
        agent.clone(), "user", "job_g1d", "Engineer",
    )).await.expect("ingest job must succeed");

    assert_eq!(r_city.disposition, Disposition::CommittedCheap);
    assert_eq!(r_job.disposition, Disposition::CommittedCheap);

    // Query city twice → identical.
    let city1 = engine.query_memory(query_req(agent.clone(), "user", "city_g1d")).await
        .expect("city query 1 must succeed");
    let city2 = engine.query_memory(query_req(agent.clone(), "user", "city_g1d")).await
        .expect("city query 2 must succeed");

    assert_eq!(city1.belief.status, city2.belief.status, "G1d: city belief status must be stable");
    assert_eq!(
        city1.belief.primary.as_ref().map(|b| &b.fact.value),
        city2.belief.primary.as_ref().map(|b| &b.fact.value),
        "G1d: city fact value must be identical on repeated reads"
    );
    assert_eq!(
        city1.belief.primary.as_ref().map(|b| &b.claim_ref),
        city2.belief.primary.as_ref().map(|b| &b.claim_ref),
        "G1d: city claim_ref must be identical on repeated reads"
    );

    // Query job twice → identical.
    let job1 = engine.query_memory(query_req(agent.clone(), "user", "job_g1d")).await
        .expect("job query 1 must succeed");
    let job2 = engine.query_memory(query_req(agent.clone(), "user", "job_g1d")).await
        .expect("job query 2 must succeed");

    assert_eq!(job1.belief.status, job2.belief.status, "G1d: job belief status must be stable");
    assert_eq!(
        job1.belief.primary.as_ref().map(|b| &b.fact.value),
        job2.belief.primary.as_ref().map(|b| &b.fact.value),
        "G1d: job fact value must be identical on repeated reads"
    );

    // Cross-predicate isolation: city query must NOT return the job claim.
    let city_claim_ref = city1.belief.primary.as_ref().map(|b| b.claim_ref.clone());
    let job_claim_ref = job1.belief.primary.as_ref().map(|b| b.claim_ref.clone());
    assert_ne!(
        city_claim_ref, job_claim_ref,
        "G1d: different predicate queries must return different claims (isolation)"
    );
}

// ── G1e: supersession determinism — HeavyPath belief is stable across engine instances ──

/// G1e: ingest a sequence that includes a supersession (ingest A then B on the same
/// subject-line). Capture the belief on Engine1, then open Engine2 on the same
/// file-backed DB and assert IDENTICAL belief (G1 determinism across a supersession).
///
/// DEFECT-1 FIXED: supersession::execute now receives pre-loaded edges (loaded before
/// begin_atomic), so the HeavyPath is reachable end-to-end via DefaultEngine.
/// Two separate engine instances reading the same persisted supersession state MUST
/// return identical belief — this is the G1 guarantee extended to the conflict path.
#[tokio::test]
async fn g1e_supersession_determinism_across_fresh_engine_instances() {
    let tmp = NamedTempFile::new().expect("tempfile must be created");
    let db_path = tmp.path().to_str().expect("path must be UTF-8").to_string();

    let belief_status_engine1: BeliefStatus;
    let primary_ref_engine1: Option<mempill_types::ClaimRef>;
    let alternatives_count_engine1: usize;

    // ── Engine 1: ingest A then B (supersession sequence) ────────────────────
    {
        let engine1 = open_default(&db_path).expect("file-backed engine1 must open");
        let agent = AgentId("g1e-agent".into());

        // Ingest A: cheap-path incumbent.
        let resp_a = engine1.ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "profile".into(),
            predicate: "status".into(),
            value: serde_json::json!("pending"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        }).await.expect("G1e: ingest A must succeed");

        assert_eq!(
            resp_a.disposition, Disposition::CommittedCheap,
            "G1e: ingest A must be CommittedCheap"
        );

        // Ingest B: same (subject, predicate), different value → HeavyPath/supersession.
        // DEFECT-1 FIXED: this must succeed now.
        let resp_b = engine1.ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "profile".into(),
            predicate: "status".into(),     // same predicate → SameLineConflict
            value: serde_json::json!("active"), // different value → HeavyPath fires
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        }).await.expect("G1e: ingest B (superseding) must succeed (DEFECT-1 fixed)");

        assert_ne!(
            resp_b.disposition, Disposition::CommittedCheap,
            "G1e: ingest B must not be CommittedCheap (HeavyPath fired)"
        );

        // Capture belief from engine1.
        let q1 = engine1.query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "profile".into(),
            predicate: "status".into(),
            as_of_tx_time: None,
        }).await.expect("G1e: query on engine1 must succeed");

        belief_status_engine1 = q1.belief.status.clone();
        primary_ref_engine1 = q1.belief.primary.as_ref().map(|b| b.claim_ref.clone());
        alternatives_count_engine1 = q1.belief.alternatives.len();

        // Engine1 drops here — connection closed.
    }

    // ── Engine 2: fresh instance on SAME file — belief must be IDENTICAL ──────
    {
        let engine2 = open_default(&db_path).expect("file-backed engine2 must open (same file)");
        let agent = AgentId("g1e-agent".into());

        let q2 = engine2.query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "profile".into(),
            predicate: "status".into(),
            as_of_tx_time: None,
        }).await.expect("G1e: query on engine2 must succeed");

        assert_eq!(
            q2.belief.status, belief_status_engine1,
            "G1e: belief status MUST be identical across fresh engine instances on same file \
             (supersession determinism). Engine1={:?}, Engine2={:?}",
            belief_status_engine1, q2.belief.status
        );

        let primary_ref_engine2 = q2.belief.primary.as_ref().map(|b| b.claim_ref.clone());
        assert_eq!(
            primary_ref_engine2, primary_ref_engine1,
            "G1e: primary claim_ref MUST be identical across fresh engine instances \
             (same supersession result persisted). Engine1={:?}, Engine2={:?}",
            primary_ref_engine1, primary_ref_engine2
        );

        assert_eq!(
            q2.belief.alternatives.len(), alternatives_count_engine1,
            "G1e: alternatives count MUST be identical across fresh engine instances \
             (same persisted supersession state). Engine1={}, Engine2={}",
            alternatives_count_engine1, q2.belief.alternatives.len()
        );
    }
}
