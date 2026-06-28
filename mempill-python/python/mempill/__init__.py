"""
mempill — Python SDK for the mempill AI-agent memory engine.

Ergonomic API (W3):
  open(path)         → Engine  (file-backed SQLite)
  open_in_memory()   → Engine  (ephemeral; tests / MCP sessions)

Types:
  Disposition        — 12-state str-Enum; comparable to engine response strings
  ProvenanceLabel    — factory helpers returning wire-shape dicts
  IngestClaimRequest, IngestClaimResponse, QueryMemoryRequest, QueryMemoryResponse,
  ReconcileRequest,  ReconcileResponse,  AuditQueryRequest,  AuditQueryResponse
  (TypedDicts for IDE / mypy; engine accepts plain dicts)

Exceptions:
  MempillError (base)
    ValidationError, NotFoundError, ConflictError,
    StorageError, ConfigError, InternalError
"""

from __future__ import annotations

from mempill._mempill import (
    PyEngine,
    PyOracleEngine,
    open_default as _open_default,
    open_in_memory as _open_in_memory,
    open_with_oracle as _open_with_oracle,
    open_with_oracle_in_memory as _open_with_oracle_in_memory,
    MempillError,
    ValidationError,
    NotFoundError,
    ConflictError,
    StorageError,
    ConfigError,
    InternalError,
)

from mempill.types import (
    Disposition,
    ProvenanceLabel,
    ConfidenceDict,
    ValidTimeDict,
    FactDict,
    BeliefSlot,
    BeliefProjection,
    IngestClaimRequest,
    IngestClaimResponse,
    QueryMemoryRequest,
    QueryMemoryResponse,
    ReconcileRequest,
    ReconcileResponse,
    AuditQueryRequest,
    AuditQueryResponse,
)

from mempill.ergonomic import (
    remember,
    recall,
    RememberOptions,
    RememberReceipt,
    BeliefDetail,
    ContestCandidate,
    RecallResult,
    UnparsableDateError,
    history,
    History,
    HistoryEntry,
)

# Re-export PyEngine under the friendlier name Engine so callers use `Engine` in
# type annotations while the compiled class is still named PyEngine internally.
Engine = PyEngine
# Re-export PyOracleEngine under the friendlier name OracleEngine.
OracleEngine = PyOracleEngine


def open(path: str) -> Engine:  # noqa: A001  (shadows builtin intentionally)
    """Open a file-backed mempill engine at *path*.

    Raises:
        StorageError: if the database cannot be opened or migrations fail.
    """
    return _open_default(path)


def open_in_memory() -> Engine:
    """Open an ephemeral in-memory mempill engine.

    Suitable for tests and short-lived MCP tool sessions. All data is lost
    when the engine object is garbage-collected.

    Raises:
        StorageError: if initialisation fails.
    """
    return _open_in_memory()


def open_oracle(path: str, oracle: object) -> OracleEngine:
    """Open a file-backed mempill engine wired to a Python oracle.

    The ``oracle`` argument must be any Python object with:

    .. code-block:: python

        def request_adjudication(self, agent_id: str, request: dict) -> str: ...

    Raises:
        StorageError: if the database cannot be opened or migrations fail.
    """
    return _open_with_oracle(path, oracle)


def open_oracle_in_memory(oracle: object) -> OracleEngine:
    """Open an ephemeral in-memory mempill engine wired to a Python oracle.

    The ``oracle`` argument must be any Python object with:

    .. code-block:: python

        def request_adjudication(self, agent_id: str, request: dict) -> str: ...

    Raises:
        StorageError: if initialisation fails.
    """
    return _open_with_oracle_in_memory(oracle)


__all__ = [
    # No-oracle constructors
    "open",
    "open_in_memory",
    # Oracle constructors
    "open_oracle",
    "open_oracle_in_memory",
    # Engine handles
    "Engine",
    "PyEngine",
    "OracleEngine",
    "PyOracleEngine",
    # Exceptions
    "MempillError",
    "ValidationError",
    "NotFoundError",
    "ConflictError",
    "StorageError",
    "ConfigError",
    "InternalError",
    # Enums / helpers
    "Disposition",
    "ProvenanceLabel",
    # TypedDicts
    "ConfidenceDict",
    "ValidTimeDict",
    "FactDict",
    "BeliefSlot",
    "BeliefProjection",
    "IngestClaimRequest",
    "IngestClaimResponse",
    "QueryMemoryRequest",
    "QueryMemoryResponse",
    "ReconcileRequest",
    "ReconcileResponse",
    "AuditQueryRequest",
    "AuditQueryResponse",
    # Tier-1 ergonomic API
    "remember",
    "recall",
    "RememberOptions",
    "RememberReceipt",
    "BeliefDetail",
    "ContestCandidate",
    "RecallResult",
    "UnparsableDateError",
    # History API
    "history",
    "History",
    "HistoryEntry",
]
