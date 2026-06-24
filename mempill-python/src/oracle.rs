//! PyOracleBridge — bridges a Python oracle object to `OraclePort`.
//!
//! # GIL / re-entrancy contract
//!
//! `request_adjudication` is invoked from a `spawn_blocking` thread DURING ingest.
//! The outer Python-facing engine methods (PyOracleEngine) MUST release the GIL
//! via `py.detach(|| runtime().block_on(...))` BEFORE that block_on runs —
//! exactly the same pattern as PyEngine — so that the inner `Python::with_gil`
//! inside `request_adjudication` can re-acquire it without deadlock.
//!
//! # Python oracle protocol (duck-typed)
//!
//! ```python
//! class MyOracle:
//!     def request_adjudication(self, agent_id: str, request: dict) -> str:
//!         """
//!         Called by the engine whenever a conflict requires oracle arbitration.
//!
//!         Parameters
//!         ----------
//!         agent_id : str
//!             The agent whose belief store is being written to.
//! 	    request : dict
//!             AdjudicationRequest dict with keys:
//!               subject_line  — dict with agent_id, subject, predicate
//!               incumbent     — Belief dict (current winner)
//!               challenger    — Claim dict (the new conflicting claim)
//!               criticality   — str ("Low"|"Medium"|"High"|"Critical")
//!               reason        — str (e.g. "ExternalContradiction")
//!
//!         Returns
//!         -------
//!         str
//!             A UUID-formatted string used as the handle_id for correlation.
//!             Store this alongside the request so you can later call
//!             engine.submit_adjudication({"handle_id": <this uuid>, ...}).
//!         """
//!         ...
//! ```

use std::sync::Arc;

use mempill_core::ports::OraclePort;
use mempill_types::{AgentId, AdjudicationRequest};
use pyo3::prelude::*;
use pythonize::pythonize;

// ── Error type for the bridge ─────────────────────────────────────────────────

/// Error emitted when the Python oracle call fails.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("Python oracle raised an exception: {0}")]
    PythonException(String),
    #[error("Oracle returned an invalid handle UUID: {0}")]
    InvalidHandle(String),
    #[error("Serialisation error passing request to Python oracle: {0}")]
    Serialise(String),
}

// ── PyOracleBridge ────────────────────────────────────────────────────────────

/// Wraps a Python oracle object (duck-typed: must have `request_adjudication`).
///
/// Holds the Python object as `Arc<Py<PyAny>>` so it is `Send + Sync` and
/// can be moved across `spawn_blocking` thread boundaries.
#[derive(Clone)]
pub struct PyOracleBridge {
    oracle: Arc<Py<PyAny>>,
}

impl PyOracleBridge {
    pub fn new(oracle: Py<PyAny>) -> Self {
        Self { oracle: Arc::new(oracle) }
    }
}

impl OraclePort for PyOracleBridge {
    type Error = BridgeError;
    type Handle = uuid::Uuid;

    /// Calls `oracle.request_adjudication(agent_id_str, request_dict)` and
    /// returns the handle UUID. Acquires the GIL, calls Python, releases GIL.
    ///
    /// This runs inside `spawn_blocking` — the outer PyOracleEngine methods
    /// MUST have released the GIL before entering `block_on` so this
    /// `Python::with_gil` can reacquire it without deadlock.
    fn request_adjudication(
        &self,
        agent_id: &AgentId,
        request: AdjudicationRequest,
    ) -> Result<Self::Handle, Self::Error> {
        let oracle = Arc::clone(&self.oracle);
        let agent_id_str = agent_id.0.clone();

        Python::attach(|py| {
            // Pythonize the AdjudicationRequest into a dict.
            let request_dict = pythonize(py, &request)
                .map_err(|e| BridgeError::Serialise(e.to_string()))?;

            // Call the Python oracle.
            let result = oracle
                .call_method1(py, "request_adjudication", (agent_id_str, request_dict))
                .map_err(|e| BridgeError::PythonException(e.to_string()))?;

            // Extract the returned handle as a UUID string.
            let handle_str: String = result
                .extract(py)
                .map_err(|e: pyo3::PyErr| BridgeError::PythonException(e.to_string()))?;

            uuid::Uuid::parse_str(&handle_str)
                .map_err(|e| BridgeError::InvalidHandle(format!("{e}: got {handle_str:?}")))
        })
    }

    /// Identity — the opaque handle IS the durable UUID.
    fn handle_to_uuid(handle: &Self::Handle) -> uuid::Uuid {
        *handle
    }
}

// ── PyOracleEngine ────────────────────────────────────────────────────────────

use std::sync::OnceLock;

use mempill_core::application::dto::{
    AuditQueryRequest, IngestClaimRequest, QueryMemoryRequest, ReconcileRequest,
};
use mempill_sqlite::OracleEngine;
use mempill_types::AdjudicationResponse;

use crate::errors::{mem_err_to_pyerr, StorageError, ValidationError};

static ORACLE_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn oracle_runtime() -> &'static tokio::runtime::Runtime {
    ORACLE_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("mempill: failed to build oracle tokio runtime")
    })
}

/// Sync Python handle to a mempill OracleEngine (SQLite, Python oracle, no vector).
///
/// Obtain via `open_with_oracle(path, oracle)` or `open_with_oracle_in_memory(oracle)`.
/// The `oracle` argument is any Python object with a `request_adjudication` method
/// matching the duck-typed protocol documented in this module.
///
/// # GIL contract
///
/// All methods release the GIL via `py.detach(|| ...)` before entering
/// `block_on`. The Python oracle's `request_adjudication` then reacquires the
/// GIL inside `Python::with_gil` on the `spawn_blocking` thread.
#[pyclass(name = "PyOracleEngine")]
pub struct PyOracleEngine {
    engine: OracleEngine<PyOracleBridge>,
}

#[pymethods]
impl PyOracleEngine {
    /// Ingest a claim into memory.
    ///
    /// Behaves identically to `PyEngine.ingest_claim`. When the claim conflicts
    /// with an incumbent, the engine calls the Python oracle's
    /// `request_adjudication` and records a `QueuedForAdjudication` disposition.
    ///
    /// Returns a dict with `claim_ref`, `disposition`, and `contested_with`.
    #[pyo3(signature = (request))]
    fn ingest_claim<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: IngestClaimRequest = pythonize::depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py
            .detach(|| oracle_runtime().block_on(engine.ingest_claim(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize::pythonize(py, &resp)?)
    }

    /// Query the current belief for a (subject, predicate) pair.
    ///
    /// Returns a dict with `belief`.
    #[pyo3(signature = (request))]
    fn query_memory<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: QueryMemoryRequest = pythonize::depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py
            .detach(|| oracle_runtime().block_on(engine.query_memory(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize::pythonize(py, &resp)?)
    }

    /// Reconcile one or more subject lines for an agent.
    ///
    /// Returns a dict with `outcomes` and `oracle_escalations`.
    #[pyo3(signature = (request))]
    fn reconcile<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: ReconcileRequest = pythonize::depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py
            .detach(|| oracle_runtime().block_on(engine.reconcile(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize::pythonize(py, &resp)?)
    }

    /// Query the audit ledger for an agent.
    ///
    /// Returns a dict with `entries`.
    #[pyo3(signature = (request))]
    fn query_audit<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: AuditQueryRequest = pythonize::depythonize(request)
            .map_err(|e| ValidationError::new_err(format!("bad request: {e}")))?;
        let engine = self.engine.clone();
        let resp = py
            .detach(|| oracle_runtime().block_on(engine.query_audit(req)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize::pythonize(py, &resp)?)
    }

    /// Submit an adjudication verdict back into the engine.
    ///
    /// `response_dict` must match the `AdjudicationResponse` shape:
    ///   ```python
    ///   {
    ///       "handle_id": "<uuid string>",          # from the handle returned by your oracle
    ///       "verdict": "Affirm" | "Deny" | "Unknown",
    ///       "evidence_provenance": {...},           # ProvenanceLabel dict
    ///   }
    ///   ```
    ///
    /// Returns an `AdjudicationOutcome` dict:
    ///   ```python
    ///   {"handle_id": "<uuid>", "disposition": "<Disposition>", "claim_ref": "<uuid>"}
    ///   ```
    ///
    /// Raises:
    ///   `NotFoundError` — if the handle is unknown, already resolved, or expired.
    ///   `StorageError`  — if a persistence error occurs.
    #[pyo3(signature = (response_dict))]
    fn submit_adjudication<'py>(
        &self,
        py: Python<'py>,
        response_dict: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Depythonize the full response (handle_id + verdict + evidence_provenance).
        let resp: AdjudicationResponse = pythonize::depythonize(response_dict)
            .map_err(|e| ValidationError::new_err(format!("bad response: {e}")))?;

        let handle_id = resp.handle_id;
        let engine = self.engine.clone();
        let outcome = py
            .detach(|| oracle_runtime().block_on(engine.submit_adjudication(handle_id, resp)))
            .map_err(mem_err_to_pyerr)?;
        Ok(pythonize::pythonize(py, &outcome)?)
    }

    /// Sweep all expired pending-adjudication rows.
    ///
    /// Call periodically to revert `QueuedForAdjudication` claims whose TTL has
    /// elapsed without a verdict being submitted. Each swept claim transitions to
    /// `Contested`.
    ///
    /// Returns the number of claims reverted.
    #[pyo3(signature = ())]
    fn sweep_expired_adjudications(&self, py: Python<'_>) -> PyResult<usize> {
        let engine = self.engine.clone();
        let count = py
            .detach(|| oracle_runtime().block_on(engine.sweep_expired_adjudications()))
            .map_err(mem_err_to_pyerr)?;
        Ok(count)
    }

    /// List all pending-adjudication rows awaiting human resolution.
    ///
    /// Returns a list of dicts. Each dict has the shape (Python pseudocode):
    ///
    /// ```text
    /// {
    ///     "handle_id":        str,   # UUID of the adjudication handle
    ///     "agent_id":         str,
    ///     "subject":          str,
    ///     "predicate":        str,
    ///     "incumbent_value":  Any,   # JSON value from AdjudicationRequest.incumbent.fact.value
    ///     "challenger_value": Any,   # JSON value from AdjudicationRequest.challenger.fact.value
    ///     "queued_at":        str,   # RFC-3339
    ///     "expires_at":       str | None,
    ///     "status":           str,   # "pending" | "resolved" | "expired"
    ///     "request_payload":  dict,  # full AdjudicationRequest for oracle context
    /// }
    /// ```
    ///
    /// When `agent_id` is `None` (the default), rows for ALL agents are returned.
    /// Rows are ordered by `queued_at ASC` (oldest first).
    ///
    /// Raises `StorageError` if a persistence error occurs.
    #[pyo3(signature = (agent_id=None))]
    fn list_pending_adjudications<'py>(
        &self,
        py: Python<'py>,
        agent_id: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let filter = agent_id.map(|s| mempill_types::AgentId(s.to_owned()));
        let engine = self.engine.clone();
        let rows = py
            .detach(|| oracle_runtime().block_on(engine.list_pending_adjudications(filter)))
            .map_err(mem_err_to_pyerr)?;

        // Build a Python list of dicts.
        let list = pyo3::types::PyList::empty(py);
        for row in rows {
            // Decode incumbent_value and challenger_value from the request_payload.
            let incumbent_value = row.request_payload.incumbent.fact.value.clone();
            let challenger_value = row.request_payload.challenger.fact().value.clone();

            let d = pyo3::types::PyDict::new(py);
            d.set_item("handle_id", row.handle_id.to_string())?;
            d.set_item("agent_id", row.agent_id.0.as_str())?;
            d.set_item("subject", row.subject.as_str())?;
            d.set_item("predicate", row.predicate.as_str())?;
            d.set_item("incumbent_value", pythonize(py, &incumbent_value)?)?;
            d.set_item("challenger_value", pythonize(py, &challenger_value)?)?;
            d.set_item("queued_at", row.queued_at.to_rfc3339())?;
            d.set_item(
                "expires_at",
                row.expires_at.map(|t| t.to_rfc3339()),
            )?;
            d.set_item("status", row.status.as_str())?;
            d.set_item("request_payload", pythonize(py, &row.request_payload)?)?;
            list.append(d)?;
        }
        Ok(list.into_any())
    }
}

// ── Module-level constructors ─────────────────────────────────────────────────

/// Open a file-backed mempill engine wired to a Python oracle.
///
/// The `oracle` argument must be any Python object with a method:
/// ```python
/// def request_adjudication(self, agent_id: str, request: dict) -> str: ...
/// ```
/// The returned string must be a UUID that your oracle stores so it can later
/// call `engine.submit_adjudication({"handle_id": ..., "verdict": ..., ...})`.
///
/// Raises `StorageError` if the database cannot be opened or migrations fail.
#[pyo3::pyfunction]
#[pyo3(signature = (path, oracle))]
pub fn open_with_oracle(py: Python<'_>, path: &str, oracle: Py<PyAny>) -> PyResult<PyOracleEngine> {
    let bridge = Arc::new(PyOracleBridge::new(oracle));
    let engine = py
        .detach(|| mempill_sqlite::open_with_oracle(path, bridge))
        .map_err(|e| StorageError::new_err(e.to_string()))?;
    Ok(PyOracleEngine { engine })
}

/// Open an in-memory mempill engine wired to a Python oracle.
///
/// The `oracle` argument must be any Python object with a method:
/// ```python
/// def request_adjudication(self, agent_id: str, request: dict) -> str: ...
/// ```
///
/// Raises `StorageError` if initialisation fails.
#[pyo3::pyfunction]
#[pyo3(signature = (oracle))]
pub fn open_with_oracle_in_memory(py: Python<'_>, oracle: Py<PyAny>) -> PyResult<PyOracleEngine> {
    let bridge = Arc::new(PyOracleBridge::new(oracle));
    let engine = py
        .detach(|| mempill_sqlite::open_with_oracle_in_memory(bridge))
        .map_err(|e| StorageError::new_err(e.to_string()))?;
    Ok(PyOracleEngine { engine })
}

// ── Rust unit tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::Python;

    /// Verify the oracle runtime initialises without panicking.
    #[test]
    fn oracle_runtime_initialises_once() {
        let rt1 = oracle_runtime() as *const _;
        let rt2 = oracle_runtime() as *const _;
        assert_eq!(rt1, rt2, "OnceLock must return the same runtime pointer");
    }

    /// Verify open_with_oracle_in_memory returns a PyOracleEngine when given a Python oracle.
    #[test]
    fn open_with_oracle_in_memory_returns_engine() {
        Python::initialize();
        Python::attach(|py| {
            // Create a minimal Python oracle object via exec.
            // Uses a static UUID string to avoid importing the `uuid` module.
            let locals = pyo3::types::PyDict::new(py);
            py.run(
                pyo3::ffi::c_str!(
                    "class _TO:\n    def request_adjudication(self,a,r):\n        return '550e8400-e29b-41d4-a716-446655440000'\noracle_obj=_TO()"
                ),
                None,
                Some(&locals),
            )
            .expect("Python oracle class must compile");
            let oracle_obj: Py<PyAny> = locals.get_item("oracle_obj").unwrap().unwrap().into();
            let result = open_with_oracle_in_memory(py, oracle_obj);
            assert!(result.is_ok(), "open_with_oracle_in_memory must succeed with a valid oracle");
        });
    }

    /// Verify BridgeError messages are informative.
    #[test]
    fn bridge_error_display() {
        let e = BridgeError::InvalidHandle("not-a-uuid".into());
        assert!(e.to_string().contains("not-a-uuid"));

        let e2 = BridgeError::PythonException("TypeError: ...".into());
        assert!(e2.to_string().contains("TypeError"));
    }
}
