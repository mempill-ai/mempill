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
pub fn run_disposition_scope_conformance<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    test_superseded_claim_excluded_despite_large_agent_ledger(store);
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
/// Exercises `compute_effective_windows` (pure) and `truth_engine::fold` via the real
/// persistence backend. Uses DISTINCT agent_id namespace (`conformance-hist-*`).
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
    use crate::application::query_history::compute_effective_windows;
    use crate::config::EngineConfig;

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
    let refs: Vec<&Claim> = claims.iter().collect();
    let windows = compute_effective_windows(&refs, &config);
    assert_eq!(windows.len(), 1, "conformance[hist-t2]: 1 window");
    assert_eq!(windows[0], None, "conformance[hist-t2]: single claim has open-ended window");
}

/// CEO succession: Alice→John→Bob — 3 entries ordered oldest first, windows correct.
/// This is the canonical CEO-timeline scenario from the DESIGN.md.
#[cfg(any(test, feature = "test-support"))]
fn hist_succession_ordering<P>(store: &P)
where
    P: PersistencePort,
    P::Error: std::fmt::Debug,
{
    use crate::application::query_history::compute_effective_windows;
    use crate::config::EngineConfig;

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

    let mut claims = store
        .load_subject_line(&agent, "hist-corp", "ceo", None)
        .expect("conformance[hist-t3]: load_subject_line must not error");
    assert_eq!(claims.len(), 3, "conformance[hist-t3]: must have 3 claims (Alice, John, Bob)");

    let config = EngineConfig::default();
    // Sort by canonical ordering key (tx_time — all low confidence).
    claims.sort_by(|a, b| {
        a.transaction_time().0.cmp(&b.transaction_time().0)
            .then(a.claim_ref().0.as_u128().cmp(&b.claim_ref().0.as_u128()))
    });

    let refs: Vec<&Claim> = claims.iter().collect();
    let windows = compute_effective_windows(&refs, &config);

    // Windows: Alice closed by John's tx, John closed by Bob's tx, Bob open.
    assert_eq!(windows[0], Some(t_john), "conformance[hist-t3]: Alice's valid_until = John's ordering key");
    assert_eq!(windows[1], Some(t_bob),  "conformance[hist-t3]: John's valid_until = Bob's ordering key");
    assert_eq!(windows[2], None,          "conformance[hist-t3]: Bob is open-ended (current)");

    // Values in canonical order (oldest first).
    assert_eq!(claims[0].fact().value, serde_json::json!("Alice"), "conformance[hist-t3]: oldest is Alice");
    assert_eq!(claims[1].fact().value, serde_json::json!("John"),  "conformance[hist-t3]: middle is John");
    assert_eq!(claims[2].fact().value, serde_json::json!("Bob"),   "conformance[hist-t3]: newest is Bob");
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
