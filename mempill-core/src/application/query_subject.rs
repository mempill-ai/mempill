#![allow(missing_docs)]
//! QuerySubjectUseCase — enumerate all resolved beliefs for a subject across predicates.
//!
//! Read-only. For each distinct predicate stored under the subject (filtered by the
//! tx-time cutoff), delegates to the EXISTING query_memory fold (QueryMemoryUseCase) and
//! collects the per-predicate result into a SubjectFactEntry.
//!
//! No new fold logic — 100% reuse of the existing truth_engine fold path.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use mempill_types::{BeliefStatus, ProvenanceLabel, ExternalKind};

use crate::{
    config::EngineConfig,
    error::MemError,
    ports::{PersistencePort, VectorPort},
};

use super::dto::{
    QueryMemoryRequest, QuerySubjectRequest, QuerySubjectResponse, SubjectFactEntry,
};
use super::query_memory::QueryMemoryUseCase;

/// Use-case: return the resolved belief for every predicate under a subject.
///
/// Generic over persistence and vector ports (mirrors QueryMemoryUseCase).
pub struct QuerySubjectUseCase<P, V>
where
    P: PersistencePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    persistence: Arc<P>,
    vector: Option<Arc<V>>,
    config: EngineConfig,
}

impl<P, V> QuerySubjectUseCase<P, V>
where
    P: PersistencePort + Send + Sync + 'static,
    V: VectorPort + Send + Sync + 'static,
{
    pub fn new(persistence: Arc<P>, vector: Option<Arc<V>>, config: EngineConfig) -> Self {
        Self { persistence, vector, config }
    }

    /// Execute with an explicit `now` (DETERMINISM — no clock reads here).
    ///
    /// Algorithm:
    ///   1. list_predicates_for_subject → distinct predicates visible at as_of_tx_time.
    ///   2. For each predicate, run the existing QueryMemoryUseCase fold (same valid_at +
    ///      as_of_tx_time), collecting a SubjectFactEntry.
    ///   3. Sort entries by predicate (stable, deterministic output).
    pub fn execute_with_time(
        &self,
        req: QuerySubjectRequest,
        now: DateTime<Utc>,
    ) -> Result<QuerySubjectResponse, MemError> {
        // Step 1: list distinct predicates visible at the tx-time cutoff.
        let predicates = self.persistence
            .list_predicates_for_subject(&req.agent_id, &req.subject, req.as_of_tx_time)
            .map_err(|e| MemError::Persistence { source: Box::new(e) })?;

        // Step 2: fold each predicate using the existing QueryMemoryUseCase.
        let query_uc = QueryMemoryUseCase::new(
            Arc::clone(&self.persistence),
            self.vector.clone(),
            self.config.clone(),
        );

        let mut entries: Vec<SubjectFactEntry> = predicates
            .into_iter()
            .map(|predicate| {
                let mem_req = QueryMemoryRequest {
                    agent_id: req.agent_id.clone(),
                    subject: req.subject.clone(),
                    predicate: predicate.clone(),
                    as_of_tx_time: req.as_of_tx_time,
                    valid_at: req.valid_at,
                };
                let resp = query_uc.execute_with_time(mem_req, now)?;
                let belief = resp.belief;

                let status_str = match belief.status {
                    BeliefStatus::Resolved => "Resolved",
                    BeliefStatus::Contested => "Contested",
                    BeliefStatus::NoBelief => "NoBelief",
                    BeliefStatus::TimingUncertain => "TimingUncertain",
                    BeliefStatus::Conflict => "Contested", // surface as Contested to caller
                    _ => "NoBelief",
                };

                // Extract from the primary belief slot (present for Resolved / TimingUncertain).
                let (value, valid_from_display, valid_until_display, provenance_str, claim_ref_str, conf) =
                    if let Some(primary) = &belief.primary {
                        let val_str = match &primary.fact.value {
                            serde_json::Value::String(s) => Some(s.clone()),
                            other => Some(other.to_string()),
                        };

                        // Compute display strings using the same helper as enrich_query_memory.
                        let from_disp = mempill_types::time::format_valid_time_endpoint(
                            primary.valid_time.start,
                            primary.valid_time.start_granularity,
                        );
                        let until_disp = mempill_types::time::format_valid_time_endpoint(
                            primary.valid_time.end,
                            primary.valid_time.end_granularity,
                        );

                        let prov = provenance_label_to_str(&primary.provenance);
                        let cr = primary.claim_ref.0.to_string();
                        let conf_val = primary.confidence.value_confidence;

                        (val_str, from_disp, until_disp, prov, Some(cr), Some(conf_val))
                    } else {
                        // NoBelief — pick from alternatives if Contested.
                        let prov = belief.alternatives.first()
                            .map(|a| provenance_label_to_str(&a.provenance))
                            .unwrap_or_else(|| "none".to_string());
                        let cr = belief.alternatives.first().map(|a| a.claim_ref.0.to_string());
                        let conf_val = belief.alternatives.first().map(|a| a.confidence.value_confidence);
                        let val_str = belief.alternatives.first().map(|a| match &a.fact.value {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        });
                        (val_str, None, None, prov, cr, conf_val)
                    };

                Ok(SubjectFactEntry {
                    predicate,
                    value,
                    status: status_str.to_string(),
                    valid_from_display,
                    valid_until_display,
                    provenance: provenance_str,
                    claim_ref: claim_ref_str,
                    conf,
                })
            })
            .collect::<Result<Vec<_>, MemError>>()?;

        // Step 3: sort by predicate for stable, deterministic output.
        entries.sort_by(|a, b| a.predicate.cmp(&b.predicate));

        Ok(QuerySubjectResponse { entries })
    }

    /// Convenience wrapper that stamps now internally.
    pub fn execute(&self, req: QuerySubjectRequest) -> Result<QuerySubjectResponse, MemError> {
        self.execute_with_time(req, Utc::now())
    }
}

fn provenance_label_to_str(p: &ProvenanceLabel) -> String {
    match p {
        ProvenanceLabel::ModelDerived => "ModelDerived".to_string(),
        ProvenanceLabel::RecallReEntry => "RecallReEntry".to_string(),
        ProvenanceLabel::External(ExternalKind::UserAsserted) => "External/UserAsserted".to_string(),
        ProvenanceLabel::External(ExternalKind::ExternalFirstHand) => "External/ExternalFirstHand".to_string(),
        _ => "Unknown".to_string(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoOpVector;
    use crate::ports::persistence::Txn;
    use chrono::TimeZone;
    use mempill_types::{
        AgentId, Cardinality, Claim, ClaimEdge, ClaimRef, Confidence, Criticality,
        ExternalAnchor, ExternalKind, Fact, LedgerEntry, ProvenanceLabel, TransactionTime,
        ValidTime, ValidityAssertion,
    };
    use std::sync::Mutex;

    // ── MockTxn + FullMockStore ────────────────────────────────────────────────

    struct MockTxn(AgentId);
    impl Txn for MockTxn {
        fn agent_id(&self) -> &AgentId { &self.0 }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("mock")]
    struct MockErr;

    #[derive(Default)]
    struct FullMockStore {
        claims: Mutex<Vec<Claim>>,
        ledger: Mutex<Vec<LedgerEntry>>,
    }

    impl PersistencePort for FullMockStore {
        type Transaction = MockTxn;
        type Error = MockErr;

        fn begin_atomic(&self, aid: &AgentId) -> Result<MockTxn, MockErr> { Ok(MockTxn(aid.clone())) }
        fn append_claim(&self, _t: &mut MockTxn, c: &Claim) -> Result<ClaimRef, MockErr> {
            self.claims.lock().unwrap().push(c.clone());
            Ok(c.claim_ref().clone())
        }
        fn append_validity_assertion(&self, _t: &mut MockTxn, _a: &ValidityAssertion) -> Result<(), MockErr> { Ok(()) }
        fn append_ledger_entry(&self, _t: &mut MockTxn, e: &LedgerEntry) -> Result<(), MockErr> {
            self.ledger.lock().unwrap().push(e.clone()); Ok(())
        }
        fn append_claim_edge(&self, _t: &mut MockTxn, _e: &ClaimEdge) -> Result<(), MockErr> { Ok(()) }
        fn commit(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn rollback(&self, _t: MockTxn) -> Result<(), MockErr> { Ok(()) }
        fn load_subject_line(&self, _aid: &AgentId, subject: &str, predicate: &str, as_of: Option<DateTime<Utc>>) -> Result<Vec<Claim>, MockErr> {
            let claims = self.claims.lock().unwrap();
            Ok(claims.iter()
                .filter(|c| c.fact().subject == subject && c.fact().predicate == predicate)
                .filter(|c| as_of.is_none_or(|t| c.transaction_time().0 <= t))
                .cloned()
                .collect())
        }
        fn load_claim(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Option<Claim>, MockErr> { Ok(None) }
        fn load_validity_assertions_for(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ValidityAssertion>, MockErr> { Ok(vec![]) }
        fn load_ledger(&self, _aid: &AgentId, _from: Option<&TransactionTime>, _lim: usize) -> Result<Vec<LedgerEntry>, MockErr> { Ok(vec![]) }
        fn load_ledger_for_claims(&self, _aid: &AgentId, refs: &[ClaimRef], as_of: Option<DateTime<Utc>>) -> Result<Vec<LedgerEntry>, MockErr> {
            let ledger = self.ledger.lock().unwrap();
            Ok(ledger.iter()
                .filter(|e| refs.contains(&e.claim_ref))
                .filter(|e| as_of.is_none_or(|t| e.recorded_at.0 <= t))
                .cloned()
                .collect())
        }
        fn load_edges_for(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn load_injected_claims(&self, _aid: &AgentId) -> Result<Vec<ClaimRef>, MockErr> { Ok(vec![]) }
        fn load_lineage(&self, _aid: &AgentId, _r: &ClaimRef) -> Result<Vec<ClaimEdge>, MockErr> { Ok(vec![]) }
        fn list_predicates_for_subject(&self, _aid: &AgentId, subject: &str, as_of: Option<DateTime<Utc>>) -> Result<Vec<String>, MockErr> {
            let claims = self.claims.lock().unwrap();
            let mut predicates: Vec<String> = claims.iter()
                .filter(|c| c.fact().subject == subject)
                .filter(|c| as_of.is_none_or(|t| c.transaction_time().0 <= t))
                .map(|c| c.fact().predicate.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            predicates.sort();
            Ok(predicates)
        }
    }

    fn make_claim_at(subject: &str, predicate: &str, value: serde_json::Value, tx: DateTime<Utc>) -> Claim {
        Claim::new(
            ClaimRef::new_random(),
            AgentId("agent".into()),
            Fact { subject: subject.into(), predicate: predicate.into(), value },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime { start: None, end: None, valid_time_confidence: 0.0, start_granularity: None, end_granularity: None },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.0 },
            Criticality::Medium,
            vec![],
            None,
            None,
        )
    }

    // ── Test: query_subject returns all 3 predicates for alice-chen ────────────
    #[test]
    fn query_subject_returns_all_predicates() {
        use std::sync::Arc;
        let store = Arc::new(FullMockStore::default());
        let agent = AgentId("agent".into());
        let tx = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        // Seed 3 predicates for alice-chen
        store.claims.lock().unwrap().extend([
            make_claim_at("alice-chen", "city", serde_json::json!("Berlin"), tx),
            make_claim_at("alice-chen", "employer", serde_json::json!("Acme Corp"), tx),
            make_claim_at("alice-chen", "dietary", serde_json::json!("vegan"), tx),
        ]);

        let uc = QuerySubjectUseCase::new(Arc::clone(&store), None::<Arc<NoOpVector>>, EngineConfig::default());
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let req = QuerySubjectRequest {
            agent_id: agent,
            subject: "alice-chen".into(),
            valid_at: None,
            as_of_tx_time: None,
        };
        let resp = uc.execute_with_time(req, now).unwrap();

        assert_eq!(resp.entries.len(), 3, "expected 3 predicates, got {}", resp.entries.len());
        let preds: Vec<&str> = resp.entries.iter().map(|e| e.predicate.as_str()).collect();
        assert!(preds.contains(&"city"), "must contain city");
        assert!(preds.contains(&"employer"), "must contain employer");
        assert!(preds.contains(&"dietary"), "must contain dietary");

        // Sorted by predicate
        assert_eq!(preds, vec!["city", "dietary", "employer"], "entries must be sorted by predicate");
    }

    // ── Test: predicate ingested AFTER as_of_tx_time is EXCLUDED ──────────────
    #[test]
    fn query_subject_excludes_predicate_after_tx_cutoff() {
        use std::sync::Arc;
        let store = Arc::new(FullMockStore::default());
        let agent = AgentId("agent".into());

        let t1 = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap(); // after cutoff

        // city is visible at t1; phone is only added at t2 (after cutoff)
        store.claims.lock().unwrap().extend([
            make_claim_at("alice-chen", "city", serde_json::json!("Paris"), t1),
            make_claim_at("alice-chen", "phone", serde_json::json!("+1234"), t2),
        ]);

        let uc = QuerySubjectUseCase::new(Arc::clone(&store), None::<Arc<NoOpVector>>, EngineConfig::default());
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let cutoff = Utc.with_ymd_and_hms(2025, 3, 1, 0, 0, 0).unwrap(); // between t1 and t2

        let req = QuerySubjectRequest {
            agent_id: agent,
            subject: "alice-chen".into(),
            valid_at: None,
            as_of_tx_time: Some(cutoff),
        };
        let resp = uc.execute_with_time(req, now).unwrap();

        let preds: Vec<&str> = resp.entries.iter().map(|e| e.predicate.as_str()).collect();
        assert!(preds.contains(&"city"), "city must be present (before cutoff)");
        assert!(!preds.contains(&"phone"), "phone must be EXCLUDED (after cutoff)");
        assert_eq!(resp.entries.len(), 1);
    }

    // ── Test: valid_at narrows to the historical value for a predicate ─────────
    #[test]
    fn query_subject_valid_at_selects_historical_value() {
        use std::sync::Arc;
        let store = Arc::new(FullMockStore::default());
        let agent = AgentId("agent".into());

        // Alice lived in Berlin [2020,2023), then Paris [2023,∞).
        let tx = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        // Two claims with valid-time windows; use make_claim but set valid_time manually.
        let berlin = Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "alice-chen".into(), predicate: "city".into(), value: serde_json::json!("Berlin") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime {
                start: Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap()),
                end: Some(Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap()),
                valid_time_confidence: 0.9,
                start_granularity: None,
                end_granularity: None,
            },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            Criticality::Medium,
            vec![],
            None,
            None,
        );
        let paris = Claim::new(
            ClaimRef::new_random(),
            agent.clone(),
            Fact { subject: "alice-chen".into(), predicate: "city".into(), value: serde_json::json!("Paris") },
            Cardinality::Functional,
            ProvenanceLabel::External(ExternalKind::UserAsserted),
            ExternalAnchor { nearest_external_anchor: None, derivation_depth: 0 },
            TransactionTime(tx),
            ValidTime {
                start: Some(Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap()),
                end: None,
                valid_time_confidence: 0.9,
                start_granularity: None,
                end_granularity: None,
            },
            Confidence { value_confidence: 0.9, valid_time_confidence: 0.9 },
            Criticality::Medium,
            vec![],
            None,
            None,
        );
        store.claims.lock().unwrap().extend([berlin, paris]);

        let uc = QuerySubjectUseCase::new(Arc::clone(&store), None::<Arc<NoOpVector>>, EngineConfig::default());
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        // valid_at = 2021 → should resolve to Berlin
        let req = QuerySubjectRequest {
            agent_id: agent.clone(),
            subject: "alice-chen".into(),
            valid_at: Some(Utc.with_ymd_and_hms(2021, 6, 1, 0, 0, 0).unwrap()),
            as_of_tx_time: None,
        };
        let resp = uc.execute_with_time(req, now).unwrap();
        let city_entry = resp.entries.iter().find(|e| e.predicate == "city").unwrap();
        assert_eq!(city_entry.value.as_deref(), Some("Berlin"), "valid_at=2021 must resolve to Berlin");
        assert_eq!(city_entry.status, "Resolved");

        // valid_at = 2024 → should resolve to Paris
        let req2 = QuerySubjectRequest {
            agent_id: agent,
            subject: "alice-chen".into(),
            valid_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
            as_of_tx_time: None,
        };
        let resp2 = uc.execute_with_time(req2, now).unwrap();
        let city_entry2 = resp2.entries.iter().find(|e| e.predicate == "city").unwrap();
        assert_eq!(city_entry2.value.as_deref(), Some("Paris"), "valid_at=2024 must resolve to Paris");
    }

    // ── Test: Contested predicate reports status=Contested ────────────────────
    #[test]
    fn query_subject_contested_predicate_reports_contested() {
        use std::sync::Arc;
        use mempill_types::{Disposition, LedgerEventKind};

        let store = Arc::new(FullMockStore::default());
        let agent = AgentId("agent".into());
        let tx = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();

        // Two overlapping (Functional, no valid_time) claims for "employer" — contested scenario.
        let c1 = make_claim_at("alice-chen", "employer", serde_json::json!("Acme"), tx);
        let c2 = make_claim_at("alice-chen", "employer", serde_json::json!("GlobalCorp"), tx);
        let ref1 = c1.claim_ref().clone();
        let ref2 = c2.claim_ref().clone();
        store.claims.lock().unwrap().extend([c1, c2]);

        // Mark both as Contested in the ledger so the fold surfaces Contested.
        let le1 = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref1,
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::Contested,
            rationale: None,
            recorded_at: TransactionTime(tx),
        };
        let le2 = LedgerEntry {
            entry_id: uuid::Uuid::new_v4(),
            agent_id: agent.clone(),
            claim_ref: ref2,
            event_kind: LedgerEventKind::ClaimCommitted,
            disposition: Disposition::Contested,
            rationale: None,
            recorded_at: TransactionTime(tx),
        };
        store.ledger.lock().unwrap().extend([le1, le2]);

        let uc = QuerySubjectUseCase::new(Arc::clone(&store), None::<Arc<NoOpVector>>, EngineConfig::default());
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let req = QuerySubjectRequest {
            agent_id: agent,
            subject: "alice-chen".into(),
            valid_at: None,
            as_of_tx_time: None,
        };
        let resp = uc.execute_with_time(req, now).unwrap();
        let employer_entry = resp.entries.iter().find(|e| e.predicate == "employer").unwrap();
        assert_eq!(employer_entry.status, "Contested", "employer must be Contested");
    }
}
