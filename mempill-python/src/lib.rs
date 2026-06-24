//! mempill-python — PyO3/maturin binding crate (v0.2).
//!
//! Exposes the mempill engine to Python as the `mempill._mempill` extension module.
//! The Python ergonomics layer (mempill/__init__.py stubs, .pyi type hints) is Wave 3.
//!
//! Module contents:
//!   - `PyEngine`           — sync wrapper around `DefaultEngine` (no oracle)
//!   - `PyOracleEngine`     — sync wrapper around `OracleEngine<PyOracleBridge>` (Python oracle)
//!   - `open_default`       — open a file-backed engine (no oracle)
//!   - `open_in_memory`     — open an in-memory engine (no oracle)
//!   - `open_with_oracle`   — open a file-backed engine wired to a Python oracle
//!   - `open_with_oracle_in_memory` — open an in-memory engine wired to a Python oracle
//!   - Exception types:  `MempillError`, `ValidationError`, `NotFoundError`,
//!                       `ConflictError`, `StorageError`, `ConfigError`, `InternalError`

mod engine;
mod errors;
mod oracle;

use pyo3::prelude::*;

use engine::{open_default, open_in_memory, PyEngine};
use errors::register_exceptions;
use oracle::{open_with_oracle, open_with_oracle_in_memory, PyOracleEngine};

/// The `mempill._mempill` extension module.
#[pymodule]
fn _mempill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register exception hierarchy first (base class must precede subclasses).
    register_exceptions(m)?;

    // Register no-oracle constructors.
    m.add_function(wrap_pyfunction!(open_default, m)?)?;
    m.add_function(wrap_pyfunction!(open_in_memory, m)?)?;

    // Register Python-oracle constructors.
    m.add_function(wrap_pyfunction!(open_with_oracle, m)?)?;
    m.add_function(wrap_pyfunction!(open_with_oracle_in_memory, m)?)?;

    // Register engine classes.
    m.add_class::<PyEngine>()?;
    m.add_class::<PyOracleEngine>()?;

    Ok(())
}
