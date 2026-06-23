//! mempill-python — PyO3/maturin binding crate (v0.2).
//!
//! Exposes the mempill engine to Python as the `mempill._mempill` extension module.
//! The Python ergonomics layer (mempill/__init__.py stubs, .pyi type hints) is Wave 3.
//!
//! Module contents:
//!   - `PyEngine`        — sync wrapper around `DefaultEngine`
//!   - `open_default`    — open a file-backed engine
//!   - `open_in_memory`  — open an in-memory engine
//!   - Exception types:  `MempillError`, `ValidationError`, `NotFoundError`,
//!                       `ConflictError`, `StorageError`, `ConfigError`, `InternalError`

mod engine;
mod errors;

use pyo3::prelude::*;

use engine::{open_default, open_in_memory, PyEngine};
use errors::register_exceptions;

/// The `mempill._mempill` extension module.
#[pymodule]
fn _mempill(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register exception hierarchy first (base class must precede subclasses).
    register_exceptions(m)?;

    // Register constructors.
    m.add_function(wrap_pyfunction!(open_default, m)?)?;
    m.add_function(wrap_pyfunction!(open_in_memory, m)?)?;

    // Register PyEngine class.
    m.add_class::<PyEngine>()?;

    Ok(())
}
