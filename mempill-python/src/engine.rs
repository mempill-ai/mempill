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
    AuditQueryRequest, IngestClaimRequest, QueryHistoryRequest, QueryMemoryRequest, ReconcileRequest,
};
use mempill_sqlite::DefaultEngine;
use pyo3::prelude::*;
use pyo3::types::PyAny;
use pythonize::{depythonize, pythonize};

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
/// Obtain via `open_default(path)` or `open_in_memory()`. Thread-safe (Arc-backed).
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
    /// Returns a dict with `belief`.
    #[pyo3(signature = (request))]
    fn query_memory<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: QueryMemoryRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.query_memory(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp)?)
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
    /// `provenance`, `value_confidence`, and `claim_ref`.
    #[pyo3(signature = (request))]
    fn query_history<'py>(&self, py: Python<'py>, request: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let req: QueryHistoryRequest = depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py.detach(|| runtime().block_on(engine.query_history(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize(py, &resp)?)
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
}

// ── Module-level constructors ─────────────────────────────────────────────────

/// Open a file-backed mempill engine at `path`.
///
/// Raises `StorageError` if the database cannot be opened or migrations fail.
#[pyfunction]
#[pyo3(signature = (path))]
pub fn open_default(path: &str) -> PyResult<PyEngine> {
    mempill_sqlite::open_default(path)
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
