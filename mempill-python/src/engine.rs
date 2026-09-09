//! PyEngine — sync wrapper around DefaultEngine via a static Tokio runtime.
//!
//! Uses a `OnceLock<tokio::runtime::Runtime>` (multi-thread) initialised once
//! per process. Each sync method releases the GIL via `py.detach(|| …)` (the
//! PyO3 0.29 name for allow_threads) before calling `runtime().block_on(…)`.
//!
//! All request/response conversion is done via `pythonize::{depythonize, pythonize}`.
//! The serde adjacently-tagged enums (ProvenanceLabel, Disposition, etc.) round-trip
//! correctly because W1 added the required `#[serde(tag="type", content="kind")]`
//! annotations to `mempill-types`.

use std::sync::OnceLock;

use mempill_core::application::dto::{
    AssertValidityRequest, AuditQueryRequest, IngestClaimRequest, LiveClaimResolution,
    QueryHistoryRequest, QueryMemoryRequest, QuerySubjectRequest, ReconcileRequest,
};
use mempill_sqlite::DefaultEngine;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use pythonize::{depythonize, pythonize};

use crate::display::{enrich_query_history, enrich_query_memory};
use crate::errors::{mem_err_to_pyerr, StorageError, ValidationError};

// ── Static runtime ────────────────────────────────────────────────────────────

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("mempill: failed to build tokio runtime")
    })
}

// ── PyEngine ──────────────────────────────────────────────────────────────────

/// Sync Python handle to a mempill DefaultEngine (SQLite, no oracle, no vector).
///
/// Obtain via `open_default_for_agent(base_dir, agent_id)` or `open_in_memory()`.
/// Thread-safe (Arc-backed).
#[pyclass(name = "PyEngine")]
pub struct PyEngine {
    engine: DefaultEngine,
}

#[pymethods]
impl PyEngine {
    /// Ingest a claim into memory.
    ///
    /// `request` must be a dict matching the `IngestClaimRequest` schema:
    ///   agent_id, subject, predicate, value, provenance, cardinality,
    ///   valid_time (optional), confidence, criticality, derived_from.
    ///
    /// Returns a dict with `claim_ref`, `disposition`, and `contested_with`.
    #[pyo3(signature = (request))]
    fn ingest_claim<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: IngestClaimRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.ingest_claim(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp)?)
    }

    /// Query the current belief for a (subject, predicate) pair.
    ///
    /// `request` must be a dict with: agent_id, subject, predicate,
    /// as_of_tx_time (optional ISO-8601 string).
    ///
    /// Returns a dict with `belief`. Each belief slot includes:
    ///   - `valid_from_display`: start of the valid-time window rendered at its
    ///     recorded precision (e.g. `"2020-03"` for Month, `"2020"` for Year,
    ///     `"2020-03-15"` for Day/Instant). Absent when the start is unknown.
    ///   - `valid_until_display`: same for the end endpoint. Absent when open-ended.
    ///   - `valid_time.start_granularity`: raw granularity string (`"year"`,
    ///     `"month"`, `"day"`, `"instant"`) when set, otherwise absent.
    ///   - `valid_time.end_granularity`: same for the end endpoint.
    #[pyo3(signature = (request))]
    fn query_memory<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: QueryMemoryRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.query_memory(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &enrich_query_memory(resp))?)
    }

    /// Reconcile one or more subject lines for an agent.
    ///
    /// `request` must be a dict with: agent_id, subject_lines (list of [subject, predicate] pairs).
    ///
    /// Returns a dict with `outcomes` and `oracle_escalations`.
    #[pyo3(signature = (request))]
    fn reconcile<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: ReconcileRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.reconcile(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp)?)
    }

    /// Query the full history timeline for a (subject, predicate) subject-line.
    ///
    /// `request` must be a dict with: agent_id, subject, predicate.
    ///
    /// Returns a dict with `entries` — all claims ordered oldest→newest, each tagged
    /// with `status` ("Current" or "Superseded"), `value`, `valid_from`, `valid_until`,
    /// `provenance`, `value_confidence`, and `claim_ref`. Each entry also includes:
    ///   - `valid_from_display` / `valid_until_display`: pre-rendered display strings at
    ///     the recorded precision, identical rendering to `query_memory` (e.g. `"2020-03"`
    ///     for Month, `"2020"` for Year). Absent when the corresponding endpoint is
    ///     unknown/open.
    ///   - `valid_from_granularity` / `valid_until_granularity`: raw granularity strings
    ///     (`"year"`, `"month"`, `"day"`, `"instant"`) when set, otherwise absent.
    ///     `valid_until_granularity` is a DERIVED value — it reflects the SUCCESSOR
    ///     claim's `start_granularity` (the honest source of the bounding instant), not
    ///     this entry's own end_granularity. See `mempill_core::application::dto::HistoryEntry`.
    #[pyo3(signature = (request))]
    fn query_history<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: QueryHistoryRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.query_history(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &enrich_query_history(resp))?)
    }

    /// Query the audit ledger for an agent (optionally filtered by claim_ref / tx_time window).
    ///
    /// `request` must be a dict with: agent_id, claim_ref (optional UUID string),
    /// from_tx_time (optional ISO-8601), limit (int).
    ///
    /// Returns a dict with `entries`.
    #[pyo3(signature = (request))]
    fn query_audit<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: AuditQueryRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.query_audit(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp)?)
    }

    /// Query all resolved beliefs for every predicate stored under a subject.
    ///
    /// `request` must be a dict with:
    ///   - `agent_id`        — str
    ///   - `subject`         — str
    ///   - `valid_at`        — optional ISO-8601 string (valid-time axis)
    ///   - `as_of_tx_time`   — optional ISO-8601 string (tx-time axis)
    ///
    /// Returns a list of dicts, one per distinct predicate, each with:
    ///   predicate, value (str|null), status, valid_from_display (str|null),
    ///   valid_until_display (str|null), provenance, claim_ref (str|null), conf (float|null).
    /// Entries are sorted by predicate (alphabetical).
    #[pyo3(signature = (request))]
    fn query_subject<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: QuerySubjectRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.query_subject(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp.entries)?)
    }

    /// Bound or reopen a claim's valid-time window (SDK_CONTRACT.md §3.1 `assert_validity`,
    /// TASK-33 E2) — the host-facing, oracle-free path to `Superseded`/`Reinstated`.
    ///
    /// `request` must be a dict with:
    ///   - `agent_id`  — str
    ///   - `target`    — claim_ref UUID string
    ///   - `assertion` — `{"type": "Bound", "value": {"at": "<RFC3339>"}}` or `{"type": "Reopen"}`
    ///   - `provenance` — must be `External(*)`; any other channel raises `ValidationError`
    ///   - `confidence` — `{"value_confidence": float, "valid_time_confidence": float}`
    ///
    /// Returns a dict with `claim_ref`, `assertion_ref` (nullable), `kind`, `effective_at`
    /// (nullable RFC3339 string), `disposition`, `no_op`.
    ///
    /// Prefer `mempill.ergonomic.end_fact()` unless you already hold the target `claim_ref`.
    #[pyo3(signature = (request))]
    fn assert_validity<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: AssertValidityRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.assert_validity(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp)?)
    }

    /// Resolve a (subject, predicate) subject-line to the single live claim, if any —
    /// the SAME canonical fold `query_memory`/`query_history` use (I8 single source of
    /// truth; never a heuristic re-derivation). Backs `mempill.ergonomic.end_fact()`.
    ///
    /// Returns a dict: `{"status": "empty"|"single"|"ambiguous", "claim_ref": str|None,
    /// "live_count": int|None}`. `live_count` is populated only for `"ambiguous"`.
    #[pyo3(signature = (agent_id, subject, predicate))]
    fn resolve_live_claim_for_line<'py>(
        &self,
        py: Python<'py>,
        agent_id: String,
        subject: String,
        predicate: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let engine = self.engine.clone();
        let resolution = py
            .detach(|| {
                runtime().block_on(engine.resolve_live_claim_for_line(
                    mempill_types::AgentId(agent_id),
                    subject,
                    predicate,
                ))
            })
            .map_err(mem_err_to_pyerr)?;

        let dict = pyo3::types::PyDict::new(py);
        match resolution {
            LiveClaimResolution::Empty => {
                dict.set_item("status", "empty")?;
                dict.set_item("claim_ref", py.None())?;
                dict.set_item("live_count", py.None())?;
            }
            LiveClaimResolution::Single(claim_ref) => {
                dict.set_item("status", "single")?;
                dict.set_item("claim_ref", claim_ref.0.to_string())?;
                dict.set_item("live_count", py.None())?;
            }
            LiveClaimResolution::Ambiguous(n) => {
                dict.set_item("status", "ambiguous")?;
                dict.set_item("claim_ref", py.None())?;
                dict.set_item("live_count", n)?;
            }
        }
        Ok(dict.into_any())
    }
}

// ── Module-level constructors ─────────────────────────────────────────────────

/// Open a file-backed, per-agent mempill engine under `base_dir`.
///
/// The database file is derived automatically as `base_dir/agent_{agent_id}.db` — there
/// is no way to point two different `agent_id`s at the same file through this API.
///
/// Raises `StorageError` if `agent_id` contains characters that could cause a filename
/// collision, if the database cannot be opened, or if migrations fail.
#[pyfunction]
#[pyo3(signature = (base_dir, agent_id))]
pub fn open_default_for_agent(base_dir: &str, agent_id: &str) -> PyResult<PyEngine> {
    mempill_sqlite::open_default_for_agent(std::path::Path::new(base_dir), agent_id)
        .map(|engine| PyEngine { engine })
        .map_err(|e| StorageError::new_err(e.to_string()))
}

/// Open an in-memory mempill engine (ephemeral; useful for tests and MCP sessions).
///
/// Raises `StorageError` if initialisation fails.
#[pyfunction]
#[pyo3(signature = ())]
pub fn open_in_memory() -> PyResult<PyEngine> {
    mempill_sqlite::open_default_in_memory()
        .map(|engine| PyEngine { engine })
        .map_err(|e| StorageError::new_err(e.to_string()))
}

// ── Rust unit tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    /// Verify the Tokio runtime initialises without panicking.
    #[test]
    fn runtime_initialises_once() {
        let rt1 = runtime() as *const _;
        let rt2 = runtime() as *const _;
        assert_eq!(rt1, rt2, "OnceLock must return the same runtime pointer");
    }

    /// Verify open_in_memory returns a PyEngine.
    #[test]
    fn open_in_memory_returns_engine() {
        Python::initialize();
        Python::attach(|_py| {
            let result = open_in_memory();
            assert!(result.is_ok(), "open_in_memory must succeed");
        });
    }

    /// Round-trip an IngestClaimRequest dict through depythonize → serde → pythonize.
    #[test]
    fn ingest_claim_request_dto_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            let req_json = serde_json::json!({
                "agent_id": "test-agent",
                "subject": "user",
                "predicate": "city",
                "value": "Berlin",
                "provenance": {"type": "External", "kind": "UserAsserted"},
                "cardinality": "Functional",
                "valid_time": null,
                "confidence": {"value_confidence": 0.95, "valid_time_confidence": 0.0},
                "criticality": "Medium",
                "derived_from": []
            });
            let py_dict = pythonize(py, &req_json).expect("pythonize json");
            let req: IngestClaimRequest = depythonize(&py_dict)
                .expect("depythonize IngestClaimRequest");
            assert_eq!(req.agent_id.0, "test-agent");
            assert_eq!(req.subject, "user");
            assert_eq!(req.predicate, "city");
            assert_eq!(req.value, serde_json::json!("Berlin"));
            assert!(matches!(
                req.provenance,
                mempill_types::ProvenanceLabel::External(mempill_types::ExternalKind::UserAsserted)
            ));
            // Re-serialize and verify ProvenanceLabel adjacently-tagged shape
            let back = serde_json::to_value(&req.provenance).unwrap();
            assert_eq!(back["type"], "External");
            assert_eq!(back["kind"], "UserAsserted");
        });
    }

    /// Verify ClaimRef serializes as a bare UUID string (serde transparent).
    #[test]
    fn claim_ref_serializes_as_bare_uuid() {
        let uuid = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let claim_ref = mempill_types::ClaimRef(uuid);
        let json = serde_json::to_string(&claim_ref).unwrap();
        assert_eq!(json, r#""550e8400-e29b-41d4-a716-446655440000""#);
    }

    /// Verify QueryMemoryRequest round-trip.
    #[test]
    fn query_memory_request_dto_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            let req_json = serde_json::json!({
                "agent_id": "test-agent",
                "subject": "user",
                "predicate": "city",
                "as_of_tx_time": null
            });
            let py_dict = pythonize(py, &req_json).expect("pythonize json");
            let req: QueryMemoryRequest = depythonize(&py_dict)
                .expect("depythonize QueryMemoryRequest");
            assert_eq!(req.agent_id.0, "test-agent");
            assert!(req.as_of_tx_time.is_none());
        });
    }

    /// Verify ReconcileRequest round-trip (subject_lines as list of [subject, predicate]).
    #[test]
    fn reconcile_request_dto_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            let req_json = serde_json::json!({
                "agent_id": "test-agent",
                "subject_lines": [["user", "city"]]
            });
            let py_dict = pythonize(py, &req_json).expect("pythonize json");
            let req: ReconcileRequest = depythonize(&py_dict)
                .expect("depythonize ReconcileRequest");
            assert_eq!(req.agent_id.0, "test-agent");
            assert_eq!(req.subject_lines.len(), 1);
            assert_eq!(req.subject_lines[0], ("user".to_string(), "city".to_string()));
        });
    }

    /// Verify valid_at depythonize: string ISO-8601 date → Option<DateTime<Utc>>.
    /// This test catches the serde(default) + Option<DateTime<Utc>> silent-ignore bug.
    #[test]
    fn query_memory_request_valid_at_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            // Test: valid_at as an ISO-8601 string should deserialize to Some(DateTime)
            let req_json = serde_json::json!({
                "agent_id": "test-agent",
                "subject": "user",
                "predicate": "city",
                "valid_at": "2021-06-01T00:00:00Z"
            });
            let py_dict = pythonize(py, &req_json).expect("pythonize json");
            let req: QueryMemoryRequest = depythonize(&py_dict)
                .expect("depythonize QueryMemoryRequest with valid_at");
            assert!(
                req.valid_at.is_some(),
                "valid_at='2021-06-01T00:00:00Z' must deserialize to Some(DateTime), got None"
            );
            let vat = req.valid_at.unwrap();
            // Verify it's in 2021 by comparing timestamps
            let expected: mempill_core::application::dto::QueryMemoryRequest = serde_json::from_str(
                r#"{"agent_id":"test-agent","subject":"user","predicate":"city","valid_at":"2021-06-01T00:00:00Z"}"#
            ).unwrap();
            assert_eq!(vat, expected.valid_at.unwrap(), "valid_at must match 2021-06-01T00:00:00Z");

            // Test: valid_at=null should deserialize to None
            let req_null = serde_json::json!({
                "agent_id": "test-agent",
                "subject": "user",
                "predicate": "city",
                "valid_at": null
            });
            let py_dict_null = pythonize(py, &req_null).expect("pythonize json null");
            let req_null: QueryMemoryRequest = depythonize(&py_dict_null)
                .expect("depythonize QueryMemoryRequest with valid_at=null");
            assert!(req_null.valid_at.is_none(), "valid_at=null must deserialize to None");

            // Test: omitted valid_at (serde default) → None
            let req_omit = serde_json::json!({
                "agent_id": "test-agent",
                "subject": "user",
                "predicate": "city"
            });
            let py_dict_omit = pythonize(py, &req_omit).expect("pythonize json omit");
            let req_omit: QueryMemoryRequest = depythonize(&py_dict_omit)
                .expect("depythonize QueryMemoryRequest without valid_at");
            assert!(req_omit.valid_at.is_none(), "absent valid_at must default to None");
        });
    }

    /// Verify valid_at depythonize from a NATIVELY constructed Python dict.
    /// This is the exact path that Python callers use (not via pythonize(serde_json::Value)).
    #[test]
    fn query_memory_valid_at_native_pydict() {
        Python::initialize();
        Python::attach(|py| {
            use pyo3::types::{PyDict, PyString};
            // Build the dict exactly as Python code would: dict["valid_at"] = "2021-06-01T00:00:00Z"
            let py_dict = PyDict::new(py);
            py_dict.set_item("agent_id", "test-agent").unwrap();
            py_dict.set_item("subject", "user").unwrap();
            py_dict.set_item("predicate", "city").unwrap();
            py_dict.set_item("valid_at", PyString::new(py, "2021-06-01T00:00:00Z")).unwrap();

            let result = depythonize::<QueryMemoryRequest>(py_dict.as_any());
            match &result {
                Ok(req) => {
                    assert!(
                        req.valid_at.is_some(),
                        "FAIL: valid_at='2021-06-01T00:00:00Z' from native PyDict must be Some(DateTime), got None"
                    );
                    eprintln!("PASS: valid_at from native PyDict = {:?}", req.valid_at);
                }
                Err(e) => {
                    panic!("FAIL: depythonize from native PyDict raised error: {e}");
                }
            }
        });
    }

    /// Full engine integration test: ingest Alice+Bob succession, query with valid_at.
    /// Mirrors the Python test scenario that was failing.
    #[test]
    fn valid_at_succession_fold_via_pyengine() {
        Python::initialize();
        Python::attach(|py| {
            use pyo3::types::{PyDict, PyList, PyString};

            let engine = open_in_memory().expect("open_in_memory");

            // Helper to build ingest request dict
            fn make_ingest_dict<'py>(
                py: Python<'py>,
                agent: &str, subject: &str, pred: &str, value: &str,
                start: &str, end: Option<&str>,
            ) -> pyo3::Bound<'py, PyDict> {
                let d = PyDict::new(py);
                d.set_item("agent_id", agent).unwrap();
                d.set_item("subject", subject).unwrap();
                d.set_item("predicate", pred).unwrap();
                d.set_item("value", value).unwrap();

                // provenance = {"type": "External", "kind": "ExternalFirstHand"}
                let prov = PyDict::new(py);
                prov.set_item("type", "External").unwrap();
                prov.set_item("kind", "ExternalFirstHand").unwrap();
                d.set_item("provenance", prov).unwrap();
                d.set_item("cardinality", "Functional").unwrap();

                // valid_time
                let vt = PyDict::new(py);
                vt.set_item("start", start).unwrap();
                if let Some(e) = end {
                    vt.set_item("end", e).unwrap();
                }
                vt.set_item("valid_time_confidence", 0.99f64).unwrap();
                d.set_item("valid_time", vt).unwrap();

                // confidence
                let conf = PyDict::new(py);
                conf.set_item("value_confidence", 0.99f64).unwrap();
                conf.set_item("valid_time_confidence", 0.99f64).unwrap();
                d.set_item("confidence", conf).unwrap();

                d.set_item("criticality", "High").unwrap();
                d.set_item("derived_from", PyList::empty(py)).unwrap();
                d
            }

            // Ingest Alice valid [2020-01-01, 2022-01-01)
            let alice_dict = make_ingest_dict(py, "test-agent", "x", "y", "Alice",
                "2020-01-01T00:00:00Z", Some("2022-01-01T00:00:00Z"));
            engine.ingest_claim(py, alice_dict.as_any()).expect("ingest Alice");

            // Ingest Bob valid [2022-01-01, open)
            let bob_dict = make_ingest_dict(py, "test-agent", "x", "y", "Bob",
                "2022-01-01T00:00:00Z", None);
            engine.ingest_claim(py, bob_dict.as_any()).expect("ingest Bob");

            // Query with valid_at=2021-06-01 → should return Alice
            let q_alice = PyDict::new(py);
            q_alice.set_item("agent_id", "test-agent").unwrap();
            q_alice.set_item("subject", "x").unwrap();
            q_alice.set_item("predicate", "y").unwrap();
            q_alice.set_item("valid_at", PyString::new(py, "2021-06-01T00:00:00Z")).unwrap();

            let resp_alice = engine.query_memory(py, q_alice.as_any()).expect("query Alice");
            let resp_json: serde_json::Value = depythonize(&resp_alice).expect("depythonize resp");
            let value_alice = resp_json["belief"]["primary"]["fact"]["value"].as_str();
            eprintln!("valid_at=2021 → {value_alice:?}");
            assert_eq!(
                value_alice,
                Some("Alice"),
                "valid_at=2021-06-01 must return Alice, got: {value_alice:?}"
            );

            // Query with valid_at=2023-01-01 → should return Bob
            let q_bob = PyDict::new(py);
            q_bob.set_item("agent_id", "test-agent").unwrap();
            q_bob.set_item("subject", "x").unwrap();
            q_bob.set_item("predicate", "y").unwrap();
            q_bob.set_item("valid_at", PyString::new(py, "2023-01-01T00:00:00Z")).unwrap();

            let resp_bob = engine.query_memory(py, q_bob.as_any()).expect("query Bob");
            let resp_json_b: serde_json::Value = depythonize(&resp_bob).expect("depythonize resp bob");
            let value_bob = resp_json_b["belief"]["primary"]["fact"]["value"].as_str();
            eprintln!("valid_at=2023 → {value_bob:?}");
            assert_eq!(
                value_bob,
                Some("Bob"),
                "valid_at=2023-01-01 must return Bob, got: {value_bob:?}"
            );
        });
    }

    /// Verify AuditQueryRequest round-trip.
    #[test]
    fn audit_query_request_dto_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            let req_json = serde_json::json!({
                "agent_id": "test-agent",
                "claim_ref": null,
                "from_tx_time": null,
                "limit": 50
            });
            let py_dict = pythonize(py, &req_json).expect("pythonize json");
            let req: AuditQueryRequest = depythonize(&py_dict)
                .expect("depythonize AuditQueryRequest");
            assert_eq!(req.agent_id.0, "test-agent");
            assert_eq!(req.limit, 50);
            assert!(req.claim_ref.is_none());
        });
    }
}
