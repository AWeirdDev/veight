use std::sync::Arc;

use once_cell::sync::OnceCell;
use pyo3::{IntoPyObjectExt, exceptions, prelude::*};
use v8::{Array, Global, Local, PinScope, Value};

use crate::{
    bridging::PyUndefined,
    transmit::{IsolateState, ObscuredGlobal},
};

const PY_SLICE: OnceCell<Py<PyAny>> = OnceCell::new();

#[inline(always)]
fn init_py_slice(py: Python) -> PyResult<()> {
    PY_SLICE
        .set(py.import("builtins")?.get_item("slice")?.unbind())
        .ok();

    Ok(())
}

#[inline(always)]
fn try_get_py_slice(py: Python) -> PyResult<Py<PyAny>> {
    if let Some(slice) = PY_SLICE.get() {
        Ok(slice.clone_ref(py))
    } else {
        init_py_slice(py)?;
        Ok(PY_SLICE.get().unwrap().clone_ref(py))
    }
}

#[pyclass(name = "Value")]
pub struct PyValue {
    state: Arc<IsolateState>,
    data: Option<ObscuredGlobal<Value>>,
}

impl PyValue {
    #[inline(always)]
    #[must_use]
    pub fn new(state: Arc<IsolateState>, value: Global<Value>) -> Self {
        Self {
            state,
            data: Some(ObscuredGlobal::new(value)),
        }
    }

    #[inline(always)]
    pub fn release(&mut self) {
        if let Some(ob_global) = self.data.take() {
            let mut scope = self.state.ctx_scope.blocking_lock();
            let ctx_scope = scope.get_ctx_scope();
            let _ = ob_global.take(ctx_scope);
        }
    }

    pub fn try_take_data_with<T>(
        &mut self,
        run: impl FnOnce(&PinScope<'_, '_>, Local<'_, Value>) -> PyResult<T>,
    ) -> PyResult<T> {
        let ob_glob = self.data.take().ok_or(exceptions::PyRuntimeError::new_err(
            "data is already released",
        ))?;

        let mut ctx_scope = self.state.ctx_scope.blocking_lock();
        let scope = std::pin::pin!(v8::HandleScope::new(ctx_scope.get_ctx_scope()));
        let scope = &mut scope.init();

        let global = ob_glob.take(scope);
        let local_value = Local::new(scope, global);

        run(scope, local_value)
    }
}

#[pymethods]
impl PyValue {
    fn to_int(&mut self) -> PyResult<i32> {
        self.try_take_data_with(|scope, value| {
            value
                .int32_value(scope)
                .ok_or_else(|| exceptions::PyTypeError::new_err("failed to convert value to int32"))
        })
    }

    fn to_uint(&mut self) -> PyResult<u32> {
        self.try_take_data_with(|scope, value| {
            value.uint32_value(scope).ok_or_else(|| {
                exceptions::PyTypeError::new_err("failed to convert value to uint32")
            })
        })
    }

    fn to_float(&mut self) -> PyResult<f64> {
        self.try_take_data_with(|scope, value| {
            value.number_value(scope).ok_or_else(|| {
                exceptions::PyTypeError::new_err("failed to convert value to float (float64)")
            })
        })
    }

    fn to_bigint(&mut self) -> PyResult<i64> {
        self.try_take_data_with(|scope, value| {
            value.integer_value(scope).ok_or_else(|| {
                exceptions::PyTypeError::new_err("failed to convert value to integer (int64)")
            })
        })
    }

    fn to_bool(&mut self) -> PyResult<bool> {
        self.try_take_data_with(|scope, value| Ok(value.boolean_value(scope)))
    }

    fn to_str(&mut self) -> PyResult<String> {
        self.try_take_data_with(|scope, value| {
            value
                .to_string(scope)
                .ok_or_else(|| exceptions::PyTypeError::new_err("failed to convert to string"))
                .map(|d| d.to_rust_string_lossy(scope))
        })
    }

    fn to_undefined(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.try_take_data_with(|_, value| {
            value
                .is_undefined()
                .then_some(PyUndefined::py_new(py))
                .ok_or_else(|| {
                    exceptions::PyTypeError::new_err(
                        "expected undefined while converting value to undefined",
                    )
                })
        })
    }

    fn to_null(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.try_take_data_with(|_, value| {
            value.is_null_or_undefined().then_some(py.None()).ok_or(
                exceptions::PyTypeError::new_err(
                    "expected undefined or null while converting value to null",
                ),
            )
        })
    }

    fn __getitem__(&mut self, py: Python, index: Py<PyAny>) -> PyResult<Py<PyAny>> {
        let index = index.bind(py);
        let slice = try_get_py_slice(py)?;
        let slice = slice.bind(py);

        let get = if index.is_instance(slice)? {
            return Err(exceptions::PyRuntimeError::new_err("nawh"));
        } else {
            index.extract::<usize>()?
        };

        let state_clone = self.state.clone();
        self.try_take_data_with(|scope, value| match value {
            _ if value.is_string() => {
                let utf16 = String::from_utf16_lossy(
                    &value
                        .to_rust_string_lossy(scope)
                        .encode_utf16()
                        .collect::<Vec<u16>>(),
                );
                let mut chars = utf16.chars();
                chars
                    .nth(get)
                    .ok_or_else(|| {
                        exceptions::PyIndexError::new_err(format!(
                            "index out of range for value type string: {}",
                            get
                        ))
                    })
                    .map(|chr| chr.into_py_any(py).unwrap())
            }

            _ if value.is_array() => Ok(PyValue::new(
                state_clone,
                Global::new(
                    scope,
                    value
                        .cast::<Array>()
                        .get(scope, v8::Number::new(scope, get as f64).cast())
                        .unwrap(),
                ),
            )
            .into_py_any(py)
            .unwrap()),

            _ => Err(exceptions::PyTypeError::new_err("found unindexable type")),
        })
    }

    #[pyo3(name = "release")]
    #[inline(always)]
    fn py_release(&mut self) {
        self.release();
    }

    fn __repr__(&self) -> String {
        format!(
            "Value({})",
            self.data
                .as_ref()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "None".to_string())
        )
    }
}

impl Drop for PyValue {
    fn drop(&mut self) {
        self.release();
    }
}
