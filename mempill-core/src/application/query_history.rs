#![allow(missing_docs)]
//! QueryHistoryUseCase — application layer read path for the history timeline.
//!
//! Read-only: no Txn opened, no writes. Returns all claims on a subject-line ordered
//! by the canonical ordering key, with each entry tagged `Current`, `Superseded`,
//! `Contested`, or `Ended`.
//!
//! ## Correctness guarantee (I8 single source of truth)
//!
//! This use-case is a THIN MAPPER over `truth_engine::fold` + `truth_engine::compute_history_windows`
//! — the SAME fold call (and the same succession/overlap primitives in `valid_time_helpers`)
//! that `query_memory` uses. There is no second window/overlap algorithm here: window
//! computation, succession-vs-overlap classification, and the has_conflict signal are all
//! computed once, in the engine layer, and shared by both read paths. This guarantees
//! `history()` and `recall()` / `query_memory` can never drift on which entry is current or
//! which entries are contested — the historical class of bug this design eliminates.
//!
//! ## Status semantics
//!
//! See `mempill_types::HistoryEntryStatus` for the full per-variant contract. In short:
//! `Superseded` = not raw-live (explicitly bounded); `Contested` = raw-live and part of an
//! unresolved conflict (structural `has_conflict` or pairwise valid-time overlap); `Current` =
//! raw-live, unconflicted, narrowed-selected, and its window contains `now`; `Ended` = raw-live
//! but its own window has closed with no live successor covering `now` (the residual bucket).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use mempill_types::{ClaimRef, HistoryEntryStatus, ProvenanceLabel, ExternalKind};

use crate::{
    application::ingest_claim::build_latest_disposition_map,
    config::EngineConfig,
    engine::truth_engine,
    error::MemError,
    ports::{PersistencePort, VectorPort},
};

use super::dto::{HistoryEntry, QueryHistoryRequest, QueryHistoryResponse};

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
        // AUDIT path: pass None to return the full claim history regardless of tx-time.
        // query_history is a historical audit view — it must show every claim ever ingested,
        // not just those visible at a particular tx-time point.
        let claims = self.persistence
            .load_subject_line(&req.agent_id, &req.subject, &req.predicate, None)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        if claims.is_empty() {
            return Ok(QueryHistoryResponse { entries: vec![] });
        }

        // Load ledger scoped to the claims on this subject-line — no agent-wide cap,
        // always complete regardless of total agent ledger size.
        let claim_refs: Vec<_> = claims.iter().map(|c| c.claim_ref().clone()).collect();
        // query_history shows the full ledger history for a subject-line — no tx-time cutoff.
        // Passing None preserves the existing behaviour: all entries visible regardless of
        // recorded_at, which is correct for the audit/history use-case.
        let all_ledger = self.persistence
            .load_ledger_for_claims(&req.agent_id, &claim_refs, None)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;
        let latest_disposition = build_latest_disposition_map(&all_ledger);

        // Canonical fold — SAME call as query_memory so Current/Contested/Superseded agrees
        // with recall. `fold.all_claims` is already sorted by the canonical ordering key
        // (I8) — this use-case never re-sorts or re-derives windows.
        let fold = truth_engine::fold(
            claims,
            |cref| {
                self.persistence
                    .load_validity_assertions_for(&req.agent_id, cref)
                    .unwrap_or_default()
            },
            now,
            None, // valid_at_instant: None = use as_of_tx_time for instant-selection (future wave adds query param)
            &self.config,
            &latest_disposition,
        );

        // Narrowed-selection membership (post succession-narrowing) — required for `Current`.
        let live_refs: std::collections::HashSet<&ClaimRef> = fold
            .live_claims
            .iter()
            .map(|cs| cs.claim.claim_ref())
            .collect();

        // Single engine-layer computation for windows + contested + contains-now, reusing the
        // SAME primitives (`valid_time_helpers::claim_is_trusted` / `windows_non_overlapping`)
        // the fold itself uses for succession narrowing — no second window algorithm.
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &self.config);

        // Map each raw claim + its computed window to a HistoryEntry.
        let entries: Vec<HistoryEntry> = fold.all_claims
            .iter()
            .zip(windows.iter())
            .map(|(cs, w)| {
                let claim = &cs.claim;
                let status = if !cs.is_live {
                    HistoryEntryStatus::Superseded
                } else if w.contested {
                    HistoryEntryStatus::Contested
                } else if live_refs.contains(claim.claim_ref()) && w.contains_now {
                    HistoryEntryStatus::Current
                } else {
                    HistoryEntryStatus::Ended
                };
                HistoryEntry {
                    claim_ref: claim.claim_ref().clone(),
                    value: claim.fact().value.clone(),
                    valid_from: claim.valid_time().start,
                    valid_until: w.valid_until,
                    valid_from_granularity: claim.valid_time().start_granularity,
                    valid_until_granularity: w.valid_until_granularity,
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
            _as_of_tx_time: Option<chrono::DateTime<chrono::Utc>>,
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
        fn load_ledger_for_claims(
            &self,
            _aid: &AgentId,
            _refs: &[ClaimRef],
            _as_of: Option<chrono::DateTime<chrono::Utc>>,
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
        fn list_predicates_for_subject(&self, _aid: &AgentId, _s: &str, _as_of: Option<chrono::DateTime<chrono::Utc>>) -> Result<Vec<String>, MockErr> { Ok(vec![]) }
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
            ValidTime { start: vt_start, end: vt_end, valid_time_confidence: vt_confidence , start_granularity: None, end_granularity: None},
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

    // ── Test 4: window correctness via the engine-layer fold + compute_history_windows ──

    #[test]
    fn effective_window_successor_closes_prior_entry() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        let c1 = make_claim(&agent, "a", "b", serde_json::json!("v1"), t1, None, None, 0.0);
        let c2 = make_claim(&agent, "a", "b", serde_json::json!("v2"), t2, None, None, 0.0);

        let now = t2 + chrono::Duration::seconds(1);
        let fold = truth_engine::fold(vec![c1, c2], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

        // c1's valid_until = ordering key of c2 (= t2 since low confidence uses tx_time; no
        // valid_time at all on either claim → legacy successor-key fallback, unchanged).
        assert_eq!(windows[0].valid_until, Some(t2), "c1 closed by c2's ordering key");
        // c2 is last → open-ended
        assert_eq!(windows[1].valid_until, None, "last entry is open-ended");
    }

    // ── Test 5: status-vs-recall consistency — real conflict must be Contested, not Current ──

    #[test]
    fn conflicting_claims_are_contested_not_silently_current() {
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

        // Two conflicting functional claims (no valid_time) → fold.has_conflict=true → BOTH
        // raw-live entries must be Contested (I7: never silently narrowed or picked as Current).
        assert_eq!(resp.entries.len(), 2);
        for e in &resp.entries {
            assert_eq!(e.status, HistoryEntryStatus::Contested,
                "real conflict must surface as Contested on every raw-live entry, never Current");
        }
        assert!(resp.current().is_none(), "no Current entry when conflicted");
    }

    // ── Test 6: high-confidence ordering key uses valid_time_start ────────────

    #[test]
    fn high_confidence_ordering_key_uses_valid_time_start() {
        let config = EngineConfig::default(); // threshold = 0.7
        let agent = agent();

        // claim A: tx_time late, vt_start early, high confidence, no end → orders by vt_start.
        let tx_late = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let vt_early = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let claim_a = make_claim(&agent, "x", "y", serde_json::json!("A"), tx_late, Some(vt_early), None, 0.9);

        // claim B: tx_time early, no vt_start, low confidence → orders by tx_time (untrusted, so
        // overlap between A and B cannot be determined → legacy successor-key fallback fires).
        let tx_early = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let claim_b = make_claim(&agent, "x", "y", serde_json::json!("B"), tx_early, None, None, 0.0);

        let now = tx_late + chrono::Duration::seconds(1);
        let fold = truth_engine::fold(vec![claim_a, claim_b], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        // A should sort before B because A's ordering key = vt_early (2020) < B's tx_early (2023).
        assert_eq!(fold.all_claims[0].claim.fact().value, serde_json::json!("A"));
        assert_eq!(fold.all_claims[1].claim.fact().value, serde_json::json!("B"));

        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);
        assert_eq!(windows[0].valid_until, Some(tx_early), "A's valid_until = B's ordering key (tx fallback)");
        assert_eq!(windows[1].valid_until, None, "B is last → open-ended");
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
            kind: AssertionKind::Bound { bound_at, bound_at_granularity: None },
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

    // ── Tests for truth_engine::compute_history_windows via fold (pure engine layer) ──

    #[test]
    fn compute_history_windows_empty() {
        let config = EngineConfig::default();
        let now = Utc::now();
        let fold = truth_engine::fold(vec![], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);
        assert!(windows.is_empty());
    }

    #[test]
    fn compute_history_windows_single() {
        let config = EngineConfig::default();
        let agent = agent();
        let tx = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let c = make_claim(&agent, "a", "b", serde_json::json!("v"), tx, None, None, 0.0);
        let now = tx + chrono::Duration::seconds(1);
        let fold = truth_engine::fold(vec![c], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].valid_until, None, "single claim → open-ended");
    }

    #[test]
    fn compute_history_windows_three_entries_no_valid_time() {
        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        let c1 = make_claim(&agent, "a", "b", serde_json::json!("v1"), t1, None, None, 0.0);
        let c2 = make_claim(&agent, "a", "b", serde_json::json!("v2"), t2, None, None, 0.0);
        let c3 = make_claim(&agent, "a", "b", serde_json::json!("v3"), t3, None, None, 0.0);

        let now = t3 + chrono::Duration::seconds(1);
        let fold = truth_engine::fold(vec![c1, c2, c3], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

        assert_eq!(windows.len(), 3);
        // No valid_time at all on any claim → legacy successor-ordering-key fallback (tx_time).
        assert_eq!(windows[0].valid_until, Some(t2));
        assert_eq!(windows[1].valid_until, Some(t3));
        assert_eq!(windows[2].valid_until, None);
    }

    // ── Derived-endpoint granularity tests (TASK-32, updated for the fold-derived design) ──

    #[allow(clippy::too_many_arguments)]
    fn make_claim_gran(
        agent_id: &AgentId,
        subject: &str,
        predicate: &str,
        value: serde_json::Value,
        tx: DateTime<Utc>,
        vt_start: Option<DateTime<Utc>>,
        vt_start_gran: Option<mempill_types::DateGranularity>,
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
            ValidTime {
                start: vt_start,
                end: None,
                valid_time_confidence: vt_confidence,
                start_granularity: vt_start_gran,
                end_granularity: None,
            },
            Confidence { value_confidence: 0.9, valid_time_confidence: vt_confidence },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    /// Predecessor's valid-time is UNTRUSTED (so overlap can't be determined and it has no own
    /// end) → legacy fallback: the trusted successor's `start_granularity` wins.
    #[test]
    fn effective_window_granularity_uses_successor_start_granularity() {
        use mempill_types::DateGranularity;

        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();

        // c1 untrusted (0.3 < threshold): overlap with c2 cannot be determined → fallback fires.
        let c1 = make_claim_gran(&agent, "a", "b", serde_json::json!("v1"), t1, Some(t1), Some(DateGranularity::Day), 0.3);
        let c2 = make_claim_gran(&agent, "a", "b", serde_json::json!("v2"), t2, Some(t2), Some(DateGranularity::Year), 0.9);

        let now = t2 + chrono::Duration::seconds(1);
        let fold = truth_engine::fold(vec![c1, c2], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

        assert_eq!(windows.len(), 2);
        assert_eq!(
            windows[0].valid_until_granularity,
            Some(DateGranularity::Year),
            "predecessor's valid_until_granularity must be the trusted successor's start_granularity"
        );
        assert_eq!(windows[1].valid_until_granularity, None, "last entry (open-ended) has no bounding successor");
    }

    /// Both untrusted: successor's ordering key falls back to `transaction_time` — the
    /// granularity must be `None` (tx_time has no date precision), never fabricated.
    #[test]
    fn effective_window_granularity_none_when_successor_uses_tx_time_fallback() {
        let config = EngineConfig::default(); // threshold = 0.7
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();

        let c1 = make_claim_gran(&agent, "a", "b", serde_json::json!("v1"), t1, Some(t1), Some(mempill_types::DateGranularity::Month), 0.3);
        // Successor has valid_time_confidence below threshold → ordering key falls back to tx_time,
        // even though start_granularity is set — it must NOT leak into valid_until_granularity.
        let c2 = make_claim_gran(&agent, "a", "b", serde_json::json!("v2"), t2, Some(t2), Some(mempill_types::DateGranularity::Year), 0.0);

        let now = t2 + chrono::Duration::seconds(1);
        let fold = truth_engine::fold(vec![c1, c2], |_| vec![], now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

        assert_eq!(
            windows[0].valid_until_granularity, None,
            "successor's ordering key used tx_time fallback → valid_until_granularity must be None, not Year"
        );
    }

    // ── Bound-derived-end granularity (TASK-33-W5-LIB-R2, DIAG-5) ────────────────

    /// The bound-narrowed end of an open-ended incumbent honours the Bound's OWN tracked
    /// granularity (e.g. the winning challenger's `start_granularity` on an Affirm) instead of
    /// silently rendering it at day/instant precision.
    #[test]
    fn bound_derived_end_honours_own_tracked_granularity() {
        use mempill_types::{AssertionKind, DateGranularity, ValidityAssertion};

        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        // Bound at month precision, at an instant that does NOT coincide with any successor
        // (no successor at all here) — proves the granularity comes from the Bound itself,
        // not a succ_gran fallback.
        let bound_at = Utc.with_ymd_and_hms(2024, 9, 1, 0, 0, 0).unwrap();

        let c1 = make_claim(&agent, "a", "b", serde_json::json!("v1"), t1, None, None, 0.0);
        let c1_ref = c1.claim_ref().clone();

        let assertion = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: c1_ref.clone(),
            kind: AssertionKind::Bound { bound_at, bound_at_granularity: Some(DateGranularity::Month) },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(bound_at),
        };

        let now = bound_at + chrono::Duration::days(1);
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == c1_ref { vec![assertion.clone()] } else { vec![] }
        };
        let fold = truth_engine::fold(vec![c1], assertions_fn, now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

        assert_eq!(windows[0].valid_until, Some(bound_at));
        assert_eq!(
            windows[0].valid_until_granularity,
            Some(DateGranularity::Month),
            "bound-derived end must carry the Bound's OWN granularity (2024-09), not None/day"
        );
    }

    /// When the Bound carries no granularity (Deny's tx_time fallback, or a legacy row) but
    /// its instant numerically coincides with the successor's start key, the successor's start
    /// granularity is attributed to the bound-derived end (DIAG-5 fallback rule) — never
    /// fabricated when the instants differ.
    #[test]
    fn bound_derived_end_falls_back_to_successor_granularity_when_instants_coincide() {
        use mempill_types::{AssertionKind, DateGranularity, ValidityAssertion};

        let config = EngineConfig::default();
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let joan_start = Utc.with_ymd_and_hms(2024, 9, 1, 0, 0, 0).unwrap();

        // Incumbent, open-ended, bounded (via Affirm-shaped write) at joan_start with NO
        // tracked granularity of its own — simulates a legacy row / pre-population gap.
        let incumbent = make_claim(&agent, "a", "b", serde_json::json!("diane"), t1, None, None, 0.0);
        let incumbent_ref = incumbent.claim_ref().clone();
        // Successor: trusted, high confidence, start = joan_start, tracked as Month precision.
        let successor = make_claim_gran(&agent, "a", "b", serde_json::json!("joan"), joan_start, Some(joan_start), Some(DateGranularity::Month), 0.9);

        let assertion = ValidityAssertion {
            assertion_ref: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            target_claim: incumbent_ref.clone(),
            kind: AssertionKind::Bound { bound_at: joan_start, bound_at_granularity: None },
            provenance: ProvenanceLabel::External(ExternalKind::UserAsserted),
            confidence: Confidence { value_confidence: 1.0, valid_time_confidence: 1.0 },
            asserted_at: TransactionTime(joan_start),
        };

        let now = joan_start + chrono::Duration::days(1);
        let assertions_fn = move |cr: &ClaimRef| -> Vec<ValidityAssertion> {
            if *cr == incumbent_ref { vec![assertion.clone()] } else { vec![] }
        };
        let fold = truth_engine::fold(vec![incumbent, successor], assertions_fn, now, None, &config, &std::collections::HashMap::new());
        let windows = truth_engine::compute_history_windows(&fold.all_claims, fold.has_conflict, now, &config);

        assert_eq!(windows[0].valid_until, Some(joan_start));
        assert_eq!(
            windows[0].valid_until_granularity,
            Some(DateGranularity::Month),
            "Bound with no own granularity, coinciding with the successor's start, must fall back to succ_gran (2024-09, not 2024-09-01)"
        );
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

    // ── End-to-end: execute_with_time populates HistoryEntry granularity fields ─

    #[test]
    fn execute_with_time_populates_history_entry_granularity_fields() {
        use mempill_types::DateGranularity;

        let store = Arc::new(MockStore::default());
        let agent = agent();
        let t1 = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();

        // Alice is UNTRUSTED (0.3 < threshold) and open-ended (no own end): overlap against Bob
        // cannot be determined, so the legacy successor-key fallback fires and Bob's OWN
        // start_granularity flows through as Alice's valid_until_granularity. (Two mutually
        // TRUSTED open-ended claims would instead be a genuine overlap → Contested — see
        // `conflicting_claims_are_contested_not_silently_current` — so this scenario requires
        // one side untrusted to exercise the fallback path honestly.)
        let c1 = make_claim_gran(&agent, "acme", "ceo", serde_json::json!("Alice"), t1, Some(t1), Some(DateGranularity::Day), 0.3);
        let c2 = make_claim_gran(&agent, "acme", "ceo", serde_json::json!("Bob"), t2, Some(t2), Some(DateGranularity::Year), 0.9);
        store.claims.lock().unwrap().push(c1);
        store.claims.lock().unwrap().push(c2);

        let uc = uc(Arc::clone(&store));
        let now = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let resp = uc.execute_with_time(
            QueryHistoryRequest { agent_id: agent, subject: "acme".into(), predicate: "ceo".into() },
            now,
        ).unwrap();

        assert_eq!(resp.entries.len(), 2);
        assert_eq!(
            resp.entries[0].valid_from_granularity,
            Some(DateGranularity::Day),
            "Alice's own valid_from_granularity must be Day"
        );
        assert_eq!(
            resp.entries[0].valid_until_granularity,
            Some(DateGranularity::Year),
            "Alice's valid_until_granularity is derived from Bob's (successor) start_granularity"
        );
        assert_eq!(
            resp.entries[1].valid_from_granularity,
            Some(DateGranularity::Year),
            "Bob's own valid_from_granularity must be Year"
        );
        assert_eq!(
            resp.entries[1].valid_until_granularity, None,
            "Bob is the open-ended current entry → None"
        );
    }

    /// Legacy row (no granularity tracked) → both fields None, never fabricated.
    #[test]
    fn execute_with_time_legacy_row_has_none_granularity() {
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

        assert_eq!(resp.entries[0].valid_from_granularity, None);
        assert_eq!(resp.entries[0].valid_until_granularity, None);
    }
}
