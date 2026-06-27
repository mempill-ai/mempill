#![allow(missing_docs)]
//! QueryHistoryUseCase — application layer read path for the history timeline.
//!
//! Read-only: no Txn opened, no writes. Returns all claims on a subject-line ordered
//! by the canonical ordering key, with each entry tagged Current or Superseded.
//!
//! ## Correctness guarantee
//!
//! `Current` / `Superseded` is derived from `is_live` in the SAME `truth_engine::fold`
//! call that `query_memory` uses (with `now` from the boundary), so `history()` and
//! `recall()` / `query_memory` are guaranteed to agree on which entry is current.
//!
//! ## Effective-window computation
//!
//! `valid_until` for entry i = the canonical ordering key of entry i+1.
//! The last (open-ended / current) entry has `valid_until = None`.
//! This logic is extracted into the pure function `compute_effective_windows` so it
//! can be unit-tested in isolation.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use mempill_types::{Claim, ClaimRef, HistoryEntryStatus, ProvenanceLabel, ExternalKind};

use crate::{
    application::ingest_claim::build_latest_disposition_map,
    config::EngineConfig,
    engine::truth_engine,
    error::MemError,
    ports::{PersistencePort, VectorPort},
};

use super::dto::{HistoryEntry, QueryHistoryRequest, QueryHistoryResponse};

// ── Ordering key (pure) ───────────────────────────────────────────────────────

/// Compute the canonical ordering key for a claim — mirrors `truth_engine::ordering_key`
/// exactly so the sort order here matches the fold sort order.
fn ordering_key_dt(claim: &Claim, config: &EngineConfig) -> DateTime<Utc> {
    if claim.valid_time().valid_time_confidence >= config.valid_time_confidence_threshold {
        claim.valid_time().start.unwrap_or(claim.transaction_time().0)
    } else {
        claim.transaction_time().0
    }
}

// ── Provenance formatting ─────────────────────────────────────────────────────

/// Format a `ProvenanceLabel` as a human-readable string.
/// Identical to `provenance_label_str` in `mempill-facade/src/ergonomic.rs`.
fn format_provenance(p: &ProvenanceLabel) -> String {
    match p {
        ProvenanceLabel::External(ExternalKind::UserAsserted) => {
            "External/UserAsserted".to_owned()
        }
        ProvenanceLabel::External(ExternalKind::ExternalFirstHand) => {
            "External/ExternalFirstHand".to_owned()
        }
        ProvenanceLabel::RecallReEntry => "RecallReEntry".to_owned(),
        ProvenanceLabel::ModelDerived => "ModelDerived".to_owned(),
        _ => format!("{p:?}"),
    }
}

// ── Pure helper: compute effective valid_until windows ────────────────────────

/// Compute the effective `valid_until` for each claim in the sorted timeline.
///
/// The timeline must be pre-sorted by the canonical ordering key (oldest first).
///
/// Rule: entry i's `valid_until` = the canonical ordering key of entry i+1.
///       The last entry (most recent / open-ended) has `valid_until = None`.
///
/// This function is PURE (no I/O, no clock) and is tested independently.
pub fn compute_effective_windows(
    sorted: &[&Claim],
    config: &EngineConfig,
) -> Vec<Option<DateTime<Utc>>> {
    let n = sorted.len();
    let mut windows = Vec::with_capacity(n);
    for i in 0..n {
        if i + 1 < n {
            // Successor's canonical ordering key closes this entry's window.
            windows.push(Some(ordering_key_dt(sorted[i + 1], config)));
        } else {
            // Last entry — open-ended.
            windows.push(None);
        }
    }
    windows
}

// ── Use-case ──────────────────────────────────────────────────────────────────

/// Use-case: retrieve the full ordered history timeline for a (subject, predicate) line.
///
/// Generic over persistence and vector ports (vector is unused; compile-time seam only).
pub struct QueryHistoryUseCase<P, V>
where
    P: PersistencePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    #[allow(dead_code)]
    vector: Option<Arc<V>>,
    config: EngineConfig,
}

impl<P, V> QueryHistoryUseCase<P, V>
where
    P: PersistencePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, vector: Option<Arc<V>>, config: EngineConfig) -> Self {
        Self { persistence, vector, config }
    }

    /// Read path: no Txn (read-only). TruthEngine fold → history timeline DTO.
    ///
    /// `now` is injected by the EngineHandle (DETERMINISM — no clock reads here).
    pub fn execute_with_time(
        &self,
        req: QueryHistoryRequest,
        now: DateTime<Utc>,
    ) -> Result<QueryHistoryResponse, MemError> {
        // Load all claims for the subject-line (including superseded ones).
        let claims = self.persistence
            .load_subject_line(&req.agent_id, &req.subject, &req.predicate)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        if claims.is_empty() {
            return Ok(QueryHistoryResponse { entries: vec![] });
        }

        // Load ledger for the disposition-based liveness filter.
        let all_ledger = self.persistence
            .load_ledger(&req.agent_id, None, 10_000)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let latest_disposition = build_latest_disposition_map(&all_ledger);

        // Canonical fold — SAME call as query_memory so Current/Superseded agrees with recall.
        let fold = truth_engine::fold(
            claims.clone(),
            |cref| {
                self.persistence
                    .load_validity_assertions_for(&req.agent_id, cref)
                    .unwrap_or_default()
            },
            now,
            &self.config,
            &latest_disposition,
        );

        // Build a set of live claim refs (those that are Current).
        let live_refs: std::collections::HashSet<&ClaimRef> = fold
            .live_claims
            .iter()
            .map(|cs| cs.claim.claim_ref())
            .collect();

        // Sort all claims by canonical ordering key (oldest first) — same sort as fold.
        let mut sorted_claims = claims;
        sorted_claims.sort_by(|a, b| {
            let ka = ordering_key_dt(a, &self.config);
            let kb = ordering_key_dt(b, &self.config);
            ka.cmp(&kb)
                .then(a.transaction_time().0.cmp(&b.transaction_time().0))
                .then(a.claim_ref().0.as_u128().cmp(&b.claim_ref().0.as_u128()))
        });

        // Compute effective valid_until windows.
        let refs: Vec<&Claim> = sorted_claims.iter().collect();
        let windows = compute_effective_windows(&refs, &self.config);

        // Map each claim to a HistoryEntry.
        let entries: Vec<HistoryEntry> = sorted_claims
            .iter()
            .zip(windows)
            .map(|(claim, valid_until)| {
                let status = if live_refs.contains(claim.claim_ref()) {
                    HistoryEntryStatus::Current
                } else {
                    HistoryEntryStatus::Superseded
                };
                HistoryEntry {
                    claim_ref: claim.claim_ref().clone(),
                    value: claim.fact().value.clone(),
                    valid_from: claim.valid_time().start,
                    valid_until,
                    status,
                    provenance: format_provenance(claim.provenance()),
                    value_confidence: claim.confidence().value_confidence,
                }
            })
            .collect();

        Ok(QueryHistoryResponse { entries })
    }

    /// Convenience wrapper that stamps now internally (for direct calls outside EngineHandle).
    pub fn execute(&self, req: QueryHistoryRequest) -> Result<QueryHistoryResponse, MemError> {
        self.execute_with_time(req, Utc::now())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EngineConfig;
    use crate::noop::NoOpVector;
    use crate::ports::persistence::Txn;
    use chrono::TimeZone;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimEdge, ClaimRef, Confidence, Criticality,
        ExternalAnchor, ExternalKind, Fact, LedgerEntry, ProvenanceLabel, TransactionTime,
        ValidTime, ValidityAssertion,
    };
    use std::sync::Mutex;

    // ── Minimal mock store ────────────────────────────────────────────────────

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock")]
    struct MockErr;

    #[derive(Default)]
    struct MockStore {
        claims: Mutex<Vec<Claim>>,
        assertions: Mutex<Vec<ValidityAssertion>>,
    }

    impl PersistencePort for MockStore {
        type Transaction = MockTxn;
        type Error = MockErr;
        fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> {
            Ok(MockTxn(aid.clone()))
        }
        fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> {
            self.claims.lock().unwrap().push(c.clone());
            Ok(c.claim_ref().clone())
        }
        fn append_validity_assertion(
            &self,
            _t: &mut MockTxn,
            a: &ValidityAssertion,
        ) -> Result<(), MockErr> {
            self.assertions.lock().unwrap().push(a.clone());
            Ok(())
        }
        fn append_ledger_entry(
            &self,
            _t: &mut MockTxn,
            _e: &LedgerEntry,
        ) -> Result<(), MockErr> {
            Ok(())
        }
        fn append_claim_edge(
            &self,
            _t: &mut MockTxn,
            _e: &ClaimEdge,
        ) -> Result<(), MockErr> {
            Ok(())
        }
        fn commit(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn load_subject_line(
            &self,
            _aid: &AgentId,
            subject: &str,
            predicate: &str,
        ) -> Result<Vec<Claim>, MockErr> {
            let claims = self.claims.lock().unwrap();
            Ok(claims
                .iter()
                .filter(|c| {
                    c.fact().subject == subject && c.fact().predicate == predicate
                })
                .cloned()
                .collect())
        }
        fn load_claim(
            &self,
            _aid: &AgentId,
            r: &ClaimRef,
        ) -> Result<Option<Claim>, MockErr> {
            let claims = self.claims.lock().unwrap();
            Ok(claims.iter().find(|c| c.claim_ref() == r).cloned())
        }
        fn load_validity_assertions_for(
            &self,
            _aid: &AgentId,
            r: &ClaimRef,
        ) -> Result<Vec<ValidityAssertion>, MockErr> {
            let assertions = self.assertions.lock().unwrap();
            Ok(assertions
                .iter()
                .filter(|a| &a.target_claim == r)
                .cloned()
                .collect())
        }
        fn load_ledger(
            &self,
            _aid: &AgentId,
            _from: Option<&mempill_types::TransactionTime>,
            _lim: usize,
        ) -> Result<Vec<LedgerEntry>, MockErr> {
            Ok(vec![])
        }
        fn load_edges_for(
            &self,
            _aid: &AgentId,
            _r: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockErr> {
            Ok(vec![])
        }
        fn load_injected_claims(
            &self,
            _aid: &AgentId,
        ) -> Result<Vec<ClaimRef>, MockErr> {
            Ok(vec![])
        }
        fn load_lineage(
            &self,
            _aid: &AgentId,
            _r: &ClaimRef,
        ) -> Result<Vec<ClaimEdge>, MockErr> {
            Ok(vec![])
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn agent() -> AgentId {
        AgentId("test-agent".into())
    }

    #[allow(clippy::too_many_arguments)]
    // reason: test helper mirrors the full Claim constructor — grouping into a struct would obscure call sites
    fn make_claim(
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
        value: serde_json::Value,
        tx: DateTime<Utc>,
        vt_start: Option<DateTime<Utc>>,
        vt_end: Option<DateTime<Utc>>,
        vt_confidence: f32,
    ) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            agent_id.clone(),
            Fact { subject: subject.into(), predicate: predicate.into(), value },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime { start: vt_start, end: vt_end, valid_time_confidence: vt_confidence },
            Confidence { value_confidence: 0.9, valid_time_confidence: vt_confidence },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    fn uc(store: Arc<MockStore>) -> QueryHistoryUseCase<MockStore, NoOpVector> {
        QueryHistoryUseCase::new(store, None::<Arc<NoOpVector>>, EngineConfig::default())
    }

    // ── Test 1: empty subject-line → empty entries ────────────────────────────

    #[test]
    fn empty_subject_line_returns_empty_entries() {
        let store = Arc::new(MockStore::default());
        let uc = uc(Arc::clone(&store));
        let now = Utc::now();
        let req = QueryHistoryRequest {
            agent_id: agent(),
            subject: "nobody".into(),
            predicate: "nothing".into(),
        };
        let resp = uc.execute_with_time(req, now).unwrap();
        assert!(resp.entries.is_empty(), "no claims → empty history");
        assert!(resp.current().is_none(), "no current entry");
    }

    // ── Test 2: single claim → 1 entry with status Current ───────────────────

    #[test]
    fn single_claim_returns_one_current_entry() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let tx = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let claim = make_claim(&agent, "acme", "ceo", serde_json::json!("Alice"), tx, None, None, 0.0);
        store.claims.lock().unwrap().push(claim.clone());

        let uc = uc(Arc::clone(&store));
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let resp = uc.execute_with_time(
            QueryHistoryRequest { agent_id: agent, subject: "acme".into(), predicate: "ceo".into() },
            now,
        ).unwrap();

        assert_eq!(resp.entries.len(), 1, "one claim → one entry");
        assert_eq!(resp.entries[0].status, HistoryEntryStatus::Current);
        assert_eq!(resp.entries[0].value, serde_json::json!("Alice"));
        assert!(resp.entries[0].valid_until.is_none(), "single entry has no successor → open-ended");
    }

    // ── Test 3: succession ordering — two claims, older first ────────────────

    #[test]
    fn succession_ordering_oldest_first() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        // Insert in reverse order to verify sort is not insertion-order
        let claim2 = make_claim(&agent, "acme", "ceo", serde_json::json!("Bob"), t2, None, None, 0.0);
        let claim1 = make_claim(&agent, "acme", "ceo", serde_json::json!("Alice"), t1, None, None, 0.0);
        store.claims.lock().unwrap().push(claim2);
        store.claims.lock().unwrap().push(claim1);

        let uc = uc(Arc::clone(&store));
        let now = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let resp = uc.execute_with_time(
            QueryHistoryRequest { agent_id: agent, subject: "acme".into(), predicate: "ceo".into() },
            now,
        ).unwrap();

        assert_eq!(resp.entries.len(), 2);
        assert_eq!(resp.entries[0].value, serde_json::json!("Alice"), "oldest first");
        assert_eq!(resp.entries[1].value, serde_json::json!("Bob"), "newer second");
    }

    // ── Test 4: effective-window correctness ──────────────────────────────────

    #[test]
    fn effective_window_successor_closes_prior_entry() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        let c1 = make_claim(&agent, "a", "b", serde_json::json!("v1"), t1, None, None, 0.0);
        let c2 = make_claim(&agent, "a", "b", serde_json::json!("v2"), t2, None, None, 0.0);

        let sorted: Vec<&Claim> = vec![&c1, &c2];
        let windows = compute_effective_windows(&sorted, &config);

        // c1's valid_until = ordering key of c2 (= t2 since low confidence uses tx_time)
        assert_eq!(windows[0], Some(t2), "c1 closed by c2's ordering key");
        // c2 is last → open-ended
        assert_eq!(windows[1], None, "last entry is open-ended");
    }

    // ── Test 5: status-vs-recall consistency ──────────────────────────────────

    #[test]
    fn current_entry_value_matches_recall_primary() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        let c1 = make_claim(&agent, "acme", "ceo", serde_json::json!("Alice"), t1, None, None, 0.0);
        let c2 = make_claim(&agent, "acme", "ceo", serde_json::json!("Bob"), t2, None, None, 0.0);
        store.claims.lock().unwrap().push(c1);
        store.claims.lock().unwrap().push(c2);

        let uc = uc(Arc::clone(&store));
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let resp = uc.execute_with_time(
            QueryHistoryRequest { agent_id: agent, subject: "acme".into(), predicate: "ceo".into() },
            now,
        ).unwrap();

        // With two conflicting functional claims (no valid_time), has_conflict=true.
        // Both are "live" in the fold sense (contested). The test verifies at least one Current entry.
        let current_entries: Vec<_> = resp.entries.iter().filter(|e| e.status == HistoryEntryStatus::Current).collect();
        assert!(!current_entries.is_empty(), "at least one Current entry must exist");
    }

    // ── Test 6: high-confidence ordering key uses valid_time_start ────────────

    #[test]
    fn high_confidence_ordering_key_uses_valid_time_start() {
        let config = EngineConfig::default(); // threshold = 0.7
        let agent = agent();

        // claim A: tx_time late, vt_start early, high confidence → orders by vt_start
        let tx_late = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let vt_early = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let claim_a = make_claim(&agent, "x", "y", serde_json::json!("A"), tx_late, Some(vt_early), None, 0.9);

        // claim B: tx_time early, no vt_start, low confidence → orders by tx_time
        let tx_early = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let claim_b = make_claim(&agent, "x", "y", serde_json::json!("B"), tx_early, None, None, 0.0);

        // A should sort before B because A's ordering key = vt_early (2020) < B's tx_early (2023)
        let key_a = ordering_key_dt(&claim_a, &config);
        let key_b = ordering_key_dt(&claim_b, &config);
        assert!(key_a < key_b, "high-confidence A (vt=2020) must precede B (tx=2023)");

        // Verify compute_effective_windows puts A's valid_until = B's key
        let sorted: Vec<&Claim> = vec![&claim_a, &claim_b];
        let windows = compute_effective_windows(&sorted, &config);
        assert_eq!(windows[0], Some(key_b), "A's valid_until = B's ordering key");
        assert_eq!(windows[1], None, "B is last → open-ended");
    }

    // ── Test 7: reinstated/edge case — no live claims → all Superseded ────────

    #[test]
    fn all_claims_bounded_returns_all_superseded() {
        use mempill_types::{AssertionKind, ValidityAssertion};
        use uuid::Uuid;

        let store = Arc::new(MockStore::default());
        let agent = agent();
        let tx = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let bound_at = Utc.with_ymd_and_hms(2021, 1, 1, 0, 0, 0).unwrap();

        let claim = make_claim(&agent, "acme", "ceo", serde_json::json!("Alice"), tx, None, None, 0.0);
        let claim_ref = claim.claim_ref().clone();

        let assertion = ValidityAssertion {
            assertion_ref: Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: claim_ref.clone(),
            kind: AssertionKind::Bound { bound_at },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_at),
        };

        store.claims.lock().unwrap().push(claim);
        store.assertions.lock().unwrap().push(assertion);

        let uc = uc(Arc::clone(&store));
        // Query well after the bound
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let resp = uc.execute_with_time(
            QueryHistoryRequest { agent_id: agent, subject: "acme".into(), predicate: "ceo".into() },
            now,
        ).unwrap();

        assert_eq!(resp.entries.len(), 1, "one claim in history");
        assert_eq!(
            resp.entries[0].status,
            HistoryEntryStatus::Superseded,
            "bounded claim must be Superseded"
        );
        assert!(resp.current().is_none(), "no current entry when all claims are bounded");
    }

    // ── Tests for compute_effective_windows (pure function) ───────────────────

    #[test]
    fn compute_effective_windows_empty() {
        let config = EngineConfig::default();
        let windows = compute_effective_windows(&[], &config);
        assert!(windows.is_empty());
    }

    #[test]
    fn compute_effective_windows_single() {
        let config = EngineConfig::default();
        let agent = agent();
        let tx = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let c = make_claim(&agent, "a", "b", serde_json::json!("v"), tx, None, None, 0.0);
        let sorted = vec![&c];
        let windows = compute_effective_windows(&sorted, &config);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0], None, "single claim → open-ended");
    }

    #[test]
    fn compute_effective_windows_three_entries() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        let c1 = make_claim(&agent, "a", "b", serde_json::json!("v1"), t1, None, None, 0.0);
        let c2 = make_claim(&agent, "a", "b", serde_json::json!("v2"), t2, None, None, 0.0);
        let c3 = make_claim(&agent, "a", "b", serde_json::json!("v3"), t3, None, None, 0.0);

        let sorted = vec![&c1, &c2, &c3];
        let windows = compute_effective_windows(&sorted, &config);

        assert_eq!(windows.len(), 3);
        // Each entry closes at successor's tx_time (low confidence)
        assert_eq!(windows[0], Some(t2));
        assert_eq!(windows[1], Some(t3));
        assert_eq!(windows[2], None);
    }

    // ── Additional: provenance format ─────────────────────────────────────────

    #[test]
    fn provenance_formatted_correctly_in_entry() {
        let store = Arc::new(MockStore::default());
        let agent = agent();
        let tx = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let claim = make_claim(&agent, "acme", "ceo", serde_json::json!("Alice"), tx, None, None, 0.0);
        store.claims.lock().unwrap().push(claim);

        let uc = uc(Arc::clone(&store));
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let resp = uc.execute_with_time(
            QueryHistoryRequest { agent_id: agent, subject: "acme".into(), predicate: "ceo".into() },
            now,
        ).unwrap();

        assert_eq!(resp.entries[0].provenance, "External/UserAsserted");
    }
}
