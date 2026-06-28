"""
Type stubs for the compiled mempill._mempill PyO3 extension module.

Hand-written (maturin generate-stubs requires the `experimental-inspect` PyO3
feature which is not enabled in this build; pyo3-stub-gen 0.4.x is incompatible
with PyO3 0.29 per RESEARCH_VERSIONS.md R2).
"""

from __future__ import annotations

from typing import Any, final

# ── Exception hierarchy ───────────────────────────────────────────────────────

class MempillError(Exception):
    """Base exception for all mempill errors."""
    ...

class ValidationError(MempillError):
    """Raised when a write is rejected due to structural/domain invariant violations."""
    ...

class NotFoundError(MempillError):
    """Raised when a requested entity (claim, agent, adjudication handle) does not exist."""
    ...

class ConflictError(MempillError):
    """Raised when a write-lock contention conflict is detected."""
    ...

class StorageError(MempillError):
    """Raised when the persistence layer encounters an error."""
    ...

class ConfigError(MempillError):
    """Raised when a calibration or configuration parameter is invalid."""
    ...

class InternalError(MempillError):
    """Raised when an internal engine invariant is violated (indicates a bug)."""
    ...

# ── PyEngine ──────────────────────────────────────────────────────────────────

@final
class PyEngine:
    """Sync Python handle to a mempill DefaultEngine (SQLite, no oracle, no vector).

    Obtain via ``open_default(path)`` or ``open_in_memory()``. Thread-safe (Arc-backed).
    All methods accept and return plain Python dicts (via pythonize/depythonize).
    """

    def ingest_claim(self, request: dict[str, Any]) -> dict[str, Any]:
        """Ingest a claim into memory.

        Args:
            request: dict matching IngestClaimRequest schema:
                - agent_id (str)
                - subject (str)
                - predicate (str)
                - value (Any JSON-serialisable)
                - provenance (dict with "type" and optional "kind" keys)
                - cardinality (str: "Functional" | "SetValued" | "Unknown")
                - valid_time (dict | None)
                - confidence (dict: {"value_confidence": float, "valid_time_confidence": float})
                - criticality (str: "Low" | "Medium" | "High" | "Critical")
                - derived_from (list[str] of UUID strings)

        Returns:
            dict with:
                - claim_ref (str): UUID string
                - disposition (str): Disposition variant name
                - contested_with (list[str]): list of UUID strings

        Raises:
            ValidationError: bad request shape or domain invariant violation
            StorageError: persistence layer failure
            ConflictError: write-lock contention
            InternalError: engine invariant violated
        """
        ...

    def query_memory(self, request: dict[str, Any]) -> dict[str, Any]:
        """Query the current belief for a (subject, predicate) pair.

        Bi-temporal query — two independent time axes (D2 independence rule):

        * ``valid_at``: selects the belief *true in the world* at this real-world
          instant (the valid-time axis).  Example: "What was the CEO on 2023-06-15?"

        * ``as_of_tx_time``: selects what the *system knew* at this point in its own
          log (the transaction-time axis).  Example: "What did the engine know last
          Tuesday, before the correction was ingested?"

        The two compose independently: transaction-time filter is applied first, then
        valid-time selection narrows the result.  When neither is set, both axes
        default to ``now`` (backward-compatible current live belief).

        Args:
            request: dict with:
                - agent_id (str)
                - subject (str)
                - predicate (str)
                - as_of_tx_time (str | None): optional ISO-8601 UTC string —
                  transaction-time axis; controls which writes are visible.
                - valid_at (str | None): optional ISO-8601 UTC string —
                  valid-time axis; selects the claim whose valid-time window
                  contains this instant.  When absent, as_of_tx_time (or now)
                  is used as the valid-time instant (backward-compatible).

        Returns:
            dict with:
                - belief (dict): BeliefProjection structure.
                  Access: result["belief"]["primary"]["fact"]["value"]

            Each belief slot (``belief["primary"]``, ``belief["alternatives"][i]``)
            also carries per-endpoint precision metadata (W6):

                - ``valid_from_display`` (str | absent): start of the valid-time window
                  rendered at its recorded precision.  Examples:
                  ``"2020"`` (Year), ``"2020-03"`` (Month), ``"2020-03-15"`` (Day/Instant).
                  Absent when the start endpoint is unknown.

                - ``valid_until_display`` (str | absent): same for the end endpoint.
                  Absent when open-ended.

                - ``valid_time["start_granularity"]`` (str | absent): raw granularity tag
                  (``"year"``, ``"month"``, ``"day"``, ``"instant"``).  Absent for legacy
                  rows or when the start was not set.

                - ``valid_time["end_granularity"]`` (str | absent): same for the end.

        Raises:
            ValidationError: bad request
            NotFoundError: agent or claim not found
            StorageError: persistence layer failure
        """
        ...

    def reconcile(self, request: dict[str, Any]) -> dict[str, Any]:
        """Reconcile one or more subject lines for an agent.

        Args:
            request: dict with:
                - agent_id (str)
                - subject_lines (list[tuple[str, str]]): (subject, predicate) pairs;
                  empty list reconciles all subject lines for the agent

        Returns:
            dict with:
                - outcomes (list): list of (claim_ref_uuid, disposition) pairs
                - oracle_escalations (int): number of subject lines needing oracle

        Raises:
            ValidationError: bad request
            StorageError: persistence layer failure
        """
        ...

    def query_audit(self, request: dict[str, Any]) -> dict[str, Any]:
        """Query the audit ledger for an agent.

        Args:
            request: dict with:
                - agent_id (str)
                - claim_ref (str | None): optional UUID string filter
                - from_tx_time (str | None): optional ISO-8601 UTC lower bound
                - limit (int): maximum number of entries to return

        Returns:
            dict with:
                - entries (list[dict]): list of LedgerEntry dicts

        Raises:
            ValidationError: bad request
            StorageError: persistence layer failure
        """
        ...

# ── Module-level constructors ─────────────────────────────────────────────────

def open_default(path: str) -> PyEngine:
    """Open a file-backed mempill engine at ``path``.

    Raises:
        StorageError: if the database cannot be opened or migrations fail.
    """
    ...

def open_in_memory() -> PyEngine:
    """Open an in-memory mempill engine (ephemeral; useful for tests and MCP sessions).

    Raises:
        StorageError: if initialisation fails.
    """
    ...

__all__ = [
    "PyEngine",
    "open_default",
    "open_in_memory",
    "MempillError",
    "ValidationError",
    "NotFoundError",
    "ConflictError",
    "StorageError",
    "ConfigError",
    "InternalError",
]
