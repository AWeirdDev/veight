use std::sync::Arc;

use pyo3::{exceptions, prelude::*};
use v8::{Global, Local, PinScope, Value};

use crate::{
    bridging::PyUndefined,
    isolates::{PyIsolate, PyIsolateCreateValue},
    transmit::{IsolateState, ObscuredGlobal},
};

// const PY_SLICE: OnceCell<Py<PyAny>> = OnceCell::new();

// #[inline(always)]
// fn init_py_slice(py: Python) -> PyResult<()> {
//     PY_SLICE
//         .set(py.import("builtins")?.get_item("slice")?.unbind())
//         .ok();

//     Ok(())
// }

// #[inline(always)]
// fn try_get_py_slice(py: Python) -> PyResult<Py<PyAny>> {
//     if let Some(slice) = PY_SLICE.get() {
//         Ok(slice.clone_ref(py))
//     } else {
//         init_py_slice(py)?;
//         Ok(PY_SLICE.get().unwrap().clone_ref(py))
//     }
// }

#[pyclass(name = "Value")]
pub struct PyValue {
    state: Arc<IsolateState>,
    pub data: Option<ObscuredGlobal<Value>>,
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

    pub fn try_get_data_with<T>(&self, run: impl FnOnce(&Value) -> PyResult<T>) -> PyResult<T> {
        let ob_glob = self
            .data
            .as_ref()
            .ok_or_else(|| exceptions::PyRuntimeError::new_err("data is already released"))?;

        let mut ctx_scope = self.state.ctx_scope.blocking_lock();
        let scope = std::pin::pin!(v8::HandleScope::new(ctx_scope.get_ctx_scope()));
        let scope = &mut scope.init();

        ob_glob.with(scope, |glob| run(glob))
    }
}

#[pymethods]
impl PyValue {
    #[staticmethod]
    fn create_string(isolate: Py<PyIsolate>, data: String) -> PyResult<Self> {
        let isolate = isolate.get();
        let value = isolate.create_value(PyIsolateCreateValue::String(data))?;
        Ok(Self::new(isolate.state.clone(), value))
    }

    #[staticmethod]
    fn create_number(isolate: Py<PyIsolate>, data: f64) -> PyResult<Self> {
        let isolate = isolate.get();
        let value = isolate.create_value(PyIsolateCreateValue::Number(data))?;
        Ok(Self::new(isolate.state.clone(), value))
    }

    #[staticmethod]
    fn create_undefined(isolate: Py<PyIsolate>) -> PyResult<Self> {
        let isolate = isolate.get();
        let value = isolate.create_value(PyIsolateCreateValue::Undefined)?;
        Ok(Self::new(isolate.state.clone(), value))
    }

    #[staticmethod]
    fn create_null(isolate: Py<PyIsolate>) -> PyResult<Self> {
        let isolate = isolate.get();
        let value = isolate.create_value(PyIsolateCreateValue::Null)?;
        Ok(Self::new(isolate.state.clone(), value))
    }

    fn is_int32(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_int32()))
    }

    fn to_int(&mut self) -> PyResult<i32> {
        self.try_take_data_with(|scope, value| {
            value
                .int32_value(scope)
                .ok_or_else(|| exceptions::PyTypeError::new_err("failed to convert value to int32"))
        })
    }

    fn is_uint32(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_uint32()))
    }

    fn to_uint(&mut self) -> PyResult<u32> {
        self.try_take_data_with(|scope, value| {
            value.uint32_value(scope).ok_or_else(|| {
                exceptions::PyTypeError::new_err("failed to convert value to uint32")
            })
        })
    }

    fn is_float64(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_number()))
    }

    fn to_float(&mut self) -> PyResult<f64> {
        self.try_take_data_with(|scope, value| {
            value.number_value(scope).ok_or_else(|| {
                exceptions::PyTypeError::new_err("failed to convert value to float (float64)")
            })
        })
    }

    fn is_bigint(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_big_int()))
    }

    fn to_bigint(&mut self) -> PyResult<i64> {
        self.try_take_data_with(|scope, value| {
            value.integer_value(scope).ok_or_else(|| {
                exceptions::PyTypeError::new_err("failed to convert value to integer (int64)")
            })
        })
    }

    fn is_boolean(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_boolean()))
    }

    fn to_bool(&mut self) -> PyResult<bool> {
        self.try_take_data_with(|scope, value| Ok(value.boolean_value(scope)))
    }

    fn is_string(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_string()))
    }

    fn to_str(&mut self) -> PyResult<String> {
        self.try_take_data_with(|scope, value| {
            value
                .is_string()
                .then(|| value.to_rust_string_lossy(scope))
                .ok_or_else(|| exceptions::PyTypeError::new_err("failed to convert to string"))
        })
    }

    fn is_undefined(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_undefined()))
    }

    fn to_undefined(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.try_take_data_with(|_, value| {
            value
                .is_null_or_undefined()
                .then_some(PyUndefined::py_new(py))
                .ok_or_else(|| {
                    exceptions::PyTypeError::new_err(
                        "expected undefined while converting value to undefined",
                    )
                })
        })
    }

    fn is_null(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_null()))
    }

    fn to_none(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.try_take_data_with(|_, value| {
            value.is_null_or_undefined().then_some(py.None()).ok_or(
                exceptions::PyTypeError::new_err(
                    "expected undefined or null while converting value to null",
                ),
            )
        })
    }

    fn is_null_or_undefined(&self) -> PyResult<bool> {
        self.try_get_data_with(|value| Ok(value.is_null_or_undefined()))
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
