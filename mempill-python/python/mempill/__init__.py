"""
mempill — Python SDK for the mempill AI-agent memory engine.

Ergonomic API (W3):
  open_for_agent(base_dir, agent_id)   → Engine  (file-backed SQLite, one file per agent)
  open_in_memory()                     → Engine  (ephemeral; tests / MCP sessions)

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
    open_default_for_agent as _open_default_for_agent,
    open_in_memory as _open_in_memory,
    open_with_oracle_for_agent as _open_with_oracle_for_agent,
    open_with_oracle_in_memory as _open_with_oracle_in_memory,
    MempillError,
    ValidationError,
    NotFoundError,
    ConflictError,
    StorageError,
    ConfigError,
    InternalError,
    date_granularity_of,
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
    end_fact,
    EndFactReceipt,
)

try:
    from importlib.metadata import PackageNotFoundError, version as _pkg_version

    __version__ = _pkg_version("mempill")
except PackageNotFoundError:  # pragma: no cover - editable/unbuilt checkout
    __version__ = "0.0.0+unknown"

# Re-export PyEngine under the friendlier name Engine so callers use `Engine` in
# type annotations while the compiled class is still named PyEngine internally.
Engine = PyEngine
# Re-export PyOracleEngine under the friendlier name OracleEngine.
OracleEngine = PyOracleEngine


def open_for_agent(base_dir: str, agent_id: str) -> Engine:
    """Open a file-backed, per-agent mempill engine under *base_dir*.

    The database file is derived automatically as ``base_dir/agent_{agent_id}.db`` —
    there is no way to point two different ``agent_id``s at the same file through this
    API. This is the only public entry point for file-backed storage; the prior
    shared-path ``open(path)`` function has been removed from the public API (breaking
    change, pre-1.0). See CHANGELOG for the manual migration path from a pre-0.4.0
    shared-file database.

    Raises:
        StorageError: if ``agent_id`` contains characters that could cause a filename
            collision (only ``[A-Za-z0-9_-]`` is accepted), if the database cannot be
            opened, or if migrations fail.
    """
    return _open_default_for_agent(base_dir, agent_id)


def open_in_memory() -> Engine:
    """Open an ephemeral in-memory mempill engine.

    Suitable for tests and short-lived MCP tool sessions. All data is lost
    when the engine object is garbage-collected.

    Raises:
        StorageError: if initialisation fails.
    """
    return _open_in_memory()


def open_oracle_for_agent(base_dir: str, agent_id: str, oracle: object) -> OracleEngine:
    """Open a file-backed, per-agent mempill engine wired to a Python oracle.

    The database file is derived automatically as ``base_dir/agent_{agent_id}.db``.

    The ``oracle`` argument must be any Python object with:

    .. code-block:: python

        def request_adjudication(self, agent_id: str, request: dict) -> str: ...

    Raises:
        StorageError: if ``agent_id`` contains characters that could cause a filename
            collision, if the database cannot be opened, or if migrations fail.
    """
    return _open_with_oracle_for_agent(base_dir, agent_id, oracle)


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
    # Package metadata
    "__version__",
    # No-oracle constructors
    "open_for_agent",
    "open_in_memory",
    # Oracle constructors
    "open_oracle_for_agent",
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
    "date_granularity_of",
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
    # assert_validity / end_fact API (TASK-33 E2)
    "end_fact",
    "EndFactReceipt",
]
