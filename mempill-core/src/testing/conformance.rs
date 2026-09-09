//! Shared persistence conformance harness.
//!
//! `run_persistence_conformance` exercises every `PersistencePort` method
//! and panics on any deviation from the expected contract.
//!
//! `run_history_conformance` exercises the history timeline logic against a real store.
//!
//! Both `mempill-sqlite` and `mempill-postgres` activate `mempill-core/test-support`
//! in dev-dependencies and call both functions to verify behavioral parity.
//!
//! Each sub-test uses DISTINCT agent_ids so they do not interfere on a shared store.

#[cfg(any(test, feature = "test-support"))]
use chrono::Utc;
#[cfg(any(test, feature = "test-support"))]
use uuid::Uuid;

#[cfg(any(test, feature = "test-support"))]
use mempill_types::{
    claim::{Cardinality, Claim, Confidence, Criticality, Fact},
    disposition::Disposition,
    edge::{ClaimEdge, EdgeKind},
    identity::{AgentId, ClaimRef},
    ledger::{LedgerEntry, LedgerEventKind},
    provenance::{ExternalAnchor, ExternalKind, ProvenanceLabel},
    time::{TransactionTime, ValidTime},
    validity::{AssertionKind, ValidityAssertion},
};

#[cfg(any(test, feature = "test-support"))]
use crate::ports::persistence::PersistencePort;

// ── Builder helpers ───────────────────────────────────────────────────────────

#[cfg(any(test, feature = "test-support"))]
fn make_claim(agent_id: &AgentId, subject: &str, predicate: &str) -> Claim {
    Claim::new(
        ClaimRef::new_random(),
        agent_id.clone(),
        Fact {
            subject: subject.to_owned(),
            predicate: predicate.to_owned(),
            value: serde_json::json!("test-value"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now()),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    )
}

#[cfg(any(test, feature = "test-support"))]
fn make_ledger_entry(agent_id: &AgentId, claim_ref: &ClaimRef) -> LedgerEntry {
    LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent_id.clone(),
        claim_ref: claim_ref.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(Utc::now()),
    }
}

#[cfg(any(test, feature = "test-support"))]
fn make_validity_assertion(agent_id: &AgentId, claim_ref: &ClaimRef) -> ValidityAssertion {
    ValidityAssertion {
        assertion_ref: Uuid::new_v4(),
        agent_id: agent_id.clone(),
        target_claim: claim_ref.clone(),
        kind: AssertionKind::Bound { bound_at: Utc::now() },
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        asserted_at: TransactionTime(Utc::now()),
    }
}

#[cfg(any(test, feature = "test-support"))]
fn make_edge(agent_id: &AgentId, from: ClaimRef, to: ClaimRef, kind: EdgeKind) -> ClaimEdge {
    ClaimEdge {
        edge_id: Uuid::new_v4(),
        agent_id: agent_id.clone(),
        from_claim: from,
        to_claim: to,
        kind,
        created_at: TransactionTime(Utc::now()),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the full persistence conformance suite against `store`.
///
/// Each sub-test uses a distinct `AgentId` to avoid cross-contamination on a shared store.
/// Panics on any contract violation with a descriptive message.
#[cfg(any(test, feature = "test-support"))]
pub fn run_persistence_conformance<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    test_begin_commit_roundtrip(store);
    test_append_all_four_tables(store);
    test_rollback_leaves_zero_rows(store);
    test_load_subject_line_ordering(store);
    test_load_lineage_multi_hop(store);
    test_load_edges_for_ordering(store);
    test_edge_uniqueness_constraint(store);
    test_load_validity_assertions_ordering(store);
    test_load_ledger_with_from(store);
    test_load_injected_claims(store);
    test_load_claim_missing(store);
    test_load_edges_for_empty(store);
    test_load_ledger_for_claims_scoped(store);
}

/// Run the disposition-scope correctness suite.
///
/// Proves that `load_ledger_for_claims` returns complete dispositions for the
/// queried claims, and that `query_memory` returns the correct live belief on a
/// subject-line whose superseded claim has a disposition event that would fall
/// outside a small agent-wide cap — the silent-wrong-belief-at-scale bug.
#[cfg(any(test, feature = "test-support"))]
pub fn run_disposition_scope_conformance<P>(store: &std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    test_superseded_claim_excluded_despite_large_agent_ledger(store.as_ref());
    test_write_audit_paths_correct_despite_large_agent_ledger(std::sync::Arc::clone(store));
}

// ── Sub-tests ─────────────────────────────────────────────────────────────────

/// begin_atomic → append_claim → commit → load_claim returns Some with fields intact.
#[cfg(any(test, feature = "test-support"))]
fn test_begin_commit_roundtrip<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t1".into());
    let claim = make_claim(&agent, "user", "favourite_colour");
    let claim_ref = claim.claim_ref().clone();

    let mut txn = store
        .begin_atomic(&agent)
        .expect("conformance[t1]: begin_atomic must succeed");
    store
        .append_claim(&mut txn, &claim)
        .expect("conformance[t1]: append_claim must succeed");
    store.commit(txn).expect("conformance[t1]: commit must succeed");

    let loaded = store
        .load_claim(&agent, &claim_ref)
        .expect("conformance[t1]: load_claim must not error");
    let loaded = loaded.expect("conformance[t1]: load_claim must return Some after commit");

    assert_eq!(
        loaded.claim_ref(),
        &claim_ref,
        "conformance[t1]: claim_ref must round-trip"
    );
    assert_eq!(
        loaded.fact().subject,
        "user",
        "conformance[t1]: subject must be preserved"
    );
    assert_eq!(
        loaded.fact().predicate,
        "favourite_colour",
        "conformance[t1]: predicate must be preserved"
    );
}

/// append claim + validity + ledger + edge in ONE txn, commit, read each back.
#[cfg(any(test, feature = "test-support"))]
fn test_append_all_four_tables<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t2".into());
    let claim = make_claim(&agent, "user", "language");
    let claim_ref = claim.claim_ref().clone();
    let validity = make_validity_assertion(&agent, &claim_ref);
    let ledger = make_ledger_entry(&agent, &claim_ref);
    let claim2 = make_claim(&agent, "user", "location");
    let claim2_ref = claim2.claim_ref().clone();
    let edge = make_edge(&agent, claim_ref.clone(), claim2_ref.clone(), EdgeKind::DependsOn);

    let mut txn = store
        .begin_atomic(&agent)
        .expect("conformance[t2]: begin_atomic must succeed");

    // Must insert claim2 before the edge (FK constraint)
    store
        .append_claim(&mut txn, &claim)
        .expect("conformance[t2]: append_claim must succeed");
    store
        .append_claim(&mut txn, &claim2)
        .expect("conformance[t2]: append_claim2 must succeed");
    store
        .append_validity_assertion(&mut txn, &validity)
        .expect("conformance[t2]: append_validity_assertion must succeed");
    store
        .append_ledger_entry(&mut txn, &ledger)
        .expect("conformance[t2]: append_ledger_entry must succeed");
    store
        .append_claim_edge(&mut txn, &edge)
        .expect("conformance[t2]: append_claim_edge must succeed");

    store.commit(txn).expect("conformance[t2]: commit must succeed");

    // Read back claim
    let loaded_claim = store
        .load_claim(&agent, &claim_ref)
        .expect("conformance[t2]: load_claim must not error")
        .expect("conformance[t2]: load_claim must return Some");
    assert_eq!(loaded_claim.claim_ref(), &claim_ref, "conformance[t2]: claim_ref must match");

    // Read back validity assertions
    let assertions = store
        .load_validity_assertions_for(&agent, &claim_ref)
        .expect("conformance[t2]: load_validity_assertions_for must not error");
    assert_eq!(
        assertions.len(),
        1,
        "conformance[t2]: must have 1 validity assertion"
    );
    assert_eq!(
        assertions[0].assertion_ref, validity.assertion_ref,
        "conformance[t2]: assertion_ref must match"
    );

    // Read back ledger entries
    let entries = store
        .load_ledger(&agent, None, 100)
        .expect("conformance[t2]: load_ledger must not error");
    assert_eq!(entries.len(), 1, "conformance[t2]: must have 1 ledger entry");
    assert_eq!(
        entries[0].entry_id, ledger.entry_id,
        "conformance[t2]: entry_id must match"
    );

    // Read back edges
    let edges = store
        .load_edges_for(&agent, &claim_ref)
        .expect("conformance[t2]: load_edges_for must not error");
    assert_eq!(edges.len(), 1, "conformance[t2]: must have 1 edge");
    assert_eq!(edges[0].edge_id, edge.edge_id, "conformance[t2]: edge_id must match");
}

/// rollback leaves ZERO rows across all 4 tables (atomicity guarantee).
#[cfg(any(test, feature = "test-support"))]
fn test_rollback_leaves_zero_rows<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t3".into());
    let claim = make_claim(&agent, "subject-rb", "predicate-rb");
    let claim_ref = claim.claim_ref().clone();
    let validity = make_validity_assertion(&agent, &claim_ref);
    let ledger = make_ledger_entry(&agent, &claim_ref);

    let mut txn = store
        .begin_atomic(&agent)
        .expect("conformance[t3]: begin_atomic must succeed");
    store
        .append_claim(&mut txn, &claim)
        .expect("conformance[t3]: append_claim must succeed");
    store
        .append_validity_assertion(&mut txn, &validity)
        .expect("conformance[t3]: append_validity_assertion must succeed");
    store
        .append_ledger_entry(&mut txn, &ledger)
        .expect("conformance[t3]: append_ledger_entry must succeed");

    store.rollback(txn).expect("conformance[t3]: rollback must succeed");

    // All reads must return empty after rollback
    let loaded_claim = store
        .load_claim(&agent, &claim_ref)
        .expect("conformance[t3]: load_claim must not error after rollback");
    assert!(
        loaded_claim.is_none(),
        "conformance[t3]: claim must be absent after rollback"
    );

    let assertions = store
        .load_validity_assertions_for(&agent, &claim_ref)
        .expect("conformance[t3]: load_validity_assertions_for must not error");
    assert!(
        assertions.is_empty(),
        "conformance[t3]: validity assertions must be absent after rollback"
    );

    let ledger_entries = store
        .load_ledger(&agent, None, 100)
        .expect("conformance[t3]: load_ledger must not error");
    assert!(
        ledger_entries.is_empty(),
        "conformance[t3]: ledger entries must be absent after rollback"
    );

    let edges = store
        .load_edges_for(&agent, &claim_ref)
        .expect("conformance[t3]: load_edges_for must not error");
    assert!(
        edges.is_empty(),
        "conformance[t3]: edges must be absent after rollback"
    );
}

/// load_subject_line ORDER BY tx_time ASC (≥2 claims).
#[cfg(any(test, feature = "test-support"))]
fn test_load_subject_line_ordering<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t4".into());

    // Create two claims with distinct tx_times (use slightly different times via sleep-free approach:
    // we can't sleep, but we can use slightly different timestamps by constructing them explicitly)
    let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp(1_000_000, 0).unwrap();
    let t2 = chrono::DateTime::<chrono::Utc>::from_timestamp(1_000_001, 0).unwrap();

    let claim1 = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "user".into(), predicate: "job".into(), value: serde_json::json!("engineer") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(t1),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );
    let claim2 = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "user".into(), predicate: "job".into(), value: serde_json::json!("architect") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(t2),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );

    let ref1 = claim1.claim_ref().clone();
    let ref2 = claim2.claim_ref().clone();

    let mut txn = store
        .begin_atomic(&agent)
        .expect("conformance[t4]: begin_atomic must succeed");
    store.append_claim(&mut txn, &claim1).expect("conformance[t4]: append claim1");
    store.append_claim(&mut txn, &claim2).expect("conformance[t4]: append claim2");
    store.commit(txn).expect("conformance[t4]: commit");

    let line = store
        .load_subject_line(&agent, "user", "job", None)
        .expect("conformance[t4]: load_subject_line must not error");

    assert_eq!(line.len(), 2, "conformance[t4]: must have 2 claims on subject line");
    assert_eq!(
        line[0].claim_ref(),
        &ref1,
        "conformance[t4]: first claim must have earliest tx_time (ASC order)"
    );
    assert_eq!(
        line[1].claim_ref(),
        &ref2,
        "conformance[t4]: second claim must have latest tx_time"
    );
}

/// load_lineage multi-hop (A→B→C DerivedFrom/DependsOn chain) returns the chain.
#[cfg(any(test, feature = "test-support"))]
fn test_load_lineage_multi_hop<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t5".into());

    let claim_a = make_claim(&agent, "topic", "summary");
    let claim_b = make_claim(&agent, "topic", "detail");
    let claim_c = make_claim(&agent, "topic", "inference");

    let ref_a = claim_a.claim_ref().clone();
    let ref_b = claim_b.claim_ref().clone();
    let ref_c = claim_c.claim_ref().clone();

    // Chain: A --DerivedFrom--> B --DerivedFrom--> C
    let t_base = chrono::DateTime::<chrono::Utc>::from_timestamp(2_000_000, 0).unwrap();
    let edge_ab = ClaimEdge {
        edge_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        from_claim: ref_a.clone(),
        to_claim: ref_b.clone(),
        kind: EdgeKind::DerivedFrom,
        created_at: TransactionTime(t_base),
    };
    let edge_bc = ClaimEdge {
        edge_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        from_claim: ref_b.clone(),
        to_claim: ref_c.clone(),
        kind: EdgeKind::DerivedFrom,
        created_at: TransactionTime(t_base + chrono::Duration::seconds(1)),
    };

    let mut txn = store.begin_atomic(&agent).expect("conformance[t5]: begin_atomic");
    store.append_claim(&mut txn, &claim_a).expect("conformance[t5]: append A");
    store.append_claim(&mut txn, &claim_b).expect("conformance[t5]: append B");
    store.append_claim(&mut txn, &claim_c).expect("conformance[t5]: append C");
    store.append_claim_edge(&mut txn, &edge_ab).expect("conformance[t5]: append edge A→B");
    store.append_claim_edge(&mut txn, &edge_bc).expect("conformance[t5]: append edge B→C");
    store.commit(txn).expect("conformance[t5]: commit");

    // load_lineage starting from A should return [A→B, B→C] ordered by depth ASC
    let lineage = store
        .load_lineage(&agent, &ref_a)
        .expect("conformance[t5]: load_lineage must not error");

    assert_eq!(
        lineage.len(),
        2,
        "conformance[t5]: lineage from A must have 2 edges (A→B at depth 1, B→C at depth 2)"
    );
    assert_eq!(
        lineage[0].from_claim, ref_a,
        "conformance[t5]: first edge must start from A (depth 1)"
    );
    assert_eq!(
        lineage[0].to_claim, ref_b,
        "conformance[t5]: first edge must point to B"
    );
    assert_eq!(
        lineage[1].from_claim, ref_b,
        "conformance[t5]: second edge must start from B (depth 2)"
    );
    assert_eq!(
        lineage[1].to_claim, ref_c,
        "conformance[t5]: second edge must point to C"
    );
}

/// load_edges_for ORDER BY created_at ASC.
#[cfg(any(test, feature = "test-support"))]
fn test_load_edges_for_ordering<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t6".into());

    let claim_hub = make_claim(&agent, "hub", "central");
    let claim_x = make_claim(&agent, "spoke", "x");
    let claim_y = make_claim(&agent, "spoke", "y");

    let hub_ref = claim_hub.claim_ref().clone();
    let x_ref = claim_x.claim_ref().clone();
    let y_ref = claim_y.claim_ref().clone();

    let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp(3_000_000, 0).unwrap();
    let t2 = chrono::DateTime::<chrono::Utc>::from_timestamp(3_000_001, 0).unwrap();

    // edge1: hub→x (created earlier)
    let edge1 = ClaimEdge {
        edge_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        from_claim: hub_ref.clone(),
        to_claim: x_ref.clone(),
        kind: EdgeKind::DependsOn,
        created_at: TransactionTime(t1),
    };
    // edge2: hub→y (created later)
    let edge2 = ClaimEdge {
        edge_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        from_claim: hub_ref.clone(),
        to_claim: y_ref.clone(),
        kind: EdgeKind::DependsOn,
        created_at: TransactionTime(t2),
    };

    let mut txn = store.begin_atomic(&agent).expect("conformance[t6]: begin_atomic");
    store.append_claim(&mut txn, &claim_hub).expect("conformance[t6]: append hub");
    store.append_claim(&mut txn, &claim_x).expect("conformance[t6]: append x");
    store.append_claim(&mut txn, &claim_y).expect("conformance[t6]: append y");
    store.append_claim_edge(&mut txn, &edge1).expect("conformance[t6]: append edge1");
    store.append_claim_edge(&mut txn, &edge2).expect("conformance[t6]: append edge2");
    store.commit(txn).expect("conformance[t6]: commit");

    let edges = store
        .load_edges_for(&agent, &hub_ref)
        .expect("conformance[t6]: load_edges_for must not error");

    assert_eq!(edges.len(), 2, "conformance[t6]: hub must have 2 edges");
    assert_eq!(
        edges[0].to_claim, x_ref,
        "conformance[t6]: first edge (ASC created_at) must point to x"
    );
    assert_eq!(
        edges[1].to_claim, y_ref,
        "conformance[t6]: second edge must point to y"
    );
}

/// edge uniqueness: duplicate (agent_id, from, to, kind) → Err.
#[cfg(any(test, feature = "test-support"))]
fn test_edge_uniqueness_constraint<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t7".into());

    let claim_a = make_claim(&agent, "dup-from", "p");
    let claim_b = make_claim(&agent, "dup-to", "p");

    let ref_a = claim_a.claim_ref().clone();
    let ref_b = claim_b.claim_ref().clone();

    // Insert both claims and the first edge
    let edge1 = ClaimEdge {
        edge_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        from_claim: ref_a.clone(),
        to_claim: ref_b.clone(),
        kind: EdgeKind::DependsOn,
        created_at: TransactionTime(Utc::now()),
    };

    let mut txn = store.begin_atomic(&agent).expect("conformance[t7]: begin_atomic");
    store.append_claim(&mut txn, &claim_a).expect("conformance[t7]: append A");
    store.append_claim(&mut txn, &claim_b).expect("conformance[t7]: append B");
    store.append_claim_edge(&mut txn, &edge1).expect("conformance[t7]: append first edge");
    store.commit(txn).expect("conformance[t7]: first commit");

    // Now attempt to insert a duplicate edge in a new transaction
    let edge_dup = ClaimEdge {
        edge_id: Uuid::new_v4(), // different edge_id, same (agent, from, to, kind)
        agent_id: agent.clone(),
        from_claim: ref_a.clone(),
        to_claim: ref_b.clone(),
        kind: EdgeKind::DependsOn,
        created_at: TransactionTime(Utc::now()),
    };

    let mut txn2 = store.begin_atomic(&agent).expect("conformance[t7]: begin_atomic txn2");
    let result = store.append_claim_edge(&mut txn2, &edge_dup);
    // Must error due to UNIQUE(agent_id, from_claim_id, to_claim_id, edge_kind)
    // Roll back regardless
    let _ = store.rollback(txn2);

    assert!(
        result.is_err(),
        "conformance[t7]: duplicate edge insert must return Err (UNIQUE constraint)"
    );
}

/// load_validity_assertions_for ORDER BY asserted_at ASC.
#[cfg(any(test, feature = "test-support"))]
fn test_load_validity_assertions_ordering<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t8".into());
    let claim = make_claim(&agent, "food", "allergy");
    let claim_ref = claim.claim_ref().clone();

    let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp(4_000_000, 0).unwrap();
    let t2 = chrono::DateTime::<chrono::Utc>::from_timestamp(4_000_001, 0).unwrap();

    let va1 = ValidityAssertion {
        assertion_ref: Uuid::new_v4(),
        agent_id: agent.clone(),
        target_claim: claim_ref.clone(),
        kind: AssertionKind::Bound { bound_at: t1 },
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        asserted_at: TransactionTime(t1),
    };
    let va2 = ValidityAssertion {
        assertion_ref: Uuid::new_v4(),
        agent_id: agent.clone(),
        target_claim: claim_ref.clone(),
        kind: AssertionKind::Reopen { reopen_at: t2 },
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        confidence: Confidence { value_confidence: 0.8, valid_time_confidence: 0.8 },
        asserted_at: TransactionTime(t2),
    };

    let ref1 = va1.assertion_ref;
    let ref2 = va2.assertion_ref;

    let mut txn = store.begin_atomic(&agent).expect("conformance[t8]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[t8]: append claim");
    // Insert in reverse order to prove ORDER BY overrides insertion order
    store.append_validity_assertion(&mut txn, &va2).expect("conformance[t8]: append va2 first");
    store.append_validity_assertion(&mut txn, &va1).expect("conformance[t8]: append va1 second");
    store.commit(txn).expect("conformance[t8]: commit");

    let assertions = store
        .load_validity_assertions_for(&agent, &claim_ref)
        .expect("conformance[t8]: load_validity_assertions_for must not error");

    assert_eq!(assertions.len(), 2, "conformance[t8]: must have 2 assertions");
    assert_eq!(
        assertions[0].assertion_ref, ref1,
        "conformance[t8]: first assertion must be the earliest asserted_at (ASC)"
    );
    assert_eq!(
        assertions[1].assertion_ref, ref2,
        "conformance[t8]: second assertion must be the later asserted_at"
    );
}

/// load_ledger with a `from` bound returns only entries >= bound.
#[cfg(any(test, feature = "test-support"))]
fn test_load_ledger_with_from<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t9".into());

    let t_early = chrono::DateTime::<chrono::Utc>::from_timestamp(5_000_000, 0).unwrap();
    let t_late = chrono::DateTime::<chrono::Utc>::from_timestamp(5_000_002, 0).unwrap();

    let claim_early = make_claim(&agent, "ledger-sub", "early");
    let ref_early = claim_early.claim_ref().clone();
    let claim_late = make_claim(&agent, "ledger-sub", "late");
    let ref_late = claim_late.claim_ref().clone();

    let entry_early = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref_early.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(t_early),
    };
    let entry_late = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref_late.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(t_late),
    };

    let late_id = entry_late.entry_id;

    let mut txn = store.begin_atomic(&agent).expect("conformance[t9]: begin_atomic");
    store.append_claim(&mut txn, &claim_early).expect("conformance[t9]: append claim_early");
    store.append_claim(&mut txn, &claim_late).expect("conformance[t9]: append claim_late");
    store.append_ledger_entry(&mut txn, &entry_early).expect("conformance[t9]: append early entry");
    store.append_ledger_entry(&mut txn, &entry_late).expect("conformance[t9]: append late entry");
    store.commit(txn).expect("conformance[t9]: commit");

    // Query with from = t_late; should return only the late entry
    let from_time = TransactionTime(t_late);
    let entries = store
        .load_ledger(&agent, Some(&from_time), 100)
        .expect("conformance[t9]: load_ledger with from must not error");

    assert_eq!(
        entries.len(),
        1,
        "conformance[t9]: load_ledger with from=t_late must return 1 entry (not the earlier one)"
    );
    assert_eq!(
        entries[0].entry_id, late_id,
        "conformance[t9]: the returned entry must be the late one"
    );
}

/// load_injected_claims returns ServedAsInjected-origin claims.
#[cfg(any(test, feature = "test-support"))]
fn test_load_injected_claims<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t10".into());

    let claim1 = make_claim(&agent, "injected-sub", "p1");
    let ref1 = claim1.claim_ref().clone();
    let claim2 = make_claim(&agent, "injected-sub", "p2");
    let ref2 = claim2.claim_ref().clone();

    // claim1 gets a ServedAsInjected entry; claim2 gets ClaimCommitted (not injected)
    let entry_injected = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref1.clone(),
        event_kind: LedgerEventKind::ServedAsInjected,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(Utc::now()),
    };
    let entry_committed = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref2.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(Utc::now()),
    };

    let mut txn = store.begin_atomic(&agent).expect("conformance[t10]: begin_atomic");
    store.append_claim(&mut txn, &claim1).expect("conformance[t10]: append claim1");
    store.append_claim(&mut txn, &claim2).expect("conformance[t10]: append claim2");
    store.append_ledger_entry(&mut txn, &entry_injected).expect("conformance[t10]: append injected entry");
    store.append_ledger_entry(&mut txn, &entry_committed).expect("conformance[t10]: append committed entry");
    store.commit(txn).expect("conformance[t10]: commit");

    let injected = store
        .load_injected_claims(&agent)
        .expect("conformance[t10]: load_injected_claims must not error");

    assert_eq!(
        injected.len(),
        1,
        "conformance[t10]: must return exactly 1 injected claim (ServedAsInjected only)"
    );
    assert_eq!(
        injected[0], ref1,
        "conformance[t10]: injected claim ref must be ref1"
    );
}

/// load_claim missing → None.
#[cfg(any(test, feature = "test-support"))]
fn test_load_claim_missing<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t11".into());
    let nonexistent_ref = ClaimRef::new_random();

    let result = store
        .load_claim(&agent, &nonexistent_ref)
        .expect("conformance[t11]: load_claim for missing ref must not error");

    assert!(
        result.is_none(),
        "conformance[t11]: load_claim for nonexistent ClaimRef must return None"
    );
}

/// load_edges_for with no edges → empty vec.
#[cfg(any(test, feature = "test-support"))]
fn test_load_edges_for_empty<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-t12".into());
    let claim = make_claim(&agent, "isolated-sub", "p");
    let claim_ref = claim.claim_ref().clone();

    let mut txn = store.begin_atomic(&agent).expect("conformance[t12]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[t12]: append claim");
    store.commit(txn).expect("conformance[t12]: commit");

    let edges = store
        .load_edges_for(&agent, &claim_ref)
        .expect("conformance[t12]: load_edges_for must not error");

    assert!(
        edges.is_empty(),
        "conformance[t12]: load_edges_for must return empty vec when no edges exist"
    );
}

// ── History conformance harness ───────────────────────────────────────────────

/// Run the history timeline conformance suite against `store`.
///
/// Exercises `truth_engine::fold` + `truth_engine::compute_history_windows` (the engine-layer
/// single source of truth) via the real persistence backend. Uses DISTINCT agent_id namespace
/// (`conformance-hist-*`).
///
/// Panics on any contract violation with a descriptive message.
#[cfg(any(test, feature = "test-support"))]
pub fn run_history_conformance<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    hist_empty_line(store);
    hist_single_claim(store);
    hist_succession_ordering(store);
    hist_current_agrees_with_fold(store);
    hist_own_end_never_discarded_when_no_overlap(store);
    hist_overlap_marks_both_entries_contested_no_narrowing(store);
    hist_current_requires_window_contains_now(store);
    hist_ended_status_for_expired_unsuperseded_claim(store);
    hist_duplicate_ordering_key_no_zero_length_window(store);
    hist_granularity_follows_winning_endpoint_source(store);
}

/// Empty subject-line → `load_subject_line` returns empty (foundation for history).
#[cfg(any(test, feature = "test-support"))]
fn hist_empty_line<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-hist-t1".into());
    let claims = store
        .load_subject_line(&agent, "hist-nobody", "hist-nothing", None)
        .expect("conformance[hist-t1]: load_subject_line must not error");
    assert!(
        claims.is_empty(),
        "conformance[hist-t1]: unknown subject-line must return empty vec"
    );
}

/// Single committed claim → 1 entry, ordering key is tx_time (low confidence).
#[cfg(any(test, feature = "test-support"))]
fn hist_single_claim<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("conformance-hist-t2".into());
    let tx = chrono::DateTime::<chrono::Utc>::from_timestamp(10_000_000, 0).unwrap();
    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "hist-acme".to_owned(), predicate: "ceo".to_owned(), value: serde_json::json!("Alice") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t2]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[hist-t2]: append_claim");
    store.commit(txn).expect("conformance[hist-t2]: commit");

    let claims = store
        .load_subject_line(&agent, "hist-acme", "ceo", None)
        .expect("conformance[hist-t2]: load_subject_line must not error");
    assert_eq!(claims.len(), 1, "conformance[hist-t2]: must have 1 claim");

    let config = EngineConfig::default();
    let now = tx + chrono::Duration::seconds(1);
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);
    assert_eq!(windows.len(), 1, "conformance[hist-t2]: 1 window");
    assert_eq!(windows[0].valid_until, None, "conformance[hist-t2]: single claim has open-ended window");
}

/// CEO succession: Alice→John→Bob — 3 entries ordered oldest first, windows correct.
/// This is the canonical CEO-timeline scenario from the DESIGN.md.
#[cfg(any(test, feature = "test-support"))]
fn hist_succession_ordering<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("conformance-hist-t3".into());

    let t_alice = chrono::DateTime::<chrono::Utc>::from_timestamp(11_000_000, 0).unwrap();
    let t_john  = chrono::DateTime::<chrono::Utc>::from_timestamp(11_000_001, 0).unwrap();
    let t_bob   = chrono::DateTime::<chrono::Utc>::from_timestamp(11_000_002, 0).unwrap();

    let make_c = |val: &str, tx: chrono::DateTime<chrono::Utc>| -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "hist-corp".to_owned(), predicate: "ceo".to_owned(), value: serde_json::json!(val) },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        )
    };

    let c_alice = make_c("Alice", t_alice);
    let c_john  = make_c("John",  t_john);
    let c_bob   = make_c("Bob",   t_bob);

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t3]: begin_atomic");
    store.append_claim(&mut txn, &c_alice).expect("conformance[hist-t3]: append Alice");
    store.append_claim(&mut txn, &c_john).expect("conformance[hist-t3]: append John");
    store.append_claim(&mut txn, &c_bob).expect("conformance[hist-t3]: append Bob");
    store.commit(txn).expect("conformance[hist-t3]: commit");

    let claims = store
        .load_subject_line(&agent, "hist-corp", "ceo", None)
        .expect("conformance[hist-t3]: load_subject_line must not error");
    assert_eq!(claims.len(), 3, "conformance[hist-t3]: must have 3 claims (Alice, John, Bob)");

    let config = EngineConfig::default();
    let now = t_bob + chrono::Duration::seconds(1);
    // fold() performs the canonical sort internally (I8) — `all_claims` is already ordered.
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    assert_eq!(
        fold.all_claims.iter().map(|cs| cs.claim.fact().value.clone()).collect::<Vec<_>>(),
        vec![serde_json::json!("Alice"), serde_json::json!("John"), serde_json::json!("Bob")],
        "conformance[hist-t3]: fold.all_claims must already be in canonical (oldest-first) order"
    );
    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

    // Windows: Alice closed by John's tx, John closed by Bob's tx, Bob open (no valid_time on
    // any claim → legacy successor-ordering-key fallback, unchanged from the pre-fix behavior).
    assert_eq!(windows[0].valid_until, Some(t_john), "conformance[hist-t3]: Alice's valid_until = John's ordering key");
    assert_eq!(windows[1].valid_until, Some(t_bob),  "conformance[hist-t3]: John's valid_until = Bob's ordering key");
    assert_eq!(windows[2].valid_until, None,          "conformance[hist-t3]: Bob is open-ended (current)");

    // Values in canonical order (oldest first) — already asserted via fold.all_claims above.
}

/// Current entry agrees with `truth_engine::fold` on which claim is live.
#[cfg(any(test, feature = "test-support"))]
fn hist_current_agrees_with_fold<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use std::collections::HashMap;
    use mempill_types::disposition::Disposition;

    let agent = AgentId("conformance-hist-t4".into());

    let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp(12_000_000, 0).unwrap();
    let t2 = chrono::DateTime::<chrono::Utc>::from_timestamp(12_000_001, 0).unwrap();

    let c = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "hist-org".to_owned(), predicate: "lead".to_owned(), value: serde_json::json!("Leader-A") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(t1),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t4]: begin_atomic");
    store.append_claim(&mut txn, &c).expect("conformance[hist-t4]: append");
    store.commit(txn).expect("conformance[hist-t4]: commit");

    let claims = store
        .load_subject_line(&agent, "hist-org", "lead", None)
        .expect("conformance[hist-t4]: load_subject_line");
    assert_eq!(claims.len(), 1, "conformance[hist-t4]: one claim loaded");

    let config = EngineConfig::default();
    let latest_disposition: HashMap<_, Disposition> = HashMap::new();
    let now = t2;

    let fold = truth_engine::fold(
        claims.clone(),
        |_| vec![],
        now,
        None, // valid_at_instant: None = backward-compatible (use as_of_tx_time for selection)
        &config,
        &latest_disposition,
    );

    assert_eq!(fold.live_claims.len(), 1, "conformance[hist-t4]: one live claim in fold");
    assert_eq!(
        fold.live_claims[0].claim.fact().value,
        serde_json::json!("Leader-A"),
        "conformance[hist-t4]: fold's live claim must match the single committed claim"
    );
}

/// Helper: build a trusted valid-time claim for the history window-narrowing tests.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::too_many_arguments)]
fn hist_vt_claim(
    agent: &AgentId,
    subject: &str,
    predicate: &str,
    value: &str,
    tx: chrono::DateTime<chrono::Utc>,
    start: chrono::DateTime<chrono::Utc>,
    end: Option<chrono::DateTime<chrono::Utc>>,
) -> Claim {
    Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: subject.to_owned(), predicate: predicate.to_owned(), value: serde_json::json!(value) },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime { start: Some(start), end, valid_time_confidence: 0.9, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        Criticality::Low,
        vec![],
        None,
        None,
    )
}

/// Bug A regression: a claim's own `valid_time.end` must never be discarded in favor of a
/// LATER successor ordering key when it is genuinely non-overlapping and earlier.
#[cfg(any(test, feature = "test-support"))]
fn hist_own_end_never_discarded_when_no_overlap<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use chrono::TimeZone;

    let agent = AgentId("conformance-hist-t5".into());
    let dt = |y, m, d| chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();

    // Linda [Jan 1 - Jan 10) — genuine gap before John starts (Jan 24), non-overlapping.
    let linda = hist_vt_claim(&agent, "hist-a-corp", "ceo", "Linda", dt(2024, 1, 1), dt(2024, 1, 1), Some(dt(2024, 1, 10)));
    let john = hist_vt_claim(&agent, "hist-a-corp", "ceo", "John", dt(2024, 1, 2), dt(2024, 1, 24), None);

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t5]: begin_atomic");
    store.append_claim(&mut txn, &linda).expect("conformance[hist-t5]: append Linda");
    store.append_claim(&mut txn, &john).expect("conformance[hist-t5]: append John");
    store.commit(txn).expect("conformance[hist-t5]: commit");

    let claims = store.load_subject_line(&agent, "hist-a-corp", "ceo", None)
        .expect("conformance[hist-t5]: load_subject_line");
    assert_eq!(claims.len(), 2, "conformance[hist-t5]: two claims");

    let config = EngineConfig::default();
    let now = dt(2025, 1, 1);
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

    assert_eq!(fold.all_claims[0].claim.fact().value, serde_json::json!("Linda"));
    assert_eq!(
        windows[0].valid_until, Some(dt(2024, 1, 10)),
        "conformance[hist-t5]: Linda's OWN end (Jan 10) must win, not John's later ordering key (Jan 24) — bug A regression"
    );
}

/// Bug B regression: a trusted, genuinely OVERLAPPING adjacent pair must mark BOTH entries
/// Contested with NO narrowing (own end preserved, not fabricated from the overlapping peer).
#[cfg(any(test, feature = "test-support"))]
fn hist_overlap_marks_both_entries_contested_no_narrowing<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use chrono::TimeZone;
    use mempill_types::HistoryEntryStatus;

    let agent = AgentId("conformance-hist-t6".into());
    let dt = |y, m, d| chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();

    // Joan [Sep 1 2024, Nov 1 2025) overlaps Linda [Sep 23 2024, Jan 24 2026).
    let joan = hist_vt_claim(&agent, "hist-b-corp", "ceo", "Joan", dt(2024, 9, 1), dt(2024, 9, 1), Some(dt(2025, 11, 1)));
    let linda = hist_vt_claim(&agent, "hist-b-corp", "ceo", "Linda", dt(2024, 9, 2), dt(2024, 9, 23), Some(dt(2026, 1, 24)));

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t6]: begin_atomic");
    store.append_claim(&mut txn, &joan).expect("conformance[hist-t6]: append Joan");
    store.append_claim(&mut txn, &linda).expect("conformance[hist-t6]: append Linda");
    store.commit(txn).expect("conformance[hist-t6]: commit");

    let claims = store.load_subject_line(&agent, "hist-b-corp", "ceo", None)
        .expect("conformance[hist-t6]: load_subject_line");
    let config = EngineConfig::default();
    let now = dt(2026, 6, 1);
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    assert!(fold.has_conflict, "conformance[hist-t6]: overlapping trusted claims must set has_conflict");
    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

    assert_eq!(fold.all_claims[0].claim.fact().value, serde_json::json!("Joan"));
    assert_eq!(
        windows[0].valid_until, Some(dt(2025, 11, 1)),
        "conformance[hist-t6]: Joan's own end must be preserved (NOT narrowed by overlapping Linda)"
    );
    assert!(windows[0].contested, "conformance[hist-t6]: Joan must be Contested (overlap)");
    assert!(windows[1].contested, "conformance[hist-t6]: Linda must also be Contested (has_conflict, structural)");
    let _ = HistoryEntryStatus::Contested; // documents the status this window feeds
}

/// Bug C regression: `Current` requires the effective window to CONTAIN `now`, not merely
/// membership in the raw live set. A predecessor whose own window has closed (superseded by
/// valid-time succession) must not be mislabeled Current.
#[cfg(any(test, feature = "test-support"))]
fn hist_current_requires_window_contains_now<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use chrono::TimeZone;

    let agent = AgentId("conformance-hist-t7".into());
    let dt = |y, m, d| chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();

    // Clean succession: Alice [Jan 2020, Jan 2022) -> Bob [Jan 2022, ∞).
    let alice = hist_vt_claim(&agent, "hist-c-corp", "ceo", "Alice", dt(2020, 1, 1), dt(2020, 1, 1), Some(dt(2022, 1, 1)));
    let bob = hist_vt_claim(&agent, "hist-c-corp", "ceo", "Bob", dt(2020, 1, 2), dt(2022, 1, 1), None);

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t7]: begin_atomic");
    store.append_claim(&mut txn, &alice).expect("conformance[hist-t7]: append Alice");
    store.append_claim(&mut txn, &bob).expect("conformance[hist-t7]: append Bob");
    store.commit(txn).expect("conformance[hist-t7]: commit");

    let claims = store.load_subject_line(&agent, "hist-c-corp", "ceo", None)
        .expect("conformance[hist-t7]: load_subject_line");
    let config = EngineConfig::default();
    let now = dt(2025, 1, 1); // well inside Bob's open window
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    assert!(fold.succession_selected, "conformance[hist-t7]: clean succession must narrow");
    assert_eq!(fold.live_claims.len(), 1, "conformance[hist-t7]: narrowed to Bob only");

    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);
    assert_eq!(fold.all_claims[0].claim.fact().value, serde_json::json!("Alice"));
    assert!(
        !windows[0].contains_now,
        "conformance[hist-t7]: Alice's window (closed Jan 2022) must NOT contain 'now' (2025) — bug C regression"
    );
}

/// New `Ended` status: a claim that is still raw-live (never explicitly Bound) but whose own
/// valid-time window has expired, with no live successor, must be `Ended` — not `Current`.
#[cfg(any(test, feature = "test-support"))]
fn hist_ended_status_for_expired_unsuperseded_claim<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use chrono::TimeZone;

    let agent = AgentId("conformance-hist-t8".into());
    let dt = |y, m, d| chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();

    // Single claim whose own end has already passed — never Bound.
    let claim = hist_vt_claim(&agent, "hist-d-corp", "role", "temp-lead", dt(2020, 1, 1), dt(2020, 1, 1), Some(dt(2020, 6, 1)));

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t8]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[hist-t8]: append");
    store.commit(txn).expect("conformance[hist-t8]: commit");

    let claims = store.load_subject_line(&agent, "hist-d-corp", "role", None)
        .expect("conformance[hist-t8]: load_subject_line");
    let config = EngineConfig::default();
    let now = dt(2026, 1, 1); // long after the claim's own end
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    assert_eq!(fold.live_claims.len(), 1, "conformance[hist-t8]: never Bound → still raw-live");
    assert!(!fold.has_conflict);

    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);
    assert!(fold.all_claims[0].is_live, "conformance[hist-t8]: raw is_live must still be true (never Bound)");
    assert!(
        !windows[0].contains_now,
        "conformance[hist-t8]: own window closed (Jun 2020) — must not contain 'now' (2026) → Ended, not Current"
    );
    assert!(!windows[0].contested);
}

/// Bug 2d regression: duplicate adjacent ordering keys must never yield a zero-length window
/// (`valid_until == valid_from`) — the effective successor is the next STRICTLY-later key.
#[cfg(any(test, feature = "test-support"))]
fn hist_duplicate_ordering_key_no_zero_length_window<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("conformance-hist-t9".into());
    let t1 = chrono::DateTime::<chrono::Utc>::from_timestamp(13_000_000, 0).unwrap();
    let t2 = chrono::DateTime::<chrono::Utc>::from_timestamp(13_000_100, 0).unwrap();

    // Two claims sharing the SAME tx_time (no valid_time — ordering key is tx_time), plus a
    // third claim with a strictly later tx_time.
    let make_c = |val: &str, tx: chrono::DateTime<chrono::Utc>| -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "hist-e-corp".to_owned(), predicate: "lead".to_owned(), value: serde_json::json!(val) },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        )
    };
    let c_tie_a = make_c("TieA", t1);
    let c_tie_b = make_c("TieB", t1);
    let c_later = make_c("Later", t2);

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t9]: begin_atomic");
    store.append_claim(&mut txn, &c_tie_a).expect("conformance[hist-t9]: append TieA");
    store.append_claim(&mut txn, &c_tie_b).expect("conformance[hist-t9]: append TieB");
    store.append_claim(&mut txn, &c_later).expect("conformance[hist-t9]: append Later");
    store.commit(txn).expect("conformance[hist-t9]: commit");

    let claims = store.load_subject_line(&agent, "hist-e-corp", "lead", None)
        .expect("conformance[hist-t9]: load_subject_line");
    let config = EngineConfig::default();
    let now = t2 + chrono::Duration::seconds(1);
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

    // Entries 0 and 1 (the tied pair) must NOT close against each other (zero-length) — both
    // must skip ahead to entry 2's strictly-later key.
    assert_eq!(windows[0].valid_until, Some(t2), "conformance[hist-t9]: first of the tied pair must skip to the strictly-later key");
    assert_eq!(windows[1].valid_until, Some(t2), "conformance[hist-t9]: second of the tied pair must use the strictly-later key");
    assert_eq!(windows[2].valid_until, None, "conformance[hist-t9]: last entry is open-ended");
    assert_ne!(windows[0].valid_until, fold.all_claims[0].claim.valid_time().start,
        "conformance[hist-t9]: no zero-length window (valid_until must not equal valid_from)");
}

/// Granularity attribution follows whichever endpoint value actually won: own `end_granularity`
/// when the own end is used, successor's `start_granularity` only when the successor's
/// ordering key is used AND itself sourced from `valid_time.start`.
#[cfg(any(test, feature = "test-support"))]
fn hist_granularity_follows_winning_endpoint_source<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use chrono::TimeZone;
    use mempill_types::DateGranularity;

    let agent = AgentId("conformance-hist-t10".into());
    let dt = |y, m, d| chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();

    // Own end (Day granularity) wins over a later, non-overlapping successor (Year granularity).
    let own_end_wins = Claim::new(
        ClaimRef::new_random(), agent.clone(),
        Fact { subject: "hist-f-corp".to_owned(), predicate: "ceo".to_owned(), value: serde_json::json!("Predecessor") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(dt(2024, 1, 1)),
        ValidTime { start: Some(dt(2024, 1, 1)), end: Some(dt(2024, 1, 10)), valid_time_confidence: 0.9, start_granularity: None, end_granularity: Some(DateGranularity::Day) },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        Criticality::Low, vec![], None, None,
    );
    let successor = Claim::new(
        ClaimRef::new_random(), agent.clone(),
        Fact { subject: "hist-f-corp".to_owned(), predicate: "ceo".to_owned(), value: serde_json::json!("Successor") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(dt(2024, 1, 2)),
        ValidTime { start: Some(dt(2024, 6, 1)), end: None, valid_time_confidence: 0.9, start_granularity: Some(DateGranularity::Year), end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        Criticality::Low, vec![], None, None,
    );

    let mut txn = store.begin_atomic(&agent).expect("conformance[hist-t10]: begin_atomic");
    store.append_claim(&mut txn, &own_end_wins).expect("conformance[hist-t10]: append predecessor");
    store.append_claim(&mut txn, &successor).expect("conformance[hist-t10]: append successor");
    store.commit(txn).expect("conformance[hist-t10]: commit");

    let claims = store.load_subject_line(&agent, "hist-f-corp", "ceo", None)
        .expect("conformance[hist-t10]: load_subject_line");
    let config = EngineConfig::default();
    let now = dt(2025, 1, 1);
    let fold = truth_engine::fold(claims, |_| vec![], now, None, &config, &std::collections::HashMap::new());
    let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

    assert_eq!(fold.all_claims[0].claim.fact().value, serde_json::json!("Predecessor"));
    assert_eq!(
        windows[0].valid_until_granularity, Some(DateGranularity::Day),
        "conformance[hist-t10]: own end_granularity (Day) must win, NOT the successor's start_granularity (Year)"
    );
}

// ── Disposition-scope conformance tests ───────────────────────────────────────

/// `load_ledger_for_claims` returns exactly the entries for the given claim refs.
///
/// Writes two claims each with a ledger entry, queries for only one, asserts only
/// one entry is returned — proving the method is correctly scoped to `claim_refs`.
#[cfg(any(test, feature = "test-support"))]
fn test_load_ledger_for_claims_scoped<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-lfc-t1".into());

    let claim_a = make_claim(&agent, "scope-subj", "scope-pred");
    let claim_b = make_claim(&agent, "scope-subj", "scope-pred-b");
    let ref_a = claim_a.claim_ref().clone();
    let ref_b = claim_b.claim_ref().clone();

    let ledger_a = make_ledger_entry(&agent, &ref_a);
    let ledger_b = make_ledger_entry(&agent, &ref_b);

    let mut txn = store.begin_atomic(&agent).expect("conformance[lfc-t1]: begin_atomic");
    store.append_claim(&mut txn, &claim_a).expect("conformance[lfc-t1]: append_claim_a");
    store.append_claim(&mut txn, &claim_b).expect("conformance[lfc-t1]: append_claim_b");
    store.append_ledger_entry(&mut txn, &ledger_a).expect("conformance[lfc-t1]: append_ledger_a");
    store.append_ledger_entry(&mut txn, &ledger_b).expect("conformance[lfc-t1]: append_ledger_b");
    store.commit(txn).expect("conformance[lfc-t1]: commit");

    // Query for only claim_a — must not return claim_b's entry.
    let result = store
        .load_ledger_for_claims(&agent, &[ref_a.clone()], None)
        .expect("conformance[lfc-t1]: load_ledger_for_claims must not error");

    assert_eq!(result.len(), 1, "conformance[lfc-t1]: exactly one entry for claim_a");
    assert_eq!(result[0].claim_ref, ref_a, "conformance[lfc-t1]: entry must be for claim_a");

    // Empty input → empty result (no IN () SQL emitted).
    let empty = store
        .load_ledger_for_claims(&agent, &[], None)
        .expect("conformance[lfc-t1]: empty input must not error");
    assert!(empty.is_empty(), "conformance[lfc-t1]: empty input must return empty vec");
}

/// Superseded claim is correctly excluded despite a large agent ledger.
///
/// Scenario: one agent accumulates many ledger entries across many subject-lines
/// (simulating a high-volume agent). On ONE subject-line, claim A is committed then
/// superseded by claim B (B's tx_time > A's). After supersession, `query_memory`
/// must return B as the live belief — not A (resurrected), not Contested.
///
/// Under the old agent-wide cap (10_000), if the supersession entry for A fell
/// outside the cap window it was missing from the disposition map and A defaulted
/// to live — this test would have returned Contested or "A" instead of "B".
#[cfg(any(test, feature = "test-support"))]
fn test_superseded_claim_excluded_despite_large_agent_ledger<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{

    let agent = AgentId("conformance-dscope-t1".into());

    // ── 1. Flood the agent ledger with entries on OTHER subject-lines ──────────
    // Use 50 noise claims with 2 ledger entries each = 100 total noise entries.
    // This is a focused correctness proof that would fail under a cap of e.g. 20.
    // (We don't literally write 10k rows; the unit test for load_ledger_for_claims
    //  above proves the scoping is correct; this test proves end-to-end correctness.)
    for i in 0..50u32 {
        let noise_claim = Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            mempill_types::claim::Fact {
                subject: format!("noise-subject-{i}"),
                predicate: "noise-predicate".to_owned(),
                value: serde_json::json!(i),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        );
        let noise_ref = noise_claim.claim_ref().clone();
        let noise_ledger1 = make_ledger_entry(&agent, &noise_ref);
        let noise_ledger2 = LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: noise_ref.clone(),
            event_kind: LedgerEventKind::ValidityAsserted,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(Utc::now()),
        };
        let mut txn = store.begin_atomic(&agent).expect("dscope[t1]: noise begin_atomic");
        store.append_claim(&mut txn, &noise_claim).expect("dscope[t1]: noise append_claim");
        store.append_ledger_entry(&mut txn, &noise_ledger1).expect("dscope[t1]: noise ledger1");
        store.append_ledger_entry(&mut txn, &noise_ledger2).expect("dscope[t1]: noise ledger2");
        store.commit(txn).expect("dscope[t1]: noise commit");
    }

    // ── 2. Ingest claim A on the test subject-line ────────────────────────────
    let t_a = chrono::DateTime::<Utc>::from_timestamp(1_000_000, 0).unwrap();
    let claim_a = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        mempill_types::claim::Fact {
            subject: "dscope-org".to_owned(),
            predicate: "ceo".to_owned(),
            value: serde_json::json!("Alice"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(t_a),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );
    let ref_a = claim_a.claim_ref().clone();

    // A is initially committed.
    let ledger_a_committed = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref_a.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(t_a),
    };

    let mut txn = store.begin_atomic(&agent).expect("dscope[t1]: claim_a begin");
    store.append_claim(&mut txn, &claim_a).expect("dscope[t1]: claim_a append");
    store.append_ledger_entry(&mut txn, &ledger_a_committed).expect("dscope[t1]: claim_a ledger");
    store.commit(txn).expect("dscope[t1]: claim_a commit");

    // ── 3. Ingest claim B (supersedes A) ─────────────────────────────────────
    let t_b = chrono::DateTime::<Utc>::from_timestamp(2_000_000, 0).unwrap();
    let claim_b = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        mempill_types::claim::Fact {
            subject: "dscope-org".to_owned(),
            predicate: "ceo".to_owned(),
            value: serde_json::json!("Bob"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(t_b),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0 , start_granularity: None, end_granularity: None},
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );
    let ref_b = claim_b.claim_ref().clone();

    // A is superseded; B is committed.
    let ledger_a_superseded = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref_a.clone(),
        event_kind: LedgerEventKind::ValidityAsserted,
        disposition: Disposition::Superseded,
        rationale: None,
        recorded_at: TransactionTime(t_b),
    };
    let ledger_b_committed = LedgerEntry {
        entry_id: Uuid::new_v4(),
        agent_id: agent.clone(),
        claim_ref: ref_b.clone(),
        event_kind: LedgerEventKind::ClaimCommitted,
        disposition: Disposition::CommittedCheap,
        rationale: None,
        recorded_at: TransactionTime(t_b),
    };

    let mut txn = store.begin_atomic(&agent).expect("dscope[t1]: claim_b begin");
    store.append_claim(&mut txn, &claim_b).expect("dscope[t1]: claim_b append");
    store.append_ledger_entry(&mut txn, &ledger_a_superseded).expect("dscope[t1]: ledger_a_superseded");
    store.append_ledger_entry(&mut txn, &ledger_b_committed).expect("dscope[t1]: ledger_b_committed");
    store.commit(txn).expect("dscope[t1]: claim_b commit");

    // ── 4. Verify load_ledger_for_claims returns both entries for both refs ───
    let scoped = store
        .load_ledger_for_claims(&agent, &[ref_a.clone(), ref_b.clone()], None)
        .expect("dscope[t1]: load_ledger_for_claims must not error");
    // Expect: ledger_a_committed + ledger_a_superseded + ledger_b_committed = 3 entries.
    assert_eq!(
        scoped.len(), 3,
        "dscope[t1]: load_ledger_for_claims must return all 3 entries for the 2 subject-line claims"
    );

    // ── 5. End-to-end: truth_engine fold must return only Bob (B) as live ───────
    // Mirrors exactly what query_memory does after load_ledger_for_claims.
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let subject_claims = store
        .load_subject_line(&agent, "dscope-org", "ceo", None)
        .expect("dscope[t1]: load_subject_line must not error");
    assert_eq!(subject_claims.len(), 2, "dscope[t1]: must have 2 claims on subject-line");

    let subject_refs: Vec<ClaimRef> = subject_claims.iter().map(|c| c.claim_ref().clone()).collect();
    let scoped_ledger = store
        .load_ledger_for_claims(&agent, &subject_refs, None)
        .expect("dscope[t1]: load_ledger_for_claims must not error after B committed");
    let latest_disposition = build_latest_disposition_map(&scoped_ledger);

    let now = chrono::DateTime::<Utc>::from_timestamp(3_000_000, 0).unwrap();
    let config = EngineConfig::default();
    let fold = truth_engine::fold(
        subject_claims,
        |_cref| vec![],
        now,
        None, // valid_at_instant: None = backward-compatible (use as_of_tx_time for selection)
        &config,
        &latest_disposition,
    );

    // With correct disposition map: A is Superseded → not live; B is CommittedCheap → live.
    // fold.live_claims must be exactly [B].
    assert_eq!(
        fold.live_claims.len(), 1,
        "dscope[t1]: exactly one live claim (Bob/B); got {:?}",
        fold.live_claims.iter().map(|cs| &cs.claim.fact().value).collect::<Vec<_>>()
    );
    assert_eq!(
        fold.live_claims[0].claim.fact().value,
        serde_json::json!("Bob"),
        "dscope[t1]: the live claim must be Bob (B), not Alice (A — superseded)"
    );
}

/// WRITE/AUDIT-path regression: `ingest_claim`, `reconcile`, `submit_adjudication`, and
/// `sweep_adjudications` must all compute correct dispositions despite an agent ledger
/// with more than 10,000 entries (AC-004-1 / AC-004-2, US-004).
///
/// Prior to this fix, these four use-cases called the CAPPED, agent-wide
/// `PersistencePort::load_ledger(agent_id, None, 10_000)` to build their disposition map.
/// A superseded/resolved claim whose disposition-changing ledger entry fell outside the
/// 10,000-row cap window was silently treated as still-live, causing:
///   - `ingest_claim`: incorrect incumbent/conflict detection against a stale "live" claim
///   - `reconcile`: an already-superseded claim re-entering reconciliation as if live
///   - `submit_adjudication`: a stale state guard allowing a duplicate/expired verdict to apply
///   - `sweep_adjudications`: a stale state guard reverting an already-resolved claim
///
/// This test floods the agent ledger with >10,000 noise entries BEFORE exercising each
/// of the four write/audit use-cases, proving each one's disposition computation is now
/// scoped via `load_ledger_for_claims` (uncapped, claim-scoped) and is therefore correct
/// regardless of total agent ledger size — mirroring the read-path proof above.
#[cfg(any(test, feature = "test-support"))]
fn test_write_audit_paths_correct_despite_large_agent_ledger<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use std::sync::Arc;

    use crate::application::{
        dto::{IngestClaimRequest, ReconcileRequest},
        ingest_claim::IngestClaimUseCase,
        reconcile::ReconcileUseCase,
        submit_adjudication::SubmitAdjudicationUseCase,
        sweep_adjudications::SweepAdjudicationsUseCase,
    };
    use crate::config::EngineConfig;
    use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
    use crate::noop::NoOpOracle;
    use crate::ports::pending_adjudication::{
        OrphanedQueuedClaim, PendingAdjudicationPort, PendingAdjudicationRow,
    };
    use crate::MemError;
    use std::sync::Mutex;

    let agent = AgentId("conformance-dscope-wa1".into());

    // ── 1. Flood the agent ledger with > 10_000 noise entries on OTHER claims ──────
    // Exceeds the old cap (10_000) so any capped agent-wide load_ledger call would
    // silently drop the disposition-critical entries written in steps 2-5 below.
    for i in 0..1010u32 {
        let noise_claim = Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact {
                subject: format!("wa-noise-subject-{i}"),
                predicate: "noise-predicate".to_owned(),
                value: serde_json::json!(i),
            },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Low,
            vec![],
            None,
            None,
        );
        let noise_ref = noise_claim.claim_ref().clone();
        let mut txn = store.begin_atomic(&agent).expect("dscope[wa1]: noise begin_atomic");
        store.append_claim(&mut txn, &noise_claim).expect("dscope[wa1]: noise append_claim");
        // 10 ledger entries per noise claim => 10,100 total noise ledger rows (> old 10_000 cap).
        for _ in 0..10 {
            let entry = make_ledger_entry(&agent, &noise_ref);
            store.append_ledger_entry(&mut txn, &entry).expect("dscope[wa1]: noise ledger entry");
        }
        store.commit(txn).expect("dscope[wa1]: noise commit");
    }

    let config = EngineConfig::default();

    // ── 2. ingest_claim: incumbent A committed directly against the store, then a
    // conflicting claim B is ingested. The use-case must see A as the live incumbent
    // (disposition map correctly scoped, not silently missing it due to the flood). ──
    let claim_a = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "wa-ingest-subject".into(), predicate: "wa-predicate".into(), value: serde_json::json!("Alice") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now() - chrono::Duration::seconds(10)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );
    let ref_a = claim_a.claim_ref().clone();
    {
        let mut txn = store.begin_atomic(&agent).expect("dscope[wa1]: claim_a begin");
        store.append_claim(&mut txn, &claim_a).expect("dscope[wa1]: claim_a append");
        store.append_ledger_entry(&mut txn, &make_ledger_entry(&agent, &ref_a))
            .expect("dscope[wa1]: claim_a ledger");
        store.commit(txn).expect("dscope[wa1]: claim_a commit");
    }

    let store_arc = Arc::clone(&store);
    let ingest_uc = IngestClaimUseCase::new(
        Arc::clone(&store_arc),
        None::<Arc<NoOpOracle>>,
        None,
        config.clone(),
    );
    let ingest_req = IngestClaimRequest {
        agent_id: agent.clone(),
        subject: "wa-ingest-subject".into(),
        predicate: "wa-predicate".into(),
        value: serde_json::json!("Bob"), // conflicts with Alice
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Medium,
        derived_from: vec![],
    };
    let ingest_resp = ingest_uc
        .execute_with_time(ingest_req, Utc::now())
        .expect("dscope[wa1]: ingest_claim must not error");
    // Oracle absent (B11a): a fresh External contradiction against a correctly-visible
    // live incumbent must surface as Contested — proving A was seen as live despite the flood.
    assert_eq!(
        ingest_resp.disposition, Disposition::Contested,
        "dscope[wa1]: ingest_claim must detect the live incumbent A despite >10k noise ledger rows"
    );
    assert_eq!(
        ingest_resp.contested_with, vec![ref_a.clone()],
        "dscope[wa1]: ingest_claim must surface A as the contested incumbent"
    );

    // ── 3. reconcile: claim C committed, then superseded by claim D directly in the
    // store. reconcile() must NOT re-surface C as live (it must see the Superseded entry). ──
    let claim_c = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "wa-reconcile-subject".into(), predicate: "wa-predicate".into(), value: serde_json::json!("Carol") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now() - chrono::Duration::seconds(20)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );
    let ref_c = claim_c.claim_ref().clone();
    let claim_d = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "wa-reconcile-subject".into(), predicate: "wa-predicate".into(), value: serde_json::json!("Dave") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now() - chrono::Duration::seconds(10)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );
    let ref_d = claim_d.claim_ref().clone();
    {
        let mut txn = store.begin_atomic(&agent).expect("dscope[wa1]: claim_c begin");
        store.append_claim(&mut txn, &claim_c).expect("dscope[wa1]: claim_c append");
        store.append_ledger_entry(&mut txn, &make_ledger_entry(&agent, &ref_c))
            .expect("dscope[wa1]: claim_c ledger");
        store.commit(txn).expect("dscope[wa1]: claim_c commit");

        let mut txn = store.begin_atomic(&agent).expect("dscope[wa1]: claim_d begin");
        store.append_claim(&mut txn, &claim_d).expect("dscope[wa1]: claim_d append");
        let superseded_c = LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref_c.clone(),
            event_kind: LedgerEventKind::ValidityAsserted,
            disposition: Disposition::Superseded,
            rationale: None,
            recorded_at: TransactionTime(Utc::now()),
        };
        store.append_ledger_entry(&mut txn, &superseded_c).expect("dscope[wa1]: superseded_c");
        store.append_ledger_entry(&mut txn, &make_ledger_entry(&agent, &ref_d))
            .expect("dscope[wa1]: claim_d ledger");
        store.commit(txn).expect("dscope[wa1]: claim_d commit");
    }

    let reconcile_uc = ReconcileUseCase::new(
        Arc::clone(&store_arc),
        None::<Arc<NoOpOracle>>,
        config.clone(),
    );
    let reconcile_resp = reconcile_uc
        .execute(ReconcileRequest {
            agent_id: agent.clone(),
            subject_lines: vec![("wa-reconcile-subject".into(), "wa-predicate".into())],
        })
        .expect("dscope[wa1]: reconcile must not error");
    let reconciled_refs: Vec<_> = reconcile_resp.outcomes.iter().map(|(r, _)| r.clone()).collect();
    assert!(
        !reconciled_refs.contains(&ref_c),
        "dscope[wa1]: reconcile must NOT re-surface superseded claim C despite >10k noise ledger rows; outcomes={:?}",
        reconcile_resp.outcomes
    );
    assert!(
        reconciled_refs.contains(&ref_d),
        "dscope[wa1]: reconcile must process live claim D; outcomes={:?}",
        reconcile_resp.outcomes
    );
    // Disposition + escalation-count: D is the SOLE live claim on this subject-line (C is
    // Superseded) — no genuine conflict, so D must resolve CommittedCheap with zero escalations.
    let d_disposition = reconcile_resp.outcomes.iter()
        .find(|(r, _)| *r == ref_d)
        .map(|(_, d)| d.clone())
        .expect("dscope[wa1]: D must be present in reconcile outcomes");
    assert_eq!(
        d_disposition, Disposition::CommittedCheap,
        "dscope[wa1]: sole live claim D (no other live claim to conflict with) must resolve CommittedCheap"
    );
    assert_eq!(
        reconcile_resp.oracle_escalations, 0,
        "dscope[wa1]: no genuine conflict on this subject-line — zero HeavyPath escalations expected"
    );
    // No supersession edge/ledger row written BY reconcile (A6/I7): reconcile() never calls
    // supersession::execute. C's Superseded entry was written directly by the test setup
    // above, not by reconcile — the ledger for both C and D must gain exactly the
    // AdjudicationResolved rows reconcile itself appends, nothing else.
    let edges_d = store.load_edges_for(&agent, &ref_d).expect("dscope[wa1]: load_edges_for D");
    assert!(edges_d.is_empty(), "dscope[wa1]: reconcile() must never write a supersession edge — found {} edge(s) on D", edges_d.len());
    let ledger_d_after = store.load_ledger_for_claims(&agent, &[ref_d.clone()], None).expect("dscope[wa1]: load ledger D after reconcile");
    assert!(
        ledger_d_after.iter().all(|e| e.event_kind != LedgerEventKind::ValidityAsserted),
        "dscope[wa1]: reconcile() must never append a ValidityAsserted (supersession) ledger row"
    );
    // Post-state belief: query_memory on this subject-line must agree with reconcile's view —
    // D is the sole live, uncontested claim, so query_memory must resolve to D, never Contested.
    let qm_uc = crate::application::query_memory::QueryMemoryUseCase::new(
        Arc::clone(&store_arc),
        None::<Arc<crate::noop::NoOpVector>>,
        config.clone(),
    );
    let qm_resp = qm_uc
        .execute_with_time(
            crate::application::dto::QueryMemoryRequest {
                agent_id: agent.clone(),
                subject: "wa-reconcile-subject".into(),
                predicate: "wa-predicate".into(),
                as_of_tx_time: None,
                valid_at: None,
            },
            Utc::now(),
        )
        .expect("dscope[wa1]: query_memory must not error");
    assert_ne!(
        qm_resp.belief.status, mempill_types::BeliefStatus::Contested,
        "dscope[wa1]: query_memory must agree with reconcile's uncontested view of D"
    );
    let qm_primary = qm_resp.belief.primary.as_ref().expect("dscope[wa1]: query_memory primary must be present for D");
    assert_eq!(qm_primary.claim_ref, ref_d, "dscope[wa1]: query_memory primary must be D, matching reconcile's live-claim view");

    // ── 4/5. submit_adjudication + sweep_adjudications: state-guard idempotency. ──────
    // A challenger claim E is QueuedForAdjudication in the ledger. A duplicate/late
    // verdict submit (or a stale sweep) must correctly see E's CURRENT (already-resolved)
    // disposition rather than a stale QueuedForAdjudication view lost behind the flood.
    let claim_e = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "wa-adj-subject".into(), predicate: "wa-predicate".into(), value: serde_json::json!("Eve") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now() - chrono::Duration::seconds(30)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );
    let ref_e = claim_e.claim_ref().clone();
    let claim_f = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "wa-adj-subject".into(), predicate: "wa-predicate".into(), value: serde_json::json!("Frank") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(Utc::now() - chrono::Duration::seconds(25)),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );
    let ref_f = claim_f.claim_ref().clone();
    {
        let mut txn = store.begin_atomic(&agent).expect("dscope[wa1]: claim_e begin");
        store.append_claim(&mut txn, &claim_e).expect("dscope[wa1]: claim_e append");
        // E starts QueuedForAdjudication, then is resolved to CommittedCheap (Affirm) by a
        // PRIOR verdict-apply — this is the "already resolved" state a duplicate/stale
        // caller must correctly observe.
        let queued_e = LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref_e.clone(),
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::QueuedForAdjudication,
            rationale: None,
            recorded_at: TransactionTime(Utc::now() - chrono::Duration::seconds(5)),
        };
        store.append_ledger_entry(&mut txn, &queued_e).expect("dscope[wa1]: queued_e");
        store.append_claim(&mut txn, &claim_f).expect("dscope[wa1]: claim_f append");
        store.append_ledger_entry(&mut txn, &make_ledger_entry(&agent, &ref_f))
            .expect("dscope[wa1]: claim_f ledger");
        store.commit(txn).expect("dscope[wa1]: claim_e/f commit");

        let mut txn = store.begin_atomic(&agent).expect("dscope[wa1]: resolve_e begin");
        let resolved_e = LedgerEntry {
            entry_id: Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref_e.clone(),
            event_kind: LedgerEventKind::AdjudicationResolved,
            disposition: Disposition::CommittedCheap,
            rationale: None,
            recorded_at: TransactionTime(Utc::now()),
        };
        store.append_ledger_entry(&mut txn, &resolved_e).expect("dscope[wa1]: resolved_e");
        store.commit(txn).expect("dscope[wa1]: resolve_e commit");
    }

    // In-memory PendingAdjudicationPort stub, seeded with a still-"pending" row for E
    // (simulating a duplicate verdict arriving after E was already resolved above).
    struct StubPendingStore {
        rows: Mutex<Vec<PendingAdjudicationRow>>,
    }
    impl PendingAdjudicationPort for StubPendingStore {
        type Error = std::io::Error;
        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error> {
            self.rows.lock().unwrap().push(row.clone());
            Ok(())
        }
        fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> {
            Ok(self.rows.lock().unwrap().iter().find(|r| r.handle_id == handle_id).cloned())
        }
        fn list_pending(&self, _agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> {
            Ok(self.rows.lock().unwrap().clone())
        }
        fn list_expired(&self, _now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> {
            Ok(vec![])
        }
        fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> {
            for r in self.rows.lock().unwrap().iter_mut() {
                if r.handle_id == handle_id { r.status = "resolved".into(); }
            }
            Ok(())
        }
        fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> {
            for r in self.rows.lock().unwrap().iter_mut() {
                if r.handle_id == handle_id { r.status = "expired".into(); }
            }
            Ok(())
        }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> {
            Ok(vec![])
        }
    }

    let handle_id = Uuid::new_v4();
    let dummy_adj_request = mempill_types::AdjudicationRequest {
        subject_line: mempill_types::SubjectLineRef {
            agent_id: agent.clone(),
            subject: "wa-adj-subject".into(),
            predicate: "wa-predicate".into(),
        },
        incumbent: mempill_types::Belief {
            claim_ref: ref_f.clone(),
            fact: claim_f.fact().clone(),
            provenance: claim_f.provenance().clone(),
            valid_time: claim_f.valid_time().clone(),
            transaction_time: claim_f.transaction_time().clone(),
            confidence: claim_f.confidence().clone(),
            currency_signal: mempill_types::CurrencySignal {
                last_refreshed_at: claim_f.transaction_time().clone(),
                state: mempill_types::CurrencyState::Fresh,
                corroboration_count: 0,
            },
            criticality: claim_f.criticality().clone(),
        },
        challenger: claim_e.clone(),
        criticality: Criticality::Medium,
        reason: mempill_types::OverturnReason::ExternalContradiction,
    };
    let pending_store = Arc::new(StubPendingStore {
        rows: Mutex::new(vec![PendingAdjudicationRow {
            handle_id,
            agent_id: agent.clone(),
            subject: "wa-adj-subject".into(),
            predicate: "wa-predicate".into(),
            challenger_claim_ref: ref_e.clone(),
            incumbent_claim_ref: ref_f.clone(),
            request_payload: dummy_adj_request,
            queued_at: Utc::now() - chrono::Duration::seconds(5),
            expires_at: None,
            status: "pending".into(),
        }]),
    });
    // `ErasedPendingStoreAdapter::new` requires an owned `S: PendingAdjudicationPort`, not
    // `Arc<S>` — wrap the shared `Arc<StubPendingStore>` in a thin delegating newtype so it
    // can still be shared with `sweep_uc` below (mirrors the `SharedWrapper` pattern used
    // elsewhere in this codebase, e.g. `ingest_claim.rs` tests).
    struct SharedPendingWrapper(Arc<StubPendingStore>);
    impl PendingAdjudicationPort for SharedPendingWrapper {
        type Error = std::io::Error;
        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error> {
            self.0.insert_pending(row)
        }
        fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> {
            self.0.get_pending(handle_id)
        }
        fn list_pending(&self, agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> {
            self.0.list_pending(agent_id)
        }
        fn list_expired(&self, now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> {
            self.0.list_expired(now)
        }
        fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> {
            self.0.mark_resolved(handle_id)
        }
        fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> {
            self.0.mark_expired(handle_id)
        }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> {
            self.0.list_queued_orphan_claims()
        }
    }
    let erased_pending: Arc<dyn ErasedPendingStore> = Arc::new(ErasedPendingStoreAdapter::new(
        SharedPendingWrapper(Arc::clone(&pending_store)),
    ));

    let submit_uc = SubmitAdjudicationUseCase::new(Arc::clone(&store_arc), Arc::clone(&erased_pending));
    let dup_response = mempill_types::AdjudicationResponse {
        handle_id,
        verdict: mempill_types::AdjudicationVerdict::Deny,
        evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
    };
    let submit_result = submit_uc.execute(handle_id, dup_response, Utc::now());
    assert!(
        matches!(submit_result, Err(MemError::AdjudicationHandleNotFound { .. })),
        "dscope[wa1]: submit_adjudication state guard must see E's CURRENT resolved disposition \
         (CommittedCheap) despite >10k noise ledger rows, not a stale QueuedForAdjudication view; got {submit_result:?}"
    );

    // sweep_adjudications must reach the identical conclusion via revert_expired_row: the
    // pending row for E is stale (challenger already resolved) — idempotent no-op (false).
    let sweep_uc = SweepAdjudicationsUseCase::new(Arc::clone(&store_arc), Arc::clone(&erased_pending));
    let stale_row = PendingAdjudicationRow {
        handle_id,
        agent_id: agent.clone(),
        subject: "wa-adj-subject".into(),
        predicate: "wa-predicate".into(),
        challenger_claim_ref: ref_e.clone(),
        incumbent_claim_ref: ref_f.clone(),
        request_payload: mempill_types::AdjudicationRequest {
            subject_line: mempill_types::SubjectLineRef {
                agent_id: agent.clone(),
                subject: "wa-adj-subject".into(),
                predicate: "wa-predicate".into(),
            },
            incumbent: mempill_types::Belief {
                claim_ref: ref_f.clone(),
                fact: claim_f.fact().clone(),
                provenance: claim_f.provenance().clone(),
                valid_time: claim_f.valid_time().clone(),
                transaction_time: claim_f.transaction_time().clone(),
                confidence: claim_f.confidence().clone(),
                currency_signal: mempill_types::CurrencySignal {
                    last_refreshed_at: claim_f.transaction_time().clone(),
                    state: mempill_types::CurrencyState::Fresh,
                    corroboration_count: 0,
                },
                criticality: claim_f.criticality().clone(),
            },
            challenger: claim_e.clone(),
            criticality: Criticality::Medium,
            reason: mempill_types::OverturnReason::ExternalContradiction,
        },
        queued_at: Utc::now() - chrono::Duration::seconds(5),
        expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
        status: "pending".into(),
    };
    let reverted = sweep_uc
        .revert_expired_row(&stale_row, Utc::now())
        .expect("dscope[wa1]: sweep revert_expired_row must not error");
    assert!(
        !reverted,
        "dscope[wa1]: sweep_adjudications must correctly see E's CURRENT resolved disposition \
         despite >10k noise ledger rows and skip the stale revert (idempotency guard)"
    );
}

// ── DIAG_silent_succession §6(b) — reconcile incumbent-selection conformance ──────────────

/// Cross-adapter conformance: for a genuine 2-claim overlap, `reconcile`'s per-candidate
/// disposition view must AGREE with `query_memory`'s primary/status view.
///
/// Regression for DIAG_silent_succession §6(b): `reconcile.rs`'s per-subject-line
/// `incumbent` used to be a single `fold.live_claims.first()` shared across every
/// candidate; when the candidate under evaluation WAS that claim, `classify_conflict`'s
/// step-3 same-value check trivially matched (identical claim) and cheap-pathed a
/// genuinely contested line into a silent `CommittedCheap`. `reconcile` now selects a
/// per-candidate incumbent that is NEVER the candidate itself, so both candidates on a
/// true overlap must escalate, and `query_memory` must show `Contested` / `primary: None`
/// — never a mismatch where reconcile reports one candidate resolved while the read path
/// still sees a contested line.
#[cfg(any(test, feature = "test-support"))]
pub fn run_reconcile_incumbent_selection_matches_query_memory_primary_conformance<P>(
    store: &std::sync::Arc<P>,
) where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use std::sync::Arc;

    use crate::application::{
        dto::{QueryMemoryRequest, ReconcileRequest},
        query_memory::QueryMemoryUseCase,
        reconcile::ReconcileUseCase,
    };
    use crate::config::EngineConfig;
    use crate::noop::{NoOpOracle, NoOpVector};
    use mempill_types::BeliefStatus;

    let agent = AgentId("conformance-reconcile-incumbent-selection".into());
    // Both claims tx-stamped strictly in the PAST (not `+ duration`, which would place a
    // claim's transaction_time in the future relative to the later as_of=now cutoff used
    // by query_memory below, silently excluding it from tx-time visibility).
    let tx = Utc::now() - chrono::Duration::seconds(10);
    let claim_a = make_vt_free_claim(&agent, "ri-subj", "ri-pred", serde_json::json!("Alice"), tx);
    let claim_b = make_vt_free_claim(&agent, "ri-subj", "ri-pred", serde_json::json!("Bob"), tx + chrono::Duration::seconds(1));
    let mut txn = store.begin_atomic(&agent).expect("ri: begin");
    store.append_claim(&mut txn, &claim_a).expect("ri: append a");
    store.append_claim(&mut txn, &claim_b).expect("ri: append b");
    store.commit(txn).expect("ri: commit");

    let config = EngineConfig::default();
    let reconcile_uc = ReconcileUseCase::new(Arc::clone(store), None::<Arc<NoOpOracle>>, config.clone());
    let resp = reconcile_uc
        .execute(ReconcileRequest {
            agent_id: agent.clone(),
            subject_lines: vec![("ri-subj".into(), "ri-pred".into())],
        })
        .expect("ri: reconcile must not error");
    assert_eq!(resp.outcomes.len(), 2, "ri: both live claims must produce an outcome");
    assert_eq!(
        resp.oracle_escalations, 2,
        "ri: BOTH candidates must escalate — per-candidate incumbent selection never \
         feeds a candidate itself as its own incumbent (DIAG §6(b))"
    );
    for (r, disposition) in &resp.outcomes {
        assert_ne!(
            *disposition,
            Disposition::CommittedCheap,
            "ri: claim {r:?} on a genuinely contested line must never resolve CommittedCheap"
        );
    }

    let qm_uc = QueryMemoryUseCase::new(Arc::clone(store), None::<Arc<NoOpVector>>, config);
    let qm_resp = qm_uc
        .execute_with_time(
            QueryMemoryRequest {
                agent_id: agent,
                subject: "ri-subj".into(),
                predicate: "ri-pred".into(),
                as_of_tx_time: None,
                valid_at: None,
            },
            Utc::now(),
        )
        .expect("ri: query_memory must not error");
    assert_eq!(
        qm_resp.belief.status,
        BeliefStatus::Contested,
        "ri: query_memory must agree with reconcile's contested view — neither candidate \
         resolved CommittedCheap, so the read path must also see Contested"
    );
    assert!(
        qm_resp.belief.primary.is_none(),
        "ri: query_memory primary must be null on a Contested line, matching reconcile's view"
    );
}

/// Cross-adapter conformance: sweeping an EXPIRED pending row reverts the challenger to
/// `Contested` and writes NO supersession (no Bound assertion, no edge) on the incumbent;
/// supersession (incumbent → Superseded, challenger → CommittedCheap) happens ONLY when
/// `submit_adjudication` is explicitly called with an `Affirm` verdict. Proves
/// DIAG_silent_succession §6(b)'s "gate supersession::execute on a resolved disposition,
/// never on Contested/QueuedForAdjudication" contract end-to-end, cross-adapter.
#[cfg(any(test, feature = "test-support"))]
pub fn run_sweep_resolves_then_supersession_happens_only_via_submit_adjudication_conformance<P>(
    store: &std::sync::Arc<P>,
) where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use std::sync::{Arc, Mutex};

    use crate::application::{
        submit_adjudication::SubmitAdjudicationUseCase, sweep_adjudications::SweepAdjudicationsUseCase,
    };
    use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
    use crate::ports::pending_adjudication::{
        OrphanedQueuedClaim, PendingAdjudicationPort, PendingAdjudicationRow,
    };

    let agent = AgentId("conformance-sweep-then-affirm".into());

    // Incumbent I, live and CommittedCheap.
    let incumbent = make_claim(&agent, "sweep-affirm-subj", "sweep-affirm-pred");
    let ref_incumbent = incumbent.claim_ref().clone();
    let mut txn = store.begin_atomic(&agent).expect("swpaff: begin incumbent");
    store.append_claim(&mut txn, &incumbent).expect("swpaff: append incumbent");
    store
        .append_ledger_entry(&mut txn, &make_ledger_entry(&agent, &ref_incumbent))
        .expect("swpaff: incumbent ledger");
    store.commit(txn).expect("swpaff: commit incumbent");

    // Challenger #1: QueuedForAdjudication, EXPIRED pending row → sweep must only revert.
    let challenger1 = make_claim(&agent, "sweep-affirm-subj", "sweep-affirm-pred");
    let ref_challenger1 = challenger1.claim_ref().clone();
    let queued1 = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent.clone(), claim_ref: ref_challenger1.clone(), event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::QueuedForAdjudication, rationale: None, recorded_at: TransactionTime(Utc::now()) };
    let mut txn = store.begin_atomic(&agent).expect("swpaff: begin c1");
    store.append_claim(&mut txn, &challenger1).expect("swpaff: append c1");
    store.append_ledger_entry(&mut txn, &queued1).expect("swpaff: queued c1");
    store.commit(txn).expect("swpaff: commit c1");

    struct NoopPending;
    impl PendingAdjudicationPort for NoopPending {
        type Error = std::io::Error;
        fn insert_pending(&self, _row: &PendingAdjudicationRow) -> Result<(), Self::Error> { Ok(()) }
        fn get_pending(&self, _handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> { Ok(None) }
        fn list_pending(&self, _agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(vec![]) }
        fn list_expired(&self, _now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(vec![]) }
        fn mark_resolved(&self, _handle_id: uuid::Uuid) -> Result<(), Self::Error> { Ok(()) }
        fn mark_expired(&self, _handle_id: uuid::Uuid) -> Result<(), Self::Error> { Ok(()) }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> { Ok(vec![]) }
    }
    let erased_noop: Arc<dyn ErasedPendingStore> = Arc::new(ErasedPendingStoreAdapter::new(NoopPending));
    let sweep_uc = SweepAdjudicationsUseCase::new(Arc::clone(store), erased_noop);

    let row1 = PendingAdjudicationRow {
        handle_id: Uuid::new_v4(), agent_id: agent.clone(), subject: "sweep-affirm-subj".into(), predicate: "sweep-affirm-pred".into(),
        challenger_claim_ref: ref_challenger1.clone(), incumbent_claim_ref: ref_incumbent.clone(),
        request_payload: mempill_types::AdjudicationRequest {
            subject_line: mempill_types::SubjectLineRef { agent_id: agent.clone(), subject: "sweep-affirm-subj".into(), predicate: "sweep-affirm-pred".into() },
            incumbent: mempill_types::Belief { claim_ref: ref_incumbent.clone(), fact: incumbent.fact().clone(), provenance: incumbent.provenance().clone(), valid_time: incumbent.valid_time().clone(), transaction_time: incumbent.transaction_time().clone(), confidence: incumbent.confidence().clone(), currency_signal: mempill_types::CurrencySignal { last_refreshed_at: incumbent.transaction_time().clone(), state: mempill_types::CurrencyState::Fresh, corroboration_count: 0 }, criticality: incumbent.criticality().clone() },
            challenger: challenger1.clone(), criticality: Criticality::Medium, reason: mempill_types::OverturnReason::ExternalContradiction,
        },
        queued_at: Utc::now() - chrono::Duration::seconds(10), expires_at: Some(Utc::now() - chrono::Duration::seconds(1)), status: "pending".into(),
    };

    let reverted = sweep_uc.revert_expired_row(&row1, Utc::now()).expect("swpaff: revert_expired_row must not error");
    assert!(reverted, "swpaff: expired row must be reverted");

    // Challenger #1 must now be Contested — NEVER Superseded/CommittedCheap; and the
    // INCUMBENT must be untouched (still live, no Bound assertion, no edge).
    let ledger_c1 = store.load_ledger_for_claims(&agent, &[ref_challenger1.clone()], None).expect("swpaff: ledger c1");
    let latest_c1 = ledger_c1.iter().max_by_key(|e| e.recorded_at.0).expect("swpaff: c1 ledger non-empty");
    assert_eq!(latest_c1.disposition, Disposition::Contested, "swpaff: sweep must revert the challenger to Contested, never resolve it");
    let assertions_incumbent = store.load_validity_assertions_for(&agent, &ref_incumbent).expect("swpaff: incumbent assertions after sweep");
    assert!(assertions_incumbent.is_empty(), "swpaff: sweep must NEVER write a Bound (supersession) assertion on the incumbent");
    let edges_incumbent = store.load_edges_for(&agent, &ref_incumbent).expect("swpaff: incumbent edges after sweep");
    assert!(edges_incumbent.is_empty(), "swpaff: sweep must NEVER write a supersession edge on the incumbent");

    // Challenger #2: QueuedForAdjudication with a LIVE (non-expired) pending row —
    // submit_adjudication(Affirm) is the ONLY path that may supersede the incumbent.
    let challenger2 = make_claim(&agent, "sweep-affirm-subj", "sweep-affirm-pred");
    let ref_challenger2 = challenger2.claim_ref().clone();
    let queued2 = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent.clone(), claim_ref: ref_challenger2.clone(), event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::QueuedForAdjudication, rationale: None, recorded_at: TransactionTime(Utc::now()) };
    let mut txn = store.begin_atomic(&agent).expect("swpaff: begin c2");
    store.append_claim(&mut txn, &challenger2).expect("swpaff: append c2");
    store.append_ledger_entry(&mut txn, &queued2).expect("swpaff: queued c2");
    store.commit(txn).expect("swpaff: commit c2");

    let handle2 = Uuid::new_v4();
    struct StubPendingStore { rows: Mutex<Vec<PendingAdjudicationRow>> }
    impl PendingAdjudicationPort for StubPendingStore {
        type Error = std::io::Error;
        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error> { self.rows.lock().unwrap().push(row.clone()); Ok(()) }
        fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> { Ok(self.rows.lock().unwrap().iter().find(|r| r.handle_id == handle_id).cloned()) }
        fn list_pending(&self, _agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(self.rows.lock().unwrap().clone()) }
        fn list_expired(&self, _now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(vec![]) }
        fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> { for r in self.rows.lock().unwrap().iter_mut() { if r.handle_id == handle_id { r.status = "resolved".into(); } } Ok(()) }
        fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> { for r in self.rows.lock().unwrap().iter_mut() { if r.handle_id == handle_id { r.status = "expired".into(); } } Ok(()) }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> { Ok(vec![]) }
    }
    let dummy_req2 = mempill_types::AdjudicationRequest {
        subject_line: mempill_types::SubjectLineRef { agent_id: agent.clone(), subject: "sweep-affirm-subj".into(), predicate: "sweep-affirm-pred".into() },
        incumbent: mempill_types::Belief { claim_ref: ref_incumbent.clone(), fact: incumbent.fact().clone(), provenance: incumbent.provenance().clone(), valid_time: incumbent.valid_time().clone(), transaction_time: incumbent.transaction_time().clone(), confidence: incumbent.confidence().clone(), currency_signal: mempill_types::CurrencySignal { last_refreshed_at: incumbent.transaction_time().clone(), state: mempill_types::CurrencyState::Fresh, corroboration_count: 0 }, criticality: incumbent.criticality().clone() },
        challenger: challenger2.clone(), criticality: Criticality::Medium, reason: mempill_types::OverturnReason::ExternalContradiction,
    };
    let pending_store2 = Arc::new(StubPendingStore { rows: Mutex::new(vec![PendingAdjudicationRow {
        handle_id: handle2, agent_id: agent.clone(), subject: "sweep-affirm-subj".into(), predicate: "sweep-affirm-pred".into(),
        challenger_claim_ref: ref_challenger2.clone(), incumbent_claim_ref: ref_incumbent.clone(), request_payload: dummy_req2,
        queued_at: Utc::now(), expires_at: None, status: "pending".into(),
    }]) });
    struct Wrapper2(Arc<StubPendingStore>);
    impl PendingAdjudicationPort for Wrapper2 {
        type Error = std::io::Error;
        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error> { self.0.insert_pending(row) }
        fn get_pending(&self, h: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> { self.0.get_pending(h) }
        fn list_pending(&self, a: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { self.0.list_pending(a) }
        fn list_expired(&self, n: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { self.0.list_expired(n) }
        fn mark_resolved(&self, h: uuid::Uuid) -> Result<(), Self::Error> { self.0.mark_resolved(h) }
        fn mark_expired(&self, h: uuid::Uuid) -> Result<(), Self::Error> { self.0.mark_expired(h) }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> { self.0.list_queued_orphan_claims() }
    }
    let erased2: Arc<dyn ErasedPendingStore> = Arc::new(ErasedPendingStoreAdapter::new(Wrapper2(Arc::clone(&pending_store2))));
    let submit_uc = SubmitAdjudicationUseCase::new(Arc::clone(store), Arc::clone(&erased2));

    let response2 = mempill_types::AdjudicationResponse { handle_id: handle2, verdict: mempill_types::AdjudicationVerdict::Affirm, evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand) };
    let outcome2 = submit_uc.execute(handle2, response2, Utc::now()).expect("swpaff: submit_adjudication(Affirm) must not error");
    assert_eq!(outcome2.disposition, Disposition::CommittedCheap, "swpaff: Affirm must commit the challenger");

    // NOW, and ONLY now, the incumbent must be Superseded: Bound assertion present.
    let assertions_incumbent_after = store.load_validity_assertions_for(&agent, &ref_incumbent).expect("swpaff: incumbent assertions after affirm");
    assert!(
        assertions_incumbent_after.iter().any(|a| matches!(a.kind, mempill_types::AssertionKind::Bound { .. })),
        "swpaff: submit_adjudication(Affirm) must write the Bound (supersession) assertion on \
         the incumbent — the ONLY path that may"
    );
    let ledger_incumbent_after = store.load_ledger_for_claims(&agent, &[ref_incumbent.clone()], None).expect("swpaff: incumbent ledger after affirm");
    assert!(
        ledger_incumbent_after.iter().any(|e| e.event_kind == LedgerEventKind::ValidityAsserted && e.disposition == Disposition::Superseded),
        "swpaff: submit_adjudication(Affirm) must append a Superseded ValidityAsserted ledger row for the incumbent"
    );
}

// ── valid_at conformance harness ──────────────────────────────────────────────

/// Run the `valid_at` point-in-time query conformance suite against `store`.
///
/// Proves that both adapters return identical `BeliefProjection` results when
/// `valid_at` is set, and that the two bi-temporal axes compose per D2.
///
/// Scenario: CEO succession timeline with three slots —
///   Alice  CEO: valid [2020-01-01, 2022-01-01)  confidence=0.9
///   Bob    CEO: valid [2022-01-01, 2024-01-01)  confidence=0.9
///   Carol  CEO: valid [2024-01-01, ∞)            confidence=0.9
///
/// All claims are written with tx_time in the past so they are always
/// tx-visible for any as_of >= tx_time used in the tests.
///
/// Sub-tests:
///   va1: valid_at in Alice's window → returns Alice
///   va2: valid_at in Bob's window   → returns Bob
///   va3: valid_at in Carol's window → returns Carol
///   va4: valid_at in gap before window start → NoBelief (pre-history)
///   va5: D2 independence: as_of_tx_time=t_alice_tx (only Alice tx-visible), valid_at in Bob's
///        window → fold sees only Alice (tx filter) → window mismatch → NoBelief (gap)
///   va6: valid_at=None backward compat: as_of=now → selects Carol (open window)
#[cfg(any(test, feature = "test-support"))]
pub fn run_valid_at_conformance<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    valid_at_alice_window(store);
    valid_at_bob_window(store);
    valid_at_carol_window(store);
    valid_at_pre_history_gap(store);
    valid_at_d2_tx_filters_before_vt(store);
    valid_at_none_backward_compat(store);
}

/// Build a trusted high-confidence Claim with a specific valid-time window.
///
/// All claims in the valid_at harness use confidence=0.9 (above the 0.7 threshold)
/// so they qualify for succession instant-selection.
#[cfg(any(test, feature = "test-support"))]
fn make_vt_claim_for_conformance(
    agent: &AgentId,
    subject: &str,
    predicate: &str,
    value: serde_json::Value,
    tx_time: chrono::DateTime<Utc>,
    vt_start: chrono::DateTime<Utc>,
    vt_end: Option<chrono::DateTime<Utc>>,
) -> Claim {
    use mempill_types::claim::Criticality;
    Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact {
            subject: subject.to_owned(),
            predicate: predicate.to_owned(),
            value,
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx_time),
        ValidTime {
            start: Some(vt_start),
            end: vt_end,
            valid_time_confidence: 0.9,
            start_granularity: None, end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        Criticality::Medium,
        vec![],
        None,
        None,
    )
}

/// Helper: parse an RFC3339 string into a `DateTime<Utc>` — panics on bad input.
#[cfg(any(test, feature = "test-support"))]
fn vat_dt(rfc3339: &str) -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .unwrap_or_else(|e| panic!("vat_dt parse failed for '{rfc3339}': {e}"))
        .with_timezone(&Utc)
}

/// Helper: commit the three-slot CEO succession to the store and return the tx_time
/// used for Alice (earliest) so sub-tests can use it for D2 testing.
///
/// Returns `(t_alice, t_bob, t_carol)` tx-times written to the store.
#[cfg(any(test, feature = "test-support"))]
fn write_ceo_succession<P>(store: &P, agent: &AgentId) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>, chrono::DateTime<Utc>)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    // Use fixed, well-separated tx_times far in the past so they are always
    // tx-visible for the instants used in the tests.
    let t_alice = vat_dt("2019-06-01T00:00:00Z"); // written well before her valid window
    let t_bob   = vat_dt("2019-06-02T00:00:00Z"); // written a day later (still before any query)
    let t_carol = vat_dt("2019-06-03T00:00:00Z");

    let alice = make_vt_claim_for_conformance(
        agent, "corp", "ceo", serde_json::json!("alice"),
        t_alice,
        vat_dt("2020-01-01T00:00:00Z"),
        Some(vat_dt("2022-01-01T00:00:00Z")),
    );
    let bob = make_vt_claim_for_conformance(
        agent, "corp", "ceo", serde_json::json!("bob"),
        t_bob,
        vat_dt("2022-01-01T00:00:00Z"),
        Some(vat_dt("2024-01-01T00:00:00Z")),
    );
    let carol = make_vt_claim_for_conformance(
        agent, "corp", "ceo", serde_json::json!("carol"),
        t_carol,
        vat_dt("2024-01-01T00:00:00Z"),
        None, // open-ended: Carol → now
    );

    let mut txn = store.begin_atomic(agent).expect("valid_at[setup]: begin_atomic");
    store.append_claim(&mut txn, &alice).expect("valid_at[setup]: append alice");
    store.append_claim(&mut txn, &bob).expect("valid_at[setup]: append bob");
    store.append_claim(&mut txn, &carol).expect("valid_at[setup]: append carol");
    store.commit(txn).expect("valid_at[setup]: commit");

    (t_alice, t_bob, t_carol)
}

/// Sub-test va1: valid_at=2021-06-01 → in Alice's window [2020, 2022) → returns "alice".
#[cfg(any(test, feature = "test-support"))]
fn valid_at_alice_window<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use std::collections::HashMap;

    let agent = AgentId("valid-at-va1".into());
    write_ceo_succession(store, &agent);

    let as_of = vat_dt("2026-01-01T00:00:00Z"); // well after all tx_times
    let valid_at = vat_dt("2021-06-01T00:00:00Z"); // inside Alice's window [2020, 2022)

    let claims = store
        .load_subject_line(&agent, "corp", "ceo", None)
        .expect("va1: load_subject_line");
    assert_eq!(claims.len(), 3, "va1: must have 3 claims");

    let claim_refs: Vec<ClaimRef> = claims.iter().map(|c| c.claim_ref().clone()).collect();
    let ledger = store.load_ledger_for_claims(&agent, &claim_refs, None).expect("va1: load_ledger");
    let latest_disposition = build_latest_disposition_map(&ledger);

    let config = EngineConfig::default();
    let fold = truth_engine::fold(
        claims,
        |_| vec![],
        as_of,
        Some(valid_at),
        &config,
        &latest_disposition,
    );

    assert!(fold.succession_selected, "va1: trusted succession must be detected");
    assert_eq!(fold.live_claims.len(), 1, "va1: valid_at=2021-06 must select exactly 1 claim");
    assert_eq!(
        fold.live_claims[0].claim.fact().value,
        serde_json::json!("alice"),
        "va1: valid_at=2021-06 is in Alice's window [2020, 2022) → must return alice"
    );
    let _ = HashMap::<String, ()>::new(); // suppress unused import warning
}

/// Sub-test va2: valid_at=2023-01-01 → in Bob's window [2022, 2024) → returns "bob".
#[cfg(any(test, feature = "test-support"))]
fn valid_at_bob_window<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("valid-at-va2".into());
    write_ceo_succession(store, &agent);

    let as_of    = vat_dt("2026-01-01T00:00:00Z");
    let valid_at = vat_dt("2023-01-01T00:00:00Z"); // inside Bob's window [2022, 2024)

    let claims = store.load_subject_line(&agent, "corp", "ceo", None).expect("va2: load");
    let refs: Vec<ClaimRef> = claims.iter().map(|c| c.claim_ref().clone()).collect();
    let ledger = store.load_ledger_for_claims(&agent, &refs, None).expect("va2: ledger");
    let disp = build_latest_disposition_map(&ledger);

    let fold = truth_engine::fold(
        claims, |_| vec![], as_of, Some(valid_at), &EngineConfig::default(), &disp,
    );

    assert_eq!(fold.live_claims.len(), 1, "va2: valid_at=2023-01 selects 1 claim");
    assert_eq!(
        fold.live_claims[0].claim.fact().value,
        serde_json::json!("bob"),
        "va2: valid_at=2023-01 is in Bob's window [2022, 2024)"
    );
}

/// Sub-test va3: valid_at=2025-06-01 → in Carol's open window [2024, ∞) → returns "carol".
#[cfg(any(test, feature = "test-support"))]
fn valid_at_carol_window<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("valid-at-va3".into());
    write_ceo_succession(store, &agent);

    let as_of    = vat_dt("2026-01-01T00:00:00Z");
    let valid_at = vat_dt("2025-06-01T00:00:00Z"); // inside Carol's open window [2024, ∞)

    let claims = store.load_subject_line(&agent, "corp", "ceo", None).expect("va3: load");
    let refs: Vec<ClaimRef> = claims.iter().map(|c| c.claim_ref().clone()).collect();
    let ledger = store.load_ledger_for_claims(&agent, &refs, None).expect("va3: ledger");
    let disp = build_latest_disposition_map(&ledger);

    let fold = truth_engine::fold(
        claims, |_| vec![], as_of, Some(valid_at), &EngineConfig::default(), &disp,
    );

    assert_eq!(fold.live_claims.len(), 1, "va3: valid_at=2025-06 selects 1 claim");
    assert_eq!(
        fold.live_claims[0].claim.fact().value,
        serde_json::json!("carol"),
        "va3: valid_at=2025-06 is in Carol's open window [2024, ∞)"
    );
}

/// Sub-test va4: valid_at=2019-01-01 → before all valid windows → NoBelief (gap/pre-history).
#[cfg(any(test, feature = "test-support"))]
fn valid_at_pre_history_gap<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("valid-at-va4".into());
    write_ceo_succession(store, &agent);

    let as_of    = vat_dt("2026-01-01T00:00:00Z");
    let valid_at = vat_dt("2019-01-01T00:00:00Z"); // before Alice's window start 2020-01-01

    let claims = store.load_subject_line(&agent, "corp", "ceo", None).expect("va4: load");
    let refs: Vec<ClaimRef> = claims.iter().map(|c| c.claim_ref().clone()).collect();
    let ledger = store.load_ledger_for_claims(&agent, &refs, None).expect("va4: ledger");
    let disp = build_latest_disposition_map(&ledger);

    let fold = truth_engine::fold(
        claims, |_| vec![], as_of, Some(valid_at), &EngineConfig::default(), &disp,
    );

    // valid_at is before any window → gap → NoBelief (empty live_claims).
    assert!(
        fold.succession_selected,
        "va4: succession detection fires even when instant is pre-history"
    );
    assert_eq!(
        fold.live_claims.len(), 0,
        "va4: valid_at=2019-01 is before all windows → NoBelief (empty live_claims)"
    );
    assert!(!fold.has_conflict, "va4: gap must not produce has_conflict");
}

/// Sub-test va5 (D2 independence): `as_of_tx_time` governs ValidityAssertion visibility
/// independently of `valid_at` which governs instant-selection.
///
/// Scenario: two-claim succession: Alice [2020, 2022) + Bob [2022, ∞).
/// A Bound assertion is appended for Alice, with `asserted_at = 2030-01-01` (far future).
///
/// Query A: as_of = 2026-01-01 (before the 2030 Bound assertion) → Bound NOT visible →
///          Alice remains live. Succession: Alice [2020,2022) + Bob [2022,∞).
///          valid_at = 2021-01-01 (in Alice's window) → selects Alice.
///
/// Query B: as_of = 2031-01-01 (after the 2030 Bound assertion) → Bound IS visible →
///          Alice is bounded (not live). Only Bob is live.
///          valid_at = 2021-01-01 (same instant, now in a bounded claim's window) →
///          only Bob visible → single live claim → no succession selection → Bob returned.
///
/// D2 confirmed: the same valid_at value yields DIFFERENT results depending on as_of,
/// proving the tx-time axis (assertion visibility) composes before valid-time selection.
#[cfg(any(test, feature = "test-support"))]
fn valid_at_d2_tx_filters_before_vt<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;
    use uuid::Uuid;

    let agent = AgentId("valid-at-va5".into());

    // Fixed tx_time for all writes (well in the past so always tx-visible at any as_of we use).
    let tx = vat_dt("2019-06-01T00:00:00Z");

    // Alice: valid [2020, 2022), Bob: valid [2022, ∞).
    let alice = make_vt_claim_for_conformance(
        &agent, "va5-corp", "ceo", serde_json::json!("alice"),
        tx,
        vat_dt("2020-01-01T00:00:00Z"),
        Some(vat_dt("2022-01-01T00:00:00Z")),
    );
    let bob = make_vt_claim_for_conformance(
        &agent, "va5-corp", "ceo", serde_json::json!("bob"),
        tx,
        vat_dt("2022-01-01T00:00:00Z"),
        None,
    );
    let alice_ref = alice.claim_ref().clone();
    let bob_ref   = bob.claim_ref().clone();

    // Bound assertion for Alice: asserted_at = 2030-01-01 (far future relative to query A).
    // bound_at = 2030-01-01 (must be <= as_of for the claim to be considered bounded).
    let bound_at    = vat_dt("2030-01-01T00:00:00Z");
    let asserted_at = bound_at; // assertion recorded at the same time as the bound
    let bound = ValidityAssertion {
        assertion_ref: Uuid::new_v4(),
        agent_id: agent.clone(),
        target_claim: alice_ref.clone(),
        kind: AssertionKind::Bound { bound_at },
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
        asserted_at: TransactionTime(asserted_at),
    };

    // Write claims and the Bound assertion.
    let mut txn = store.begin_atomic(&agent).expect("va5: begin_atomic");
    store.append_claim(&mut txn, &alice).expect("va5: append alice");
    store.append_claim(&mut txn, &bob).expect("va5: append bob");
    store.append_validity_assertion(&mut txn, &bound).expect("va5: append bound");
    store.commit(txn).expect("va5: commit");

    let claims = store.load_subject_line(&agent, "va5-corp", "ceo", None).expect("va5: load");
    assert_eq!(claims.len(), 2, "va5: must have 2 claims");

    let refs: Vec<ClaimRef> = vec![alice_ref.clone(), bob_ref.clone()];
    let ledger = store.load_ledger_for_claims(&agent, &refs, None).expect("va5: ledger");
    let disp = build_latest_disposition_map(&ledger);
    let config = EngineConfig::default();

    // ── Query A: as_of = 2026-01-01 → Bound(2030) NOT visible → Alice live ────
    let as_of_a  = vat_dt("2026-01-01T00:00:00Z");
    let valid_at = vat_dt("2021-01-01T00:00:00Z"); // in Alice's window [2020, 2022)

    let assertions_fn_a = {
        let alice_ref = alice_ref.clone();
        let bound = bound.clone();
        move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if cr == &alice_ref { vec![bound.clone()] } else { vec![] }
        }
    };

    let fold_a = truth_engine::fold(
        claims.clone(), assertions_fn_a, as_of_a, Some(valid_at), &config, &disp,
    );

    // at as_of=2026: Bound asserted_at=2030 > 2026 → NOT visible → Alice is live.
    // Succession: Alice [2020,2022) + Bob [2022,∞). valid_at=2021 → Alice selected.
    assert_eq!(
        fold_a.live_claims.len(), 1,
        "va5 Query A: Bound not visible at as_of=2026 → Alice live; valid_at=2021 selects Alice"
    );
    assert_eq!(
        fold_a.live_claims[0].claim.fact().value,
        serde_json::json!("alice"),
        "va5 Query A: as_of=2026 (Bound invisible) + valid_at=2021 → Alice"
    );

    // ── Query B: as_of = 2031-01-01 → Bound(2030) visible → Alice bounded ────
    let as_of_b = vat_dt("2031-01-01T00:00:00Z");

    let assertions_fn_b = {
        let alice_ref = alice_ref.clone();
        move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if cr == &alice_ref { vec![bound.clone()] } else { vec![] }
        }
    };

    let fold_b = truth_engine::fold(
        claims, assertions_fn_b, as_of_b, Some(valid_at), &config, &disp,
    );

    // at as_of=2031: Bound asserted_at=2030 ≤ 2031 AND bound_at=2030 ≤ 2031 → visible → Alice bounded.
    // Only Bob is live (single claim, no succession). valid_at=2021 is not relevant to selection
    // (single-claim path skips instant-selection). Bob is the sole live claim.
    assert_eq!(
        fold_b.live_claims.len(), 1,
        "va5 Query B: Bound visible at as_of=2031 → Alice bounded; Bob is the only live claim"
    );
    assert_eq!(
        fold_b.live_claims[0].claim.fact().value,
        serde_json::json!("bob"),
        "va5 Query B: same valid_at=2021 but as_of=2031 makes Alice bounded → Bob returned (D2)"
    );

    println!(
        "[va5 D2] Query A (as_of=2026): {:?} | Query B (as_of=2031): {:?}",
        fold_a.live_claims[0].claim.fact().value,
        fold_b.live_claims[0].claim.fact().value
    );
}

/// Sub-test va6: valid_at=None backward compat.
///
/// When valid_at is None, as_of_tx_time (now) is used as the valid-time instant.
/// Both axes point to 2026 → Carol's open window [2024, ∞) → "carol".
/// This ensures the None path remains identical to pre-wave-2 behavior.
#[cfg(any(test, feature = "test-support"))]
fn valid_at_none_backward_compat<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::ingest_claim::build_latest_disposition_map;
    use crate::config::EngineConfig;
    use crate::engine::truth_engine;

    let agent = AgentId("valid-at-va6".into());
    write_ceo_succession(store, &agent);

    let as_of = vat_dt("2026-06-01T00:00:00Z"); // both tx-visible and vt-instant = 2026

    let claims = store.load_subject_line(&agent, "corp", "ceo", None).expect("va6: load");
    let refs: Vec<ClaimRef> = claims.iter().map(|c| c.claim_ref().clone()).collect();
    let ledger = store.load_ledger_for_claims(&agent, &refs, None).expect("va6: ledger");
    let disp = build_latest_disposition_map(&ledger);

    let fold = truth_engine::fold(
        claims, |_| vec![], as_of,
        None, // valid_at=None → backward-compat: as_of is used for both axes
        &EngineConfig::default(), &disp,
    );

    assert_eq!(fold.live_claims.len(), 1, "va6: None valid_at selects Carol (as_of=2026 in her window)");
    assert_eq!(
        fold.live_claims[0].claim.fact().value,
        serde_json::json!("carol"),
        "va6: backward compat — None valid_at with as_of=2026 → Carol's open window [2024, ∞)"
    );
}

// ── Granularity conformance harness ──────────────────────────────────────────

/// Run the `DateGranularity` persistence conformance suite against `store`.
///
/// Verifies that both adapters store and retrieve `start_granularity` / `end_granularity`
/// identically. Each scenario uses a distinct `AgentId` to avoid cross-contamination.
///
/// Scenarios:
///   gran1 — `start_granularity=Month`, open end  → exact values round-trip
///   gran2 — `start_granularity=Day`, `end_granularity=Year` → both round-trip
///   gran3 — both `None` (legacy / no granularity) → remain `None`
///
/// This harness complements the per-adapter `granularity_roundtrip` tests (W2/W3) by
/// proving cross-adapter behavioral parity through the same shared fixture path used by
/// the rest of the conformance suite.
#[cfg(any(test, feature = "test-support"))]
pub fn run_granularity_conformance<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    granularity_month_start_open_end(store);
    granularity_day_start_year_end(store);
    granularity_none_none_legacy(store);
}

/// gran1: `start_granularity=Month`, `end_granularity=None` round-trips.
#[cfg(any(test, feature = "test-support"))]
fn granularity_month_start_open_end<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use mempill_types::DateGranularity;

    let agent = AgentId("conformance-gran-t1".into());
    let tx = chrono::DateTime::<chrono::Utc>::from_timestamp(20_000_000, 0).unwrap();
    // "2020-03" parses to first-of-month midnight UTC.
    let start = chrono::DateTime::<chrono::Utc>::from_timestamp(1583020800, 0).unwrap(); // 2020-03-01

    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact {
            subject: "gran-person".into(),
            predicate: "birth_month".into(),
            value: serde_json::json!("test-value"),
        },
        mempill_types::claim::Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime {
            start: Some(start),
            end: None,
            valid_time_confidence: 0.9,
            start_granularity: Some(DateGranularity::Month),
            end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        mempill_types::claim::Criticality::Medium,
        vec![],
        None,
        None,
    );
    let claim_ref = claim.claim_ref().clone();

    let mut txn = store.begin_atomic(&agent).expect("conformance[gran-t1]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[gran-t1]: append_claim");
    store.commit(txn).expect("conformance[gran-t1]: commit");

    let loaded = store
        .load_claim(&agent, &claim_ref)
        .expect("conformance[gran-t1]: load_claim must not error")
        .expect("conformance[gran-t1]: claim must be present");

    assert_eq!(
        loaded.valid_time().start_granularity,
        Some(DateGranularity::Month),
        "conformance[gran-t1]: start_granularity must round-trip as Month; got {:?}",
        loaded.valid_time().start_granularity,
    );
    assert_eq!(
        loaded.valid_time().end_granularity,
        None,
        "conformance[gran-t1]: end_granularity must remain None for open end; got {:?}",
        loaded.valid_time().end_granularity,
    );
    assert_eq!(
        loaded.valid_time().start,
        Some(start),
        "conformance[gran-t1]: start datetime must be preserved"
    );
}

/// gran2: `start_granularity=Day`, `end_granularity=Year` both round-trip.
#[cfg(any(test, feature = "test-support"))]
fn granularity_day_start_year_end<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use mempill_types::DateGranularity;

    let agent = AgentId("conformance-gran-t2".into());
    let tx = chrono::DateTime::<chrono::Utc>::from_timestamp(20_000_001, 0).unwrap();
    let start = chrono::DateTime::<chrono::Utc>::from_timestamp(1560556800, 0).unwrap(); // 2019-06-15
    let end   = chrono::DateTime::<chrono::Utc>::from_timestamp(1672531200, 0).unwrap(); // 2023-01-01

    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact {
            subject: "gran-project".into(),
            predicate: "active_window".into(),
            value: serde_json::json!("test-value"),
        },
        mempill_types::claim::Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime {
            start: Some(start),
            end: Some(end),
            valid_time_confidence: 0.8,
            start_granularity: Some(DateGranularity::Day),
            end_granularity: Some(DateGranularity::Year),
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.8 },
        mempill_types::claim::Criticality::Low,
        vec![],
        None,
        None,
    );
    let claim_ref = claim.claim_ref().clone();

    let mut txn = store.begin_atomic(&agent).expect("conformance[gran-t2]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[gran-t2]: append_claim");
    store.commit(txn).expect("conformance[gran-t2]: commit");

    let loaded = store
        .load_claim(&agent, &claim_ref)
        .expect("conformance[gran-t2]: load_claim must not error")
        .expect("conformance[gran-t2]: claim must be present");

    assert_eq!(
        loaded.valid_time().start_granularity,
        Some(DateGranularity::Day),
        "conformance[gran-t2]: start_granularity must round-trip as Day; got {:?}",
        loaded.valid_time().start_granularity,
    );
    assert_eq!(
        loaded.valid_time().end_granularity,
        Some(DateGranularity::Year),
        "conformance[gran-t2]: end_granularity must round-trip as Year; got {:?}",
        loaded.valid_time().end_granularity,
    );
}

/// gran3: both granularities `None` (legacy rows) remain `None` after round-trip.
#[cfg(any(test, feature = "test-support"))]
fn granularity_none_none_legacy<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-gran-t3".into());
    let tx = chrono::DateTime::<chrono::Utc>::from_timestamp(20_000_002, 0).unwrap();

    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact {
            subject: "gran-legacy".into(),
            predicate: "event_time".into(),
            value: serde_json::json!("test-value"),
        },
        mempill_types::claim::Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime {
            start: None,
            end: None,
            valid_time_confidence: 0.0,
            start_granularity: None,
            end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        mempill_types::claim::Criticality::Low,
        vec![],
        None,
        None,
    );
    let claim_ref = claim.claim_ref().clone();

    let mut txn = store.begin_atomic(&agent).expect("conformance[gran-t3]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[gran-t3]: append_claim");
    store.commit(txn).expect("conformance[gran-t3]: commit");

    let loaded = store
        .load_claim(&agent, &claim_ref)
        .expect("conformance[gran-t3]: load_claim must not error")
        .expect("conformance[gran-t3]: claim must be present");

    assert_eq!(
        loaded.valid_time().start_granularity,
        None,
        "conformance[gran-t3]: start_granularity must remain None for legacy rows"
    );
    assert_eq!(
        loaded.valid_time().end_granularity,
        None,
        "conformance[gran-t3]: end_granularity must remain None for legacy rows"
    );
}

// ── History granularity conformance harness (TASK-32) ─────────────────────────

/// Run the `HistoryEntry` granularity/derived-endpoint conformance suite against `store`.
///
/// Exercises `QueryHistoryUseCase::execute_with_time` end-to-end (real persistence +
/// real fold), proving that `valid_from_granularity` / `valid_until_granularity` round-trip
/// honestly across both adapters, including the derived-endpoint rule for `valid_until`
/// (successor's `start_granularity`, or `None` when the successor's ordering key falls
/// back to `transaction_time`).
///
/// Requires `Arc<P>` (not `&P`) because `QueryHistoryUseCase::new` takes ownership of an
/// `Arc` — mirrors `run_disposition_scope_conformance`'s calling convention.
#[cfg(any(test, feature = "test-support"))]
pub fn run_history_granularity_conformance<P>(store: &std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    hist_gran_mixed_precision_timeline(std::sync::Arc::clone(store));
    hist_gran_supersession_uses_successor_granularity(std::sync::Arc::clone(store));
    hist_gran_legacy_none_granularity_row(std::sync::Arc::clone(store));
}

/// hist-gran-t1: a single claim with `start_granularity=Month` reports that granularity
/// verbatim on `valid_from_granularity`; being the only (open-ended) entry,
/// `valid_until_granularity` must be `None`.
#[cfg(any(test, feature = "test-support"))]
fn hist_gran_mixed_precision_timeline<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::query_history::QueryHistoryUseCase;
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;
    use mempill_types::DateGranularity;

    let agent = AgentId("conformance-histgran-t1".into());
    let tx = chrono::DateTime::<chrono::Utc>::from_timestamp(30_000_000, 0).unwrap();
    // "2020-03" → first-of-month midnight UTC.
    let start = chrono::DateTime::<chrono::Utc>::from_timestamp(1583020800, 0).unwrap();

    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact {
            subject: "histgran-person".into(),
            predicate: "birth_month".into(),
            value: serde_json::json!("test-value"),
        },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime {
            start: Some(start),
            end: None,
            valid_time_confidence: 0.9,
            start_granularity: Some(DateGranularity::Month),
            end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );

    let mut txn = store.begin_atomic(&agent).expect("conformance[histgran-t1]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[histgran-t1]: append_claim");
    store.commit(txn).expect("conformance[histgran-t1]: commit");

    let uc = QueryHistoryUseCase::new(store, None::<std::sync::Arc<NoOpVector>>, EngineConfig::default());
    let resp = uc
        .execute_with_time(
            crate::application::dto::QueryHistoryRequest {
                agent_id: agent,
                subject: "histgran-person".into(),
                predicate: "birth_month".into(),
            },
            chrono::DateTime::<chrono::Utc>::from_timestamp(40_000_000, 0).unwrap(),
        )
        .expect("conformance[histgran-t1]: execute_with_time must not error");

    assert_eq!(resp.entries.len(), 1, "conformance[histgran-t1]: one claim → one entry");
    assert_eq!(
        resp.entries[0].valid_from_granularity,
        Some(DateGranularity::Month),
        "conformance[histgran-t1]: valid_from_granularity must be the claim's own start_granularity"
    );
    assert_eq!(
        resp.entries[0].valid_until_granularity, None,
        "conformance[histgran-t1]: open-ended (only) entry must have None valid_until_granularity"
    );
}

/// hist-gran-t2: two-claim timeline where the predecessor is UNTRUSTED (confidence below
/// threshold, no own end) and the successor has `start_granularity=Year` and high valid-time
/// confidence. Since overlap cannot be determined when one side is untrusted, the legacy
/// successor-key fallback fires: the predecessor's `valid_until_granularity` must equal the
/// SUCCESSOR's `start_granularity` (Year) — not the predecessor's own `end_granularity`
/// (which is deliberately set to a different value, Day, to prove no cross-contamination).
/// (Two mutually-TRUSTED open-ended claims would instead be a genuine valid-time overlap —
/// Contested, no narrowing — under the fold-derived history design; this scenario requires
/// one side untrusted to honestly exercise the fallback path.)
#[cfg(any(test, feature = "test-support"))]
fn hist_gran_supersession_uses_successor_granularity<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::query_history::QueryHistoryUseCase;
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;
    use mempill_types::DateGranularity;

    let agent = AgentId("conformance-histgran-t2".into());

    let tx_old = chrono::DateTime::<chrono::Utc>::from_timestamp(30_000_100, 0).unwrap();
    let tx_new = chrono::DateTime::<chrono::Utc>::from_timestamp(30_000_200, 0).unwrap();
    let vt_old_start = chrono::DateTime::<chrono::Utc>::from_timestamp(1560556800, 0).unwrap(); // 2019-06-15
    let vt_new_start = chrono::DateTime::<chrono::Utc>::from_timestamp(1577836800, 0).unwrap(); // 2020-01-01

    let old_claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "histgran-corp".into(), predicate: "ceo".into(), value: serde_json::json!("Alice") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx_old),
        ValidTime {
            start: Some(vt_old_start),
            end: None,
            // UNTRUSTED (below the 0.7 default threshold): overlap against the successor
            // cannot be determined, so the legacy successor-key fallback fires below.
            valid_time_confidence: 0.3,
            start_granularity: Some(DateGranularity::Day),
            // Deliberately Day (not Year) to prove the effective valid_until_granularity
            // is NOT this claim's own end_granularity.
            end_granularity: Some(DateGranularity::Day),
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.3 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );

    let new_claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "histgran-corp".into(), predicate: "ceo".into(), value: serde_json::json!("Bob") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx_new),
        ValidTime {
            start: Some(vt_new_start),
            end: None,
            valid_time_confidence: 0.9,
            start_granularity: Some(DateGranularity::Year),
            end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
        Criticality::Medium,
        vec![],
        None,
        None,
    );

    let mut txn = store.begin_atomic(&agent).expect("conformance[histgran-t2]: begin_atomic");
    store.append_claim(&mut txn, &old_claim).expect("conformance[histgran-t2]: append old_claim");
    store.append_claim(&mut txn, &new_claim).expect("conformance[histgran-t2]: append new_claim");
    store.commit(txn).expect("conformance[histgran-t2]: commit");

    let uc = QueryHistoryUseCase::new(store, None::<std::sync::Arc<NoOpVector>>, EngineConfig::default());
    let resp = uc
        .execute_with_time(
            crate::application::dto::QueryHistoryRequest {
                agent_id: agent,
                subject: "histgran-corp".into(),
                predicate: "ceo".into(),
            },
            chrono::DateTime::<chrono::Utc>::from_timestamp(40_000_000, 0).unwrap(),
        )
        .expect("conformance[histgran-t2]: execute_with_time must not error");

    assert_eq!(resp.entries.len(), 2, "conformance[histgran-t2]: two claims → two entries");
    assert_eq!(resp.entries[0].value, serde_json::json!("Alice"), "oldest first");
    assert_eq!(resp.entries[1].value, serde_json::json!("Bob"), "newer second");

    assert_eq!(
        resp.entries[0].valid_until_granularity,
        Some(DateGranularity::Year),
        "conformance[histgran-t2]: predecessor's valid_until_granularity must be the \
         SUCCESSOR's start_granularity (Year), not the predecessor's own end_granularity (Day)"
    );
    assert_eq!(
        resp.entries[1].valid_until_granularity, None,
        "conformance[histgran-t2]: last (open-ended) entry must have None valid_until_granularity"
    );
    assert_eq!(
        resp.entries[1].valid_from_granularity,
        Some(DateGranularity::Year),
        "conformance[histgran-t2]: successor's own valid_from_granularity must be Year"
    );
}

/// hist-gran-t3: legacy row with no granularity tracked (`start_granularity=None`,
/// `end_granularity=None`) — both `HistoryEntry` granularity fields must remain `None`,
/// never fabricated.
#[cfg(any(test, feature = "test-support"))]
fn hist_gran_legacy_none_granularity_row<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::query_history::QueryHistoryUseCase;
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;

    let agent = AgentId("conformance-histgran-t3".into());
    let tx = chrono::DateTime::<chrono::Utc>::from_timestamp(30_000_300, 0).unwrap();

    let claim = Claim::new(
        ClaimRef::new_random(),
        agent.clone(),
        Fact { subject: "histgran-legacy".into(), predicate: "event_time".into(), value: serde_json::json!("test-value") },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx),
        ValidTime {
            start: None,
            end: None,
            valid_time_confidence: 0.0,
            start_granularity: None,
            end_granularity: None,
        },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    );

    let mut txn = store.begin_atomic(&agent).expect("conformance[histgran-t3]: begin_atomic");
    store.append_claim(&mut txn, &claim).expect("conformance[histgran-t3]: append_claim");
    store.commit(txn).expect("conformance[histgran-t3]: commit");

    let uc = QueryHistoryUseCase::new(store, None::<std::sync::Arc<NoOpVector>>, EngineConfig::default());
    let resp = uc
        .execute_with_time(
            crate::application::dto::QueryHistoryRequest {
                agent_id: agent,
                subject: "histgran-legacy".into(),
                predicate: "event_time".into(),
            },
            chrono::DateTime::<chrono::Utc>::from_timestamp(40_000_000, 0).unwrap(),
        )
        .expect("conformance[histgran-t3]: execute_with_time must not error");

    assert_eq!(resp.entries.len(), 1);
    assert_eq!(
        resp.entries[0].valid_from_granularity, None,
        "conformance[histgran-t3]: legacy row must report None valid_from_granularity"
    );
    assert_eq!(
        resp.entries[0].valid_until_granularity, None,
        "conformance[histgran-t3]: legacy row (open-ended) must report None valid_until_granularity"
    );
}

// ── Cross-agent scope-isolation conformance harness (TASK-33 / QA-A) ──────────
//
// Proves that two agents (A and B) sharing ONE store never see or affect each
// other's data through any application-layer use-case. Uses value-level
// assertions (exact sets/values), not just counts — a count-only assertion
// would not catch e.g. B's claim leaking INTO A's result set replacing one of
// A's own entries.
//
// A failing test in this harness is a publish-blocking cross-tenant data leak
// or corruption bug in the engine — it must NEVER be "fixed" by loosening the
// assertion. Report it loudly instead.

/// Run the full cross-agent isolation conformance suite against `store`.
#[cfg(any(test, feature = "test-support"))]
pub fn run_agent_isolation_conformance<P>(store: &std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    iso_ingest_claim_incumbent_not_shared(std::sync::Arc::clone(store));
    iso_query_memory_scoped(std::sync::Arc::clone(store));
    iso_query_history_scoped(std::sync::Arc::clone(store));
    iso_query_subject_scoped(std::sync::Arc::clone(store));
    iso_query_ledger_scoped(std::sync::Arc::clone(store));
    iso_reconcile_does_not_touch_other_agent(std::sync::Arc::clone(store));
    iso_submit_adjudication_unknown_handle_is_clean_error(std::sync::Arc::clone(store));
    iso_sweep_scoped_to_single_row(std::sync::Arc::clone(store));
}

/// iso-a: `ingest_claim` — A's incumbent/conflict classification is unaffected by B
/// having a conflicting claim on the identical (subject, predicate). A ingests X=1,
/// B ingests X=2 on the SAME subject-line name — both must be treated as fresh
/// (no incumbent), i.e. both `CommittedCheap`, neither `Contested`.
#[cfg(any(test, feature = "test-support"))]
fn iso_ingest_claim_incumbent_not_shared<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{dto::IngestClaimRequest, ingest_claim::IngestClaimUseCase};
    use crate::config::EngineConfig;
    use crate::noop::NoOpOracle;

    let agent_a = AgentId("iso-agent-a".into());
    let agent_b = AgentId("iso-agent-b".into());
    let config = EngineConfig::default();

    let uc = IngestClaimUseCase::new(std::sync::Arc::clone(&store), None::<std::sync::Arc<NoOpOracle>>, None, config);

    let req_a = IngestClaimRequest {
        agent_id: agent_a.clone(),
        subject: "iso-subject".into(),
        predicate: "iso-pred".into(),
        value: serde_json::json!(1),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Low,
        derived_from: vec![],
    };
    let resp_a = uc.execute(req_a).expect("iso[a]: A's ingest must not error");
    assert_eq!(
        resp_a.disposition,
        Disposition::CommittedCheap,
        "ISOLATION DEFECT: A's fresh claim on subject-line 'iso-subject'/'iso-pred' must be \
         CommittedCheap (no incumbent yet for A); got {:?}",
        resp_a.disposition
    );
    assert!(resp_a.contested_with.is_empty(), "ISOLATION DEFECT: A's fresh ingest must not report contested_with");

    // B ingests a CONFLICTING value on the identical subject-line NAME. If the engine
    // were incorrectly scoping incumbent lookup across agents, B would see A's claim
    // as its own incumbent and report Contested.
    let req_b = IngestClaimRequest {
        agent_id: agent_b.clone(),
        subject: "iso-subject".into(),
        predicate: "iso-pred".into(),
        value: serde_json::json!(2),
        provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
        cardinality: Cardinality::Functional,
        valid_time: None,
        confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        criticality: Criticality::Low,
        derived_from: vec![],
    };
    let resp_b = uc.execute(req_b).expect("iso[a]: B's ingest must not error");
    assert_eq!(
        resp_b.disposition,
        Disposition::CommittedCheap,
        "ISOLATION DEFECT: B's fresh claim on the SAME subject-line name as A's must still be \
         CommittedCheap — A's claim must never be treated as B's incumbent; got {:?} contested_with={:?}",
        resp_b.disposition, resp_b.contested_with
    );
    assert!(
        resp_b.contested_with.is_empty(),
        "ISOLATION DEFECT: B's ingest reported contested_with={:?} — B must never be contested \
         against A's claim_ref {:?}",
        resp_b.contested_with, resp_a.claim_ref
    );
    assert_ne!(resp_a.claim_ref, resp_b.claim_ref, "iso[a]: A and B must get distinct claim_refs");
}

/// iso-b: `query_memory` — A sees only A's belief, including the `as_of_tx_time` and
/// `valid_at` variants.
#[cfg(any(test, feature = "test-support"))]
fn iso_query_memory_scoped<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{dto::QueryMemoryRequest, query_memory::QueryMemoryUseCase};
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;
    use mempill_types::BeliefStatus;

    let agent_a = AgentId("iso-agent-a-qm".into());
    let agent_b = AgentId("iso-agent-b-qm".into());
    let tx = chrono::Utc::now();

    let claim_a = make_vt_free_claim(&agent_a, "iso-qm-subj", "iso-qm-pred", serde_json::json!("A-value"), tx);
    let claim_b = make_vt_free_claim(&agent_b, "iso-qm-subj", "iso-qm-pred", serde_json::json!("B-value"), tx);

    let mut txn = store.begin_atomic(&agent_a).expect("iso[b]: begin A");
    store.append_claim(&mut txn, &claim_a).expect("iso[b]: append A");
    store.commit(txn).expect("iso[b]: commit A");
    let mut txn = store.begin_atomic(&agent_b).expect("iso[b]: begin B");
    store.append_claim(&mut txn, &claim_b).expect("iso[b]: append B");
    store.commit(txn).expect("iso[b]: commit B");

    let config = EngineConfig::default();
    let uc = QueryMemoryUseCase::new(std::sync::Arc::clone(&store), None::<std::sync::Arc<NoOpVector>>, config);
    let now = chrono::Utc::now();

    for (agent, expected_value) in [(&agent_a, "A-value"), (&agent_b, "B-value")] {
        // Plain (backward-compat) query.
        let resp = uc
            .execute_with_time(
                QueryMemoryRequest { agent_id: agent.clone(), subject: "iso-qm-subj".into(), predicate: "iso-qm-pred".into(), as_of_tx_time: None, valid_at: None },
                now,
            )
            .expect("iso[b]: query_memory must not error");
        // Claims here carry no explicit valid_time, so a single live claim legitimately
        // projects as TimingUncertain (not Resolved) per the documented query_memory
        // contract — assert on VALUE isolation, not the exact status label.
        assert!(
            matches!(resp.belief.status, BeliefStatus::Resolved | BeliefStatus::TimingUncertain),
            "ISOLATION DEFECT: {agent:?} must resolve to a single belief (Resolved or TimingUncertain), \
             not Contested/NoBelief — cross-agent leak suspected; got {:?}", resp.belief.status
        );
        let primary = resp.belief.primary.as_ref().expect("iso[b]: primary must be present");
        assert_eq!(primary.fact.value, serde_json::json!(expected_value), "ISOLATION DEFECT: {agent:?}'s query_memory returned the wrong value — got {:?}, expected {expected_value:?}", primary.fact.value);

        // as_of_tx_time variant.
        let resp_asof = uc
            .execute_with_time(
                QueryMemoryRequest { agent_id: agent.clone(), subject: "iso-qm-subj".into(), predicate: "iso-qm-pred".into(), as_of_tx_time: Some(now), valid_at: None },
                now,
            )
            .expect("iso[b]: query_memory as_of_tx_time must not error");
        assert_eq!(resp_asof.belief.primary.as_ref().expect("primary").fact.value, serde_json::json!(expected_value), "ISOLATION DEFECT: {agent:?}'s as_of_tx_time query leaked cross-agent data");

        // valid_at variant.
        let resp_valid_at = uc
            .execute_with_time(
                QueryMemoryRequest { agent_id: agent.clone(), subject: "iso-qm-subj".into(), predicate: "iso-qm-pred".into(), as_of_tx_time: None, valid_at: Some(now) },
                now,
            )
            .expect("iso[b]: query_memory valid_at must not error");
        // valid_at with no valid_time set on either claim → NoBelief is the documented D2
        // gap semantics for claims without an explicit window; skip value assertion, only
        // assert no cross-agent VALUE leak if a primary IS surfaced.
        if let Some(primary) = resp_valid_at.belief.primary.as_ref() {
            assert_eq!(primary.fact.value, serde_json::json!(expected_value), "ISOLATION DEFECT: {agent:?}'s valid_at query leaked cross-agent data");
        }
    }
}

/// iso-c: `query_history` — A's timeline excludes B's claims entirely.
#[cfg(any(test, feature = "test-support"))]
fn iso_query_history_scoped<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{dto::QueryHistoryRequest, query_history::QueryHistoryUseCase};
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;

    let agent_a = AgentId("iso-agent-a-hist".into());
    let agent_b = AgentId("iso-agent-b-hist".into());
    let t1 = chrono::Utc::now();
    let t2 = t1 + chrono::Duration::seconds(1);

    let a1 = make_vt_free_claim(&agent_a, "iso-hist-subj", "iso-hist-pred", serde_json::json!("A1"), t1);
    let a2 = make_vt_free_claim(&agent_a, "iso-hist-subj", "iso-hist-pred", serde_json::json!("A2"), t2);
    let b1 = make_vt_free_claim(&agent_b, "iso-hist-subj", "iso-hist-pred", serde_json::json!("B1"), t1);

    let ref_a1 = a1.claim_ref().clone();
    let ref_a2 = a2.claim_ref().clone();
    let ref_b1 = b1.claim_ref().clone();

    let mut txn = store.begin_atomic(&agent_a).expect("iso[c]: begin A");
    store.append_claim(&mut txn, &a1).expect("iso[c]: append a1");
    store.append_claim(&mut txn, &a2).expect("iso[c]: append a2");
    store.commit(txn).expect("iso[c]: commit A");
    let mut txn = store.begin_atomic(&agent_b).expect("iso[c]: begin B");
    store.append_claim(&mut txn, &b1).expect("iso[c]: append b1");
    store.commit(txn).expect("iso[c]: commit B");

    let uc = QueryHistoryUseCase::new(std::sync::Arc::clone(&store), None::<std::sync::Arc<NoOpVector>>, EngineConfig::default());
    let now = t2 + chrono::Duration::seconds(1);

    let resp_a = uc
        .execute_with_time(QueryHistoryRequest { agent_id: agent_a, subject: "iso-hist-subj".into(), predicate: "iso-hist-pred".into() }, now)
        .expect("iso[c]: A history must not error");
    let a_refs: std::collections::HashSet<_> = resp_a.entries.iter().map(|e| e.claim_ref.clone()).collect();
    assert_eq!(a_refs, std::collections::HashSet::from([ref_a1, ref_a2]), "ISOLATION DEFECT: A's query_history entry set must be exactly {{A1, A2}} — got {a_refs:?}");
    assert!(!a_refs.contains(&ref_b1), "ISOLATION DEFECT: A's query_history leaked B's claim_ref {ref_b1:?}");

    let resp_b = uc
        .execute_with_time(QueryHistoryRequest { agent_id: agent_b, subject: "iso-hist-subj".into(), predicate: "iso-hist-pred".into() }, now)
        .expect("iso[c]: B history must not error");
    let b_refs: std::collections::HashSet<_> = resp_b.entries.iter().map(|e| e.claim_ref.clone()).collect();
    assert_eq!(b_refs, std::collections::HashSet::from([ref_b1]), "ISOLATION DEFECT: B's query_history entry set must be exactly {{B1}} — got {b_refs:?}");
}

/// iso-d: `query_subject` — A's predicate list excludes B's predicates on the same
/// subject name, even when B has an EXTRA predicate A never wrote.
#[cfg(any(test, feature = "test-support"))]
fn iso_query_subject_scoped<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{dto::QuerySubjectRequest, query_subject::QuerySubjectUseCase};
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;

    let agent_a = AgentId("iso-agent-a-qs".into());
    let agent_b = AgentId("iso-agent-b-qs".into());
    let tx = chrono::Utc::now();

    let a_shared = make_vt_free_claim(&agent_a, "iso-qs-subj", "iso-qs-shared", serde_json::json!("A-shared"), tx);
    let b_shared = make_vt_free_claim(&agent_b, "iso-qs-subj", "iso-qs-shared", serde_json::json!("B-shared"), tx);
    let b_only = make_vt_free_claim(&agent_b, "iso-qs-subj", "iso-qs-b-only", serde_json::json!("B-only"), tx);

    let mut txn = store.begin_atomic(&agent_a).expect("iso[d]: begin A");
    store.append_claim(&mut txn, &a_shared).expect("iso[d]: append a_shared");
    store.commit(txn).expect("iso[d]: commit A");
    let mut txn = store.begin_atomic(&agent_b).expect("iso[d]: begin B");
    store.append_claim(&mut txn, &b_shared).expect("iso[d]: append b_shared");
    store.append_claim(&mut txn, &b_only).expect("iso[d]: append b_only");
    store.commit(txn).expect("iso[d]: commit B");

    let uc = QuerySubjectUseCase::new(std::sync::Arc::clone(&store), None::<std::sync::Arc<NoOpVector>>, EngineConfig::default());
    let now = chrono::Utc::now();

    let resp_a = uc
        .execute_with_time(QuerySubjectRequest { agent_id: agent_a, subject: "iso-qs-subj".into(), valid_at: None, as_of_tx_time: None }, now)
        .expect("iso[d]: A query_subject must not error");
    let a_preds: std::collections::HashSet<_> = resp_a.entries.iter().map(|e| e.predicate.clone()).collect();
    assert_eq!(a_preds, std::collections::HashSet::from(["iso-qs-shared".to_string()]), "ISOLATION DEFECT: A's query_subject predicate set must be exactly {{iso-qs-shared}} — got {a_preds:?} (B's 'iso-qs-b-only' must never leak into A)");
    let a_shared_entry = resp_a.entries.iter().find(|e| e.predicate == "iso-qs-shared").expect("iso[d]: A shared entry");
    assert_eq!(a_shared_entry.value.as_deref(), Some("A-shared"), "ISOLATION DEFECT: A's shared-predicate value must be A's own, not B's; got {:?}", a_shared_entry.value);

    let resp_b = uc
        .execute_with_time(QuerySubjectRequest { agent_id: agent_b, subject: "iso-qs-subj".into(), valid_at: None, as_of_tx_time: None }, now)
        .expect("iso[d]: B query_subject must not error");
    let b_preds: std::collections::HashSet<_> = resp_b.entries.iter().map(|e| e.predicate.clone()).collect();
    assert_eq!(b_preds, std::collections::HashSet::from(["iso-qs-shared".to_string(), "iso-qs-b-only".to_string()]), "iso[d]: B's predicate set must be exactly {{iso-qs-shared, iso-qs-b-only}} — got {b_preds:?}");
}

/// iso-e: `query_ledger` (`AuditUseCase`) — A's ledger (full AND claim-scoped) has ZERO
/// B entries. Also proves that auditing under agent B while passing A's real `claim_ref`
/// does not leak A's ledger rows (the scoping must be agent_id-AND-claim_ref, not
/// claim_ref alone).
#[cfg(any(test, feature = "test-support"))]
fn iso_query_ledger_scoped<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{audit::AuditUseCase, dto::AuditQueryRequest};

    let agent_a = AgentId("iso-agent-a-ledger".into());
    let agent_b = AgentId("iso-agent-b-ledger".into());

    let claim_a = make_claim(&agent_a, "iso-ledger-subj", "iso-ledger-pred");
    let claim_b = make_claim(&agent_b, "iso-ledger-subj", "iso-ledger-pred");
    let ref_a = claim_a.claim_ref().clone();
    let ref_b = claim_b.claim_ref().clone();
    let ledger_a = make_ledger_entry(&agent_a, &ref_a);
    let ledger_b = make_ledger_entry(&agent_b, &ref_b);

    let mut txn = store.begin_atomic(&agent_a).expect("iso[e]: begin A");
    store.append_claim(&mut txn, &claim_a).expect("iso[e]: append claim_a");
    store.append_ledger_entry(&mut txn, &ledger_a).expect("iso[e]: append ledger_a");
    store.commit(txn).expect("iso[e]: commit A");
    let mut txn = store.begin_atomic(&agent_b).expect("iso[e]: begin B");
    store.append_claim(&mut txn, &claim_b).expect("iso[e]: append claim_b");
    store.append_ledger_entry(&mut txn, &ledger_b).expect("iso[e]: append ledger_b");
    store.commit(txn).expect("iso[e]: commit B");

    let uc = AuditUseCase::new(std::sync::Arc::clone(&store));

    // Full-agent ledger query for A: must contain ONLY A's entries.
    let resp_a_full = uc
        .execute(AuditQueryRequest { agent_id: agent_a.clone(), claim_ref: None, from_tx_time: None, limit: 1000 })
        .expect("iso[e]: A full ledger query must not error");
    assert!(resp_a_full.entries.iter().all(|e| e.claim_ref != ref_b), "ISOLATION DEFECT: A's full ledger query returned B's claim_ref {ref_b:?}");
    assert!(resp_a_full.entries.iter().any(|e| e.claim_ref == ref_a), "iso[e]: A's full ledger query must contain A's own entry");

    // Claim-scoped query for A, requesting exactly ref_a: must not include ref_b.
    let resp_a_scoped = uc
        .execute(AuditQueryRequest { agent_id: agent_a.clone(), claim_ref: Some(ref_a.clone()), from_tx_time: None, limit: 1000 })
        .expect("iso[e]: A claim-scoped ledger query must not error");
    assert!(resp_a_scoped.entries.iter().all(|e| e.claim_ref == ref_a), "ISOLATION DEFECT: A's claim-scoped ledger query returned entries for a different claim_ref");

    // CRITICAL cross-tenant probe: audit under agent_id=B but pass A's REAL claim_ref.
    // If the adapter scopes by claim_ref alone (ignoring agent_id), this would leak A's
    // ledger rows to a caller impersonating/auditing as B.
    let resp_b_with_a_ref = uc
        .execute(AuditQueryRequest { agent_id: agent_b.clone(), claim_ref: Some(ref_a.clone()), from_tx_time: None, limit: 1000 })
        .expect("iso[e]: B-scoped query with A's claim_ref must not error");
    assert!(
        resp_b_with_a_ref.entries.is_empty(),
        "ISOLATION DEFECT: querying the ledger as agent B while passing A's real claim_ref {ref_a:?} \
         returned {} entries — the adapter is scoping load_ledger_for_claims by claim_ref alone, \
         not (agent_id AND claim_ref). This is a cross-tenant ledger leak.",
        resp_b_with_a_ref.entries.len()
    );
}

/// iso-f: `reconcile` — A's reconcile pass does not touch B's existing Contested state.
#[cfg(any(test, feature = "test-support"))]
fn iso_reconcile_does_not_touch_other_agent<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{audit::AuditUseCase, dto::{AuditQueryRequest, ReconcileRequest}, reconcile::ReconcileUseCase};
    use crate::config::EngineConfig;
    use crate::noop::NoOpOracle;

    let agent_a = AgentId("iso-agent-a-recon".into());
    let agent_b = AgentId("iso-agent-b-recon".into());
    let tx = chrono::Utc::now();

    // B: two claims already marked Contested directly (simulates pre-existing contested state).
    let b1 = make_vt_free_claim(&agent_b, "iso-recon-subj", "iso-recon-pred", serde_json::json!("B1"), tx);
    let b2 = make_vt_free_claim(&agent_b, "iso-recon-subj", "iso-recon-pred", serde_json::json!("B2"), tx + chrono::Duration::seconds(1));
    let ref_b1 = b1.claim_ref().clone();
    let ref_b2 = b2.claim_ref().clone();
    let contested_b1 = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_b.clone(), claim_ref: ref_b1.clone(), event_kind: LedgerEventKind::ValidityAsserted, disposition: Disposition::Contested, rationale: None, recorded_at: TransactionTime(tx) };
    let contested_b2 = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_b.clone(), claim_ref: ref_b2.clone(), event_kind: LedgerEventKind::ValidityAsserted, disposition: Disposition::Contested, rationale: None, recorded_at: TransactionTime(tx) };

    let mut txn = store.begin_atomic(&agent_b).expect("iso[f]: begin B");
    store.append_claim(&mut txn, &b1).expect("iso[f]: append b1");
    store.append_claim(&mut txn, &b2).expect("iso[f]: append b2");
    store.append_ledger_entry(&mut txn, &contested_b1).expect("iso[f]: contested_b1");
    store.append_ledger_entry(&mut txn, &contested_b2).expect("iso[f]: contested_b2");
    store.commit(txn).expect("iso[f]: commit B");

    // A: a plain, uncontested claim on the SAME subject-line name.
    let a1 = make_vt_free_claim(&agent_a, "iso-recon-subj", "iso-recon-pred", serde_json::json!("A1"), tx);
    let mut txn = store.begin_atomic(&agent_a).expect("iso[f]: begin A");
    store.append_claim(&mut txn, &a1).expect("iso[f]: append a1");
    store.commit(txn).expect("iso[f]: commit A");

    let audit_uc = AuditUseCase::new(std::sync::Arc::clone(&store));
    let b_ledger_before = audit_uc
        .execute(AuditQueryRequest { agent_id: agent_b.clone(), claim_ref: None, from_tx_time: None, limit: 1000 })
        .expect("iso[f]: B ledger snapshot before").entries.len();

    let config = EngineConfig::default();
    let reconcile_uc = ReconcileUseCase::new(std::sync::Arc::clone(&store), None::<std::sync::Arc<NoOpOracle>>, config);
    let resp = reconcile_uc
        .execute(ReconcileRequest { agent_id: agent_a, subject_lines: vec![("iso-recon-subj".into(), "iso-recon-pred".into())] })
        .expect("iso[f]: A's reconcile must not error");
    let touched_refs: std::collections::HashSet<_> = resp.outcomes.iter().map(|(r, _)| r.clone()).collect();
    assert!(!touched_refs.contains(&ref_b1) && !touched_refs.contains(&ref_b2), "ISOLATION DEFECT: A's reconcile() outcomes touched B's claim_refs {touched_refs:?}");

    let b_ledger_after = audit_uc
        .execute(AuditQueryRequest { agent_id: agent_b, claim_ref: None, from_tx_time: None, limit: 1000 })
        .expect("iso[f]: B ledger snapshot after").entries.len();
    assert_eq!(b_ledger_after, b_ledger_before, "ISOLATION DEFECT: A's reconcile() call appended ledger entries to agent B ({b_ledger_before} -> {b_ledger_after})");
}

/// iso-g: `submit_adjudication` — a caller without knowledge of B's real `handle_id`
/// (i.e. supplying an unrelated/unknown UUID, as any agent that never received B's
/// handle would) gets a clean `AdjudicationHandleNotFound` error, and B's real pending
/// row / ledger are provably untouched by the attempt.
///
/// NOTE: `SubmitAdjudicationUseCase::execute` takes NO caller `agent_id` parameter —
/// `handle_id` is the sole authorization token (a capability-token design, not an
/// agent-authenticated one). This test proves the isolation property that DOES hold
/// (an unknown handle is rejected cleanly, no cross-agent mutation) and flags the
/// design property in RECOMMENDATIONS rather than treating the absence of a caller-
/// agent check as something to "fix" here (out of scope — no production-code changes).
#[cfg(any(test, feature = "test-support"))]
fn iso_submit_adjudication_unknown_handle_is_clean_error<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::submit_adjudication::SubmitAdjudicationUseCase;
    use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
    use crate::MemError;
    use crate::ports::pending_adjudication::{OrphanedQueuedClaim, PendingAdjudicationPort, PendingAdjudicationRow};
    use std::sync::Mutex;

    let agent_b = AgentId("iso-agent-b-adj".into());
    let challenger = make_claim(&agent_b, "iso-adj-subj", "iso-adj-pred");
    let ref_challenger = challenger.claim_ref().clone();
    let incumbent = make_claim(&agent_b, "iso-adj-subj", "iso-adj-pred");
    let ref_incumbent = incumbent.claim_ref().clone();
    let real_handle_id = Uuid::new_v4();

    let queued = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_b.clone(), claim_ref: ref_challenger.clone(), event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::QueuedForAdjudication, rationale: None, recorded_at: TransactionTime(Utc::now()) };
    let mut txn = store.begin_atomic(&agent_b).expect("iso[g]: begin");
    store.append_claim(&mut txn, &challenger).expect("iso[g]: append challenger");
    store.append_claim(&mut txn, &incumbent).expect("iso[g]: append incumbent");
    store.append_ledger_entry(&mut txn, &queued).expect("iso[g]: append queued");
    store.commit(txn).expect("iso[g]: commit");

    struct StubPendingStore { rows: Mutex<Vec<PendingAdjudicationRow>> }
    impl PendingAdjudicationPort for StubPendingStore {
        type Error = std::io::Error;
        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error> { self.rows.lock().unwrap().push(row.clone()); Ok(()) }
        fn get_pending(&self, handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> { Ok(self.rows.lock().unwrap().iter().find(|r| r.handle_id == handle_id).cloned()) }
        fn list_pending(&self, _agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(self.rows.lock().unwrap().clone()) }
        fn list_expired(&self, _now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(vec![]) }
        fn mark_resolved(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> { for r in self.rows.lock().unwrap().iter_mut() { if r.handle_id == handle_id { r.status = "resolved".into(); } } Ok(()) }
        fn mark_expired(&self, handle_id: uuid::Uuid) -> Result<(), Self::Error> { for r in self.rows.lock().unwrap().iter_mut() { if r.handle_id == handle_id { r.status = "expired".into(); } } Ok(()) }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> { Ok(vec![]) }
    }

    let dummy_req = mempill_types::AdjudicationRequest {
        subject_line: mempill_types::SubjectLineRef { agent_id: agent_b.clone(), subject: "iso-adj-subj".into(), predicate: "iso-adj-pred".into() },
        incumbent: mempill_types::Belief {
            claim_ref: ref_incumbent.clone(), fact: incumbent.fact().clone(), provenance: incumbent.provenance().clone(), valid_time: incumbent.valid_time().clone(), transaction_time: incumbent.transaction_time().clone(), confidence: incumbent.confidence().clone(),
            currency_signal: mempill_types::CurrencySignal { last_refreshed_at: incumbent.transaction_time().clone(), state: mempill_types::CurrencyState::Fresh, corroboration_count: 0 },
            criticality: incumbent.criticality().clone(),
        },
        challenger: challenger.clone(),
        criticality: Criticality::Medium,
        reason: mempill_types::OverturnReason::ExternalContradiction,
    };
    let pending_store = std::sync::Arc::new(StubPendingStore { rows: Mutex::new(vec![PendingAdjudicationRow {
        handle_id: real_handle_id, agent_id: agent_b.clone(), subject: "iso-adj-subj".into(), predicate: "iso-adj-pred".into(),
        challenger_claim_ref: ref_challenger.clone(), incumbent_claim_ref: ref_incumbent.clone(), request_payload: dummy_req,
        queued_at: Utc::now(), expires_at: None, status: "pending".into(),
    }]) });

    struct Wrapper(std::sync::Arc<StubPendingStore>);
    impl PendingAdjudicationPort for Wrapper {
        type Error = std::io::Error;
        fn insert_pending(&self, row: &PendingAdjudicationRow) -> Result<(), Self::Error> { self.0.insert_pending(row) }
        fn get_pending(&self, h: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> { self.0.get_pending(h) }
        fn list_pending(&self, a: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { self.0.list_pending(a) }
        fn list_expired(&self, n: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { self.0.list_expired(n) }
        fn mark_resolved(&self, h: uuid::Uuid) -> Result<(), Self::Error> { self.0.mark_resolved(h) }
        fn mark_expired(&self, h: uuid::Uuid) -> Result<(), Self::Error> { self.0.mark_expired(h) }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> { self.0.list_queued_orphan_claims() }
    }
    let erased: std::sync::Arc<dyn ErasedPendingStore> = std::sync::Arc::new(ErasedPendingStoreAdapter::new(Wrapper(std::sync::Arc::clone(&pending_store))));

    let uc = SubmitAdjudicationUseCase::new(std::sync::Arc::clone(&store), std::sync::Arc::clone(&erased));

    // An unrelated caller (no knowledge of B's real handle_id) submits with a random UUID.
    let unknown_handle = Uuid::new_v4();
    assert_ne!(unknown_handle, real_handle_id, "iso[g]: test invariant — the probe handle must differ from B's real handle");
    let response = mempill_types::AdjudicationResponse { handle_id: unknown_handle, verdict: mempill_types::AdjudicationVerdict::Affirm, evidence_provenance: ProvenanceLabel::External(ExternalKind::ExternalFirstHand) };
    let result = uc.execute(unknown_handle, response, Utc::now());
    assert!(
        matches!(result, Err(MemError::AdjudicationHandleNotFound { .. })),
        "ISOLATION DEFECT: submitting an unknown handle_id must return a clean AdjudicationHandleNotFound, not {result:?} \
         (a non-HandleNotFound outcome here could indicate the handle-lookup accidentally matched a different row)"
    );

    // B's real row must be untouched (still pending) and B's ledger must not have gained
    // any AdjudicationResolved entry from the failed probe.
    let row_after = pending_store.get_pending(real_handle_id).expect("iso[g]: get_pending").expect("iso[g]: B's row must still exist");
    assert_eq!(row_after.status, "pending", "ISOLATION DEFECT: an unrelated submit_adjudication call with an unknown handle_id mutated B's real pending row's status to {:?}", row_after.status);

    let ledger_after = store.load_ledger_for_claims(&agent_b, &[ref_challenger.clone()], None).expect("iso[g]: load_ledger_for_claims");
    assert!(
        !ledger_after.iter().any(|e| e.event_kind == LedgerEventKind::AdjudicationResolved),
        "ISOLATION DEFECT: an unrelated submit_adjudication call with an unknown handle_id wrote an \
         AdjudicationResolved entry into B's ledger — cross-agent mutation from an unauthorized handle"
    );
}

/// iso-h: sweep for A's expired pending row leaves B's separately-pending row untouched.
#[cfg(any(test, feature = "test-support"))]
fn iso_sweep_scoped_to_single_row<P>(store: std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::sweep_adjudications::SweepAdjudicationsUseCase;
    use crate::engine_handle::{ErasedPendingStore, ErasedPendingStoreAdapter};
    use crate::ports::pending_adjudication::{OrphanedQueuedClaim, PendingAdjudicationPort, PendingAdjudicationRow};

    let agent_a = AgentId("iso-agent-a-sweep".into());
    let agent_b = AgentId("iso-agent-b-sweep".into());

    let claim_a = make_claim(&agent_a, "iso-sweep-subj", "iso-sweep-pred");
    let ref_a = claim_a.claim_ref().clone();
    let claim_b = make_claim(&agent_b, "iso-sweep-subj", "iso-sweep-pred");
    let ref_b = claim_b.claim_ref().clone();

    let queued_a = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_a.clone(), claim_ref: ref_a.clone(), event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::QueuedForAdjudication, rationale: None, recorded_at: TransactionTime(Utc::now()) };
    let queued_b = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_b.clone(), claim_ref: ref_b.clone(), event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::QueuedForAdjudication, rationale: None, recorded_at: TransactionTime(Utc::now()) };

    let mut txn = store.begin_atomic(&agent_a).expect("iso[h]: begin A");
    store.append_claim(&mut txn, &claim_a).expect("iso[h]: append claim_a");
    store.append_ledger_entry(&mut txn, &queued_a).expect("iso[h]: append queued_a");
    store.commit(txn).expect("iso[h]: commit A");
    let mut txn = store.begin_atomic(&agent_b).expect("iso[h]: begin B");
    store.append_claim(&mut txn, &claim_b).expect("iso[h]: append claim_b");
    store.append_ledger_entry(&mut txn, &queued_b).expect("iso[h]: append queued_b");
    store.commit(txn).expect("iso[h]: commit B");

    struct NoopPending;
    impl PendingAdjudicationPort for NoopPending {
        type Error = std::io::Error;
        fn insert_pending(&self, _row: &PendingAdjudicationRow) -> Result<(), Self::Error> { Ok(()) }
        fn get_pending(&self, _handle_id: uuid::Uuid) -> Result<Option<PendingAdjudicationRow>, Self::Error> { Ok(None) }
        fn list_pending(&self, _agent_id: Option<&AgentId>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(vec![]) }
        fn list_expired(&self, _now: chrono::DateTime<Utc>) -> Result<Vec<PendingAdjudicationRow>, Self::Error> { Ok(vec![]) }
        fn mark_resolved(&self, _handle_id: uuid::Uuid) -> Result<(), Self::Error> { Ok(()) }
        fn mark_expired(&self, _handle_id: uuid::Uuid) -> Result<(), Self::Error> { Ok(()) }
        fn list_queued_orphan_claims(&self) -> Result<Vec<OrphanedQueuedClaim>, Self::Error> { Ok(vec![]) }
    }
    let erased: std::sync::Arc<dyn ErasedPendingStore> = std::sync::Arc::new(ErasedPendingStoreAdapter::new(NoopPending));
    let uc = SweepAdjudicationsUseCase::new(std::sync::Arc::clone(&store), erased);

    let row_a = PendingAdjudicationRow {
        handle_id: Uuid::new_v4(), agent_id: agent_a.clone(), subject: "iso-sweep-subj".into(), predicate: "iso-sweep-pred".into(),
        challenger_claim_ref: ref_a.clone(), incumbent_claim_ref: ref_a.clone(),
        request_payload: mempill_types::AdjudicationRequest {
            subject_line: mempill_types::SubjectLineRef { agent_id: agent_a.clone(), subject: "iso-sweep-subj".into(), predicate: "iso-sweep-pred".into() },
            incumbent: mempill_types::Belief { claim_ref: ref_a.clone(), fact: claim_a.fact().clone(), provenance: claim_a.provenance().clone(), valid_time: claim_a.valid_time().clone(), transaction_time: claim_a.transaction_time().clone(), confidence: claim_a.confidence().clone(), currency_signal: mempill_types::CurrencySignal { last_refreshed_at: claim_a.transaction_time().clone(), state: mempill_types::CurrencyState::Fresh, corroboration_count: 0 }, criticality: claim_a.criticality().clone() },
            challenger: claim_a.clone(), criticality: Criticality::Medium, reason: mempill_types::OverturnReason::ExternalContradiction,
        },
        queued_at: Utc::now() - chrono::Duration::seconds(10), expires_at: Some(Utc::now() - chrono::Duration::seconds(1)), status: "pending".into(),
    };

    let reverted = uc.revert_expired_row(&row_a, Utc::now()).expect("iso[h]: revert_expired_row for A must not error");
    assert!(reverted, "iso[h]: A's expired row must be reverted");

    // A's claim is now Contested; B's must remain QueuedForAdjudication (untouched).
    let ledger_a = store.load_ledger_for_claims(&agent_a, &[ref_a.clone()], None).expect("iso[h]: load A ledger");
    let latest_a = ledger_a.iter().max_by_key(|e| e.recorded_at.0).expect("iso[h]: A ledger non-empty");
    assert_eq!(latest_a.disposition, Disposition::Contested, "iso[h]: A's challenger must now be Contested after sweep");

    let ledger_b = store.load_ledger_for_claims(&agent_b, &[ref_b.clone()], None).expect("iso[h]: load B ledger");
    assert_eq!(ledger_b.len(), 1, "ISOLATION DEFECT: sweeping A's expired row appended an unexpected ledger entry to B (expected exactly 1, the original queued_b)");
    assert_eq!(ledger_b[0].disposition, Disposition::QueuedForAdjudication, "ISOLATION DEFECT: sweeping A's expired pending row touched B's claim's disposition — got {:?}, expected still QueuedForAdjudication", ledger_b[0].disposition);
}

/// Build a `Claim` with no valid-time window set (backward-compatible tx_time ordering),
/// mirroring `make_claim` but with a caller-supplied `tx_time` and `value`.
#[cfg(any(test, feature = "test-support"))]
fn make_vt_free_claim(agent_id: &AgentId, subject: &str, predicate: &str, value: serde_json::Value, tx_time: chrono::DateTime<Utc>) -> Claim {
    Claim::new(
        ClaimRef::new_random(),
        agent_id.clone(),
        Fact { subject: subject.to_owned(), predicate: predicate.to_owned(), value },
        Cardinality::Functional,
        ProvenanceLabel::External(ExternalKind::UserAsserted),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
        TransactionTime(tx_time),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
        Criticality::Low,
        vec![],
        None,
        None,
    )
}

// ── Scale × tenancy conformance harness (TASK-33 / QA-A, 🔴2) ─────────────────

/// Extends the `>10k`-row conformance scenario to TWO agents in one store: agent A
/// accumulates an oversized (`>10_000`) noise ledger while agent B has a small ledger
/// sharing subject/predicate NAMES with A. Proves neither agent's disposition
/// computation is polluted or slowed-to-incorrectness by the other's scale.
#[cfg(any(test, feature = "test-support"))]
pub fn run_scale_tenancy_isolation_conformance<P>(store: &std::sync::Arc<P>)
where
    P: PersistencePort + Send + Sync + 'static,
    P::Error: std::fmt::Debug,
{
    use crate::application::{dto::{IngestClaimRequest, QueryHistoryRequest, QueryMemoryRequest}, ingest_claim::IngestClaimUseCase, query_history::QueryHistoryUseCase, query_memory::QueryMemoryUseCase};
    use crate::config::EngineConfig;
    use crate::noop::{NoOpOracle, NoOpVector};
    use mempill_types::BeliefStatus;

    let agent_a = AgentId("scaletenancy-agent-a".into());
    let agent_b = AgentId("scaletenancy-agent-b".into());
    let config = EngineConfig::default();

    // ── 1. Flood agent A with >10_000 noise ledger rows (mirrors the existing single-agent
    // scale proof — 1010 claims x 10 ledger entries = 10_100 rows) BEFORE B ever writes. ──
    for i in 0..1010u32 {
        let noise = Claim::new(
            ClaimRef::new_random(), agent_a.clone(),
            Fact { subject: format!("scale-noise-subject-{i}"), predicate: "scale-noise-predicate".into(), value: serde_json::json!(i) },
            Cardinality::Functional, ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 }, TransactionTime(Utc::now()),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 }, Criticality::Low, vec![], None, None,
        );
        let noise_ref = noise.claim_ref().clone();
        let mut txn = store.begin_atomic(&agent_a).expect("scaletenancy: noise begin_atomic");
        store.append_claim(&mut txn, &noise).expect("scaletenancy: noise append_claim");
        for _ in 0..10 {
            store.append_ledger_entry(&mut txn, &make_ledger_entry(&agent_a, &noise_ref)).expect("scaletenancy: noise ledger entry");
        }
        store.commit(txn).expect("scaletenancy: noise commit");
    }

    // ── 2. A ingests its OWN belief on a shared subject-line NAME. ──────────────────────
    let ingest_uc = IngestClaimUseCase::new(std::sync::Arc::clone(store), None::<std::sync::Arc<NoOpOracle>>, None, config.clone());
    let resp_a = ingest_uc
        .execute(IngestClaimRequest { agent_id: agent_a.clone(), subject: "scale-org".into(), predicate: "scale-ceo".into(), value: serde_json::json!("A-Alice"), provenance: ProvenanceLabel::External(ExternalKind::UserAsserted), cardinality: Cardinality::Functional, valid_time: None, confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 }, criticality: Criticality::Medium, derived_from: vec![] })
        .expect("scaletenancy: A's ingest must not error despite >10k noise rows");
    assert_eq!(resp_a.disposition, Disposition::CommittedCheap, "scaletenancy: A's first claim on 'scale-org'/'scale-ceo' must be CommittedCheap");

    // ── 3. B ingests, then supersedes, its OWN belief on the SAME subject-line name —
    // B's ledger stays small; must resolve correctly (not Contested, not stale) despite
    // sharing the store with A's flood. ─────────────────────────────────────────────────
    let resp_b1 = ingest_uc
        .execute(IngestClaimRequest { agent_id: agent_b.clone(), subject: "scale-org".into(), predicate: "scale-ceo".into(), value: serde_json::json!("B-Bob"), provenance: ProvenanceLabel::External(ExternalKind::UserAsserted), cardinality: Cardinality::Functional, valid_time: None, confidence: Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 }, criticality: Criticality::Medium, derived_from: vec![] })
        .expect("scaletenancy: B's first ingest must not error");
    assert_eq!(resp_b1.disposition, Disposition::CommittedCheap, "ISOLATION DEFECT: B's first claim must be CommittedCheap, unaffected by A's >10k-row ledger; got {:?} contested_with={:?}", resp_b1.disposition, resp_b1.contested_with);

    // B's second write directly supersedes B1 (Bob) with B2 (Carol) via the store —
    // mirrors the proven single-agent supersession pattern (see
    // test_superseded_claim_excluded_despite_large_agent_ledger above), isolating THIS
    // test to the read-path correctness question: does B's small ledger resolve
    // correctly to Carol despite sharing the store with A's >10k-row flood?
    let claim_b2 = Claim::new(
        ClaimRef::new_random(), agent_b.clone(),
        Fact { subject: "scale-org".into(), predicate: "scale-ceo".into(), value: serde_json::json!("B-Carol") },
        Cardinality::Functional, ProvenanceLabel::External(ExternalKind::ExternalFirstHand),
        ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 }, TransactionTime(Utc::now()),
        ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
        Confidence { value_confidence: 0.95, valid_time_confidence: 0.0 }, Criticality::Medium, vec![], None, None,
    );
    let ref_b2 = claim_b2.claim_ref().clone();
    let ref_b1 = resp_b1.claim_ref.clone();
    let ledger_b1_superseded = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_b.clone(), claim_ref: ref_b1.clone(), event_kind: LedgerEventKind::ValidityAsserted, disposition: Disposition::Superseded, rationale: None, recorded_at: TransactionTime(Utc::now()) };
    let ledger_b2_committed = LedgerEntry { entry_id: Uuid::new_v4(), agent_id: agent_b.clone(), claim_ref: ref_b2.clone(), event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::CommittedCheap, rationale: None, recorded_at: TransactionTime(Utc::now()) };
    let mut txn = store.begin_atomic(&agent_b).expect("scaletenancy: begin B2");
    store.append_claim(&mut txn, &claim_b2).expect("scaletenancy: append B2");
    store.append_ledger_entry(&mut txn, &ledger_b1_superseded).expect("scaletenancy: supersede B1");
    store.append_ledger_entry(&mut txn, &ledger_b2_committed).expect("scaletenancy: commit B2 ledger");
    store.commit(txn).expect("scaletenancy: commit B2 txn");
    let resp_b2_claim_ref = ref_b2;

    // ── 4. Verify: A's belief is still A-Alice (unpolluted by its own flood or by B),
    // and B's belief resolves correctly to its own latest write. ───────────────────────
    let qm_uc = QueryMemoryUseCase::new(std::sync::Arc::clone(store), None::<std::sync::Arc<NoOpVector>>, config.clone());
    let now = Utc::now();
    let belief_a = qm_uc
        .execute_with_time(QueryMemoryRequest { agent_id: agent_a.clone(), subject: "scale-org".into(), predicate: "scale-ceo".into(), as_of_tx_time: None, valid_at: None }, now)
        .expect("scaletenancy: A's query_memory must not error");
    assert!(
        matches!(belief_a.belief.status, BeliefStatus::Resolved | BeliefStatus::TimingUncertain),
        "ISOLATION DEFECT at scale: A's query_memory status must resolve to a single belief \
         (Resolved or TimingUncertain — no explicit valid_time was set) despite >10k noise rows; got {:?}",
        belief_a.belief.status
    );
    assert_eq!(belief_a.belief.primary.as_ref().expect("primary").fact.value, serde_json::json!("A-Alice"), "ISOLATION DEFECT at scale: A's belief value corrupted");

    let qh_uc = QueryHistoryUseCase::new(std::sync::Arc::clone(store), None::<std::sync::Arc<NoOpVector>>, config);
    let hist_b = qh_uc
        .execute_with_time(QueryHistoryRequest { agent_id: agent_b.clone(), subject: "scale-org".into(), predicate: "scale-ceo".into() }, now)
        .expect("scaletenancy: B's query_history must not error");
    assert_eq!(hist_b.entries.len(), 2, "ISOLATION DEFECT at scale: B's history must have exactly 2 entries (Bob superseded, Carol current) despite sharing the store with A's >10k-row flood");
    let current_b = hist_b.current().expect("scaletenancy: B must have a Current entry");
    assert_eq!(current_b.value, serde_json::json!("B-Carol"), "ISOLATION DEFECT at scale: B's current belief must be B-Carol, not stale/wrong");

    assert_ne!(&resp_a.claim_ref, &resp_b1.claim_ref, "scaletenancy: sanity — A and B claim_refs must differ");
    assert_eq!(current_b.claim_ref, resp_b2_claim_ref, "ISOLATION DEFECT at scale: B's current history entry must reference B's own second (Carol) claim_ref");
}

// ── Ledger pagination conformance harness (TASK-33 / QA-A, 🟡9) ───────────────

/// Proves `PersistencePort::load_ledger` supports correct page-walking via
/// `limit` + `from_tx_time` continuation: full coverage, no skips, including a
/// tie-boundary where two entries share the EXACT same `recorded_at`.
///
/// Contract exercised: `load_ledger(agent, from, limit)` returns entries with
/// `recorded_at >= from` (inclusive), ordered ASC. Continuation sets
/// `from = last_seen.recorded_at`, which means entries at an exact tie boundary
/// may be returned again on the next page — callers MUST dedupe by `entry_id`.
/// This test proves the inclusive-bound contract never SKIPS an entry (favors
/// duplication over loss at tie boundaries), and that after de-duplication the
/// walked set is exactly the full entry set with no omissions.
#[cfg(any(test, feature = "test-support"))]
pub fn run_ledger_pagination_conformance<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    let agent = AgentId("conformance-ledger-page-t1".into());
    let base = chrono::DateTime::<Utc>::from_timestamp(50_000_000, 0).unwrap();

    let mut expected_ids: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut txn = store.begin_atomic(&agent).expect("ledger-page[t1]: begin_atomic");
    // 20 entries at distinct, strictly increasing recorded_at (1s apart).
    for i in 0..20i64 {
        let claim = make_claim(&agent, "ledger-page-subj", &format!("pred-{i}"));
        let claim_ref = claim.claim_ref().clone();
        store.append_claim(&mut txn, &claim).expect("ledger-page[t1]: append_claim");
        let entry = LedgerEntry {
            entry_id: Uuid::new_v4(), agent_id: agent.clone(), claim_ref,
            event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::CommittedCheap,
            rationale: None, recorded_at: TransactionTime(base + chrono::Duration::seconds(i)),
        };
        expected_ids.insert(entry.entry_id);
        store.append_ledger_entry(&mut txn, &entry).expect("ledger-page[t1]: append_ledger_entry");
    }
    // Tie: one more entry sharing the EXACT recorded_at of entry index 10 (base+10s).
    let tie_claim = make_claim(&agent, "ledger-page-subj", "pred-tie");
    let tie_ref = tie_claim.claim_ref().clone();
    store.append_claim(&mut txn, &tie_claim).expect("ledger-page[t1]: append tie claim");
    let tie_entry = LedgerEntry {
        entry_id: Uuid::new_v4(), agent_id: agent.clone(), claim_ref: tie_ref,
        event_kind: LedgerEventKind::ClaimCommitted, disposition: Disposition::CommittedCheap,
        rationale: None, recorded_at: TransactionTime(base + chrono::Duration::seconds(10)),
    };
    expected_ids.insert(tie_entry.entry_id);
    store.append_ledger_entry(&mut txn, &tie_entry).expect("ledger-page[t1]: append tie entry");
    store.commit(txn).expect("ledger-page[t1]: commit");

    let total_expected = expected_ids.len();
    assert_eq!(total_expected, 21, "ledger-page[t1]: test setup sanity — 21 distinct entries expected");

    // Walk pages with a small limit that does NOT evenly divide the total, forcing the
    // tie boundary to straddle a page split.
    const PAGE_LIMIT: usize = 7;
    let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut from: Option<TransactionTime> = None;
    let mut pages_walked = 0usize;
    let mut duplicates_observed = 0usize;

    loop {
        pages_walked += 1;
        assert!(pages_walked <= 20, "ledger-page[t1]: pagination did not terminate within 20 pages — possible infinite loop at the tie boundary");

        let page = store
            .load_ledger(&agent, from.as_ref(), PAGE_LIMIT)
            .expect("ledger-page[t1]: load_ledger must not error");

        if page.is_empty() {
            break;
        }

        // ASC ordering within the page.
        for w in page.windows(2) {
            assert!(w[0].recorded_at.0 <= w[1].recorded_at.0, "ledger-page[t1]: page must be ASC-ordered by recorded_at");
        }

        for entry in &page {
            if !seen.insert(entry.entry_id) {
                duplicates_observed += 1;
            }
        }

        let last = page.last().expect("ledger-page[t1]: page non-empty here");
        let is_last_page = page.len() < PAGE_LIMIT;
        from = Some(last.recorded_at.clone());

        if is_last_page {
            break;
        }
    }

    let seen_expected_only: std::collections::HashSet<_> = seen.intersection(&expected_ids).cloned().collect();
    assert_eq!(
        seen_expected_only.len(), total_expected,
        "ledger-page[t1]: pagination must achieve FULL COVERAGE with no skips — walked {} of {} expected entries; missing={:?}",
        seen_expected_only.len(), total_expected,
        expected_ids.difference(&seen).collect::<Vec<_>>()
    );
    // Documented pagination contract: `from` is an INCLUSIVE lower bound with no
    // secondary tiebreak column, so each page boundary legitimately re-fetches the prior
    // page's last `recorded_at` value (favoring duplication over silent loss at ties).
    // Expect roughly one duplicate per page transition (pages_walked - 1); bound loosely
    // to avoid flakiness from ORDER BY tie-ordering, while still catching a runaway
    // over-duplication regression.
    assert!(
        duplicates_observed >= 1,
        "ledger-page[t1]: expected at least one duplicate from inclusive page-boundary \
         continuation (this is the DOCUMENTED contract callers must dedupe against by \
         entry_id) — got zero, which would indicate `from` became exclusive unexpectedly"
    );
    assert!(
        duplicates_observed <= pages_walked,
        "ledger-page[t1]: duplicates_observed={duplicates_observed} exceeds pages_walked={pages_walked} \
         — more duplication than one-per-page-transition; investigate pagination regression"
    );
}
