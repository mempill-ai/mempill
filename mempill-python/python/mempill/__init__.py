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
    open_default as _open_default,
    open_in_memory as _open_in_memory,
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
    IngestClaimRequest,
    IngestClaimResponse,
    QueryMemoryRequest,
    QueryMemoryResponse,
    ReconcileRequest,
    ReconcileResponse,
    AuditQueryRequest,
    AuditQueryResponse,
)

# Re-export PyEngine under the friendlier name Engine so callers use `Engine` in
# type annotations while the compiled class is still named PyEngine internally.
Engine = PyEngine


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


__all__ = [
    # Constructors
    "open",
    "open_in_memory",
    # Engine handle
    "Engine",
    "PyEngine",
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
    "IngestClaimRequest",
    "IngestClaimResponse",
    "QueryMemoryRequest",
    "QueryMemoryResponse",
    "ReconcileRequest",
    "ReconcileResponse",
    "AuditQueryRequest",
    "AuditQueryResponse",
]
