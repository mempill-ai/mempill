//! history_qa — SQLite integration tests for `query_history`.
//!
//! Scenario: CEO succession timeline Alice→John→Bob.
//!
//! Verifies:
//! 1. Three ordered entries with correct status + effective windows.
//! 2. `history.current().value == recall (query_memory) primary value`.
//! 3. Empty subject-line → empty history.

use chrono::Utc;
use mempill_core::application::{IngestClaimRequest, QueryHistoryRequest, QueryMemoryRequest};
use mempill_sqlite::open_default_in_memory;
use mempill_types::{
    AgentId, BeliefStatus, Cardinality, Confidence, Criticality, ExternalKind,
    HistoryEntryStatus, ProvenanceLabel,
};

// ── Test 1: CEO succession timeline Alice→John→Bob ───────────────────────────

#[tokio::test]
async fn ceo_succession_timeline_three_entries() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("history-qa-agent".into());

    // Ingest Alice first
    let resp_alice = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "qa-corp".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("Alice"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("alice ingest must succeed");

    println!("[history-qa] alice claim_ref = {}", resp_alice.claim_ref.0);

    // Small sleep to ensure distinct tx_times.
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let resp_john = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "qa-corp".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("John"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("john ingest must succeed");

    println!("[history-qa] john claim_ref = {}", resp_john.claim_ref.0);

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let resp_bob = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "qa-corp".into(),
            predicate: "ceo".into(),
            value: serde_json::json!("Bob"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("bob ingest must succeed");

    println!("[history-qa] bob claim_ref = {}", resp_bob.claim_ref.0);

    // Query history.
    let hist = engine
        .query_history(QueryHistoryRequest {
            agent_id: agent.clone(),
            subject: "qa-corp".into(),
            predicate: "ceo".into(),
        })
        .await
        .expect("query_history must succeed");

    println!(
        "[history-qa] history entries ({}):",
        hist.entries.len()
    );
    for e in &hist.entries {
        println!(
            "  value={:?} status={:?} valid_from={:?} valid_until={:?}",
            e.value, e.status, e.valid_from, e.valid_until
        );
    }

    // With 3 contested functional claims (no valid_time), all may be "live" (contested).
    // The engine surfaces all three as present on the subject-line.
    // We only assert: 3 entries total, ordered oldest→newest, windows computed.
    assert_eq!(
        hist.entries.len(),
        3,
        "CEO succession must have 3 history entries"
    );

    // Canonical order: oldest first (by tx_time for low-confidence claims).
    assert_eq!(
        hist.entries[0].value,
        serde_json::json!("Alice"),
        "first entry must be Alice (oldest tx)"
    );
    assert_eq!(
        hist.entries[1].value,
        serde_json::json!("John"),
        "second entry must be John"
    );
    assert_eq!(
        hist.entries[2].value,
        serde_json::json!("Bob"),
        "third entry must be Bob (newest tx)"
    );

    // Effective windows: Alice and John must have valid_until set; Bob is open-ended.
    assert!(
        hist.entries[0].valid_until.is_some(),
        "Alice's valid_until must be Some (closed by John's ordering key)"
    );
    assert!(
        hist.entries[1].valid_until.is_some(),
        "John's valid_until must be Some (closed by Bob's ordering key)"
    );
    assert!(
        hist.entries[2].valid_until.is_none(),
        "Bob's valid_until must be None (open-ended, newest)"
    );

    // Alice's valid_until <= John's valid_until (monotone windows).
    let alice_until = hist.entries[0].valid_until.unwrap();
    let john_until = hist.entries[1].valid_until.unwrap();
    assert!(
        alice_until <= john_until,
        "Alice's valid_until must be <= John's valid_until (monotone)"
    );

    println!("[history-qa] TEST 1 PASS: 3 ordered entries, correct effective windows");
}

// ── Test 2: history.current().value == query_memory primary value ─────────────

#[tokio::test]
async fn history_current_agrees_with_query_memory() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("history-qa-agree-agent".into());

    // Single uncontested claim so there's a clear primary.
    let resp = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "qa-org".into(),
            predicate: "leader".into(),
            value: serde_json::json!("Solo-Leader"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("ingest must succeed");

    println!("[history-qa] solo leader claim_ref = {}", resp.claim_ref.0);

    // query_memory primary value.
    let qm = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "qa-org".into(),
            predicate: "leader".into(),
            as_of_tx_time: None,
        })
        .await
        .expect("query_memory must succeed");

    assert!(
        matches!(
            qm.belief.status,
            BeliefStatus::Resolved | BeliefStatus::TimingUncertain
        ),
        "solo claim must be Resolved or TimingUncertain, got {:?}",
        qm.belief.status
    );
    let primary_value = qm
        .belief
        .primary
        .as_ref()
        .expect("primary must be Some")
        .fact
        .value
        .clone();

    // query_history current value.
    let hist = engine
        .query_history(QueryHistoryRequest {
            agent_id: agent.clone(),
            subject: "qa-org".into(),
            predicate: "leader".into(),
        })
        .await
        .expect("query_history must succeed");

    let current = hist.current().expect("current() must be Some for a live claim");

    assert_eq!(
        current.value, primary_value,
        "history current value must equal query_memory primary value"
    );
    assert_eq!(
        current.status,
        HistoryEntryStatus::Current,
        "current() entry must have status Current"
    );

    println!(
        "[history-qa] TEST 2 PASS: history.current().value = {:?} == recall primary {:?}",
        current.value, primary_value
    );
}

// ── Test 3: empty subject-line → empty history ────────────────────────────────

#[tokio::test]
async fn empty_subject_line_returns_empty_history() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("history-qa-empty-agent".into());

    let hist = engine
        .query_history(QueryHistoryRequest {
            agent_id: agent.clone(),
            subject: "nonexistent-subject".into(),
            predicate: "nonexistent-predicate".into(),
        })
        .await
        .expect("query_history on empty line must succeed");

    assert!(
        hist.entries.is_empty(),
        "empty subject-line must return empty entries"
    );
    assert!(
        hist.current().is_none(),
        "current() on empty history must be None"
    );

    println!("[history-qa] TEST 3 PASS: empty subject-line → empty history");
}

// ── Test 4: recall / query_memory unchanged ───────────────────────────────────

/// Regression guard: recall (query_memory) behavior is unchanged by this Wave.
#[tokio::test]
async fn query_memory_unchanged_by_history_wave() {
    let engine = open_default_in_memory().expect("in-memory engine must open");
    let agent = AgentId("history-qa-regression-agent".into());

    // Ingest a single claim.
    let _ = engine
        .ingest_claim(IngestClaimRequest {
            agent_id: agent.clone(),
            subject: "regression".into(),
            predicate: "check".into(),
            value: serde_json::json!("value-v1"),
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            cardinality: Cardinality::Functional,
            valid_time: None,
            confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            criticality: Criticality::Medium,
            derived_from: vec![],
        })
        .await
        .expect("ingest must succeed");

    // query_memory must still return correctly.
    let qm = engine
        .query_memory(QueryMemoryRequest {
            agent_id: agent.clone(),
            subject: "regression".into(),
            predicate: "check".into(),
            as_of_tx_time: None,
        })
        .await
        .expect("query_memory must succeed");

    assert!(
        matches!(
            qm.belief.status,
            BeliefStatus::Resolved | BeliefStatus::TimingUncertain
        ),
        "query_memory must still return Resolved/TimingUncertain after history Wave, got {:?}",
        qm.belief.status
    );
    assert!(qm.belief.primary.is_some(), "primary must be Some");
    assert_eq!(
        qm.belief.primary.unwrap().fact.value,
        serde_json::json!("value-v1"),
        "primary value must match the ingested value"
    );

    println!("[history-qa] TEST 4 PASS: query_memory unchanged by history wave");
}
