use once_cell::sync::OnceCell;
use pyo3::{IntoPyObjectExt, prelude::*};

static PY_UNDEFINED: OnceCell<Py<PyAny>> = OnceCell::new();

#[pyclass(name = "undefined", frozen)]
pub struct PyUndefined;

#[pymethods]
impl PyUndefined {
    #[new]
    pub fn py_new(py: Python<'_>) -> Py<PyAny> {
        PY_UNDEFINED
            .get_or_init(move || PyUndefined.into_py_any(py).unwrap())
            .clone_ref(py)
    }

    const fn __bool__(&self) -> bool {
        false
    }

    const fn __repr__(&self) -> &'static str {
        "undefined"
    }
}
