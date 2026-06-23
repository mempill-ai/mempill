"""
mempill — Python bindings for the mempill AI-agent memory engine.

Re-exports all public symbols from the compiled extension module (_mempill).
W3 will flesh out the ergonomics layer; for now this makes `import mempill` work.
"""

from mempill._mempill import (
    PyEngine,
    open_default,
    open_in_memory,
    MempillError,
    ValidationError,
    NotFoundError,
    ConflictError,
    StorageError,
    ConfigError,
    InternalError,
)

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
