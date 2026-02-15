use pyo3::{create_exception, exceptions, prelude::*};
use v8::PinScope;

create_exception!(
    veight,
    JsError,
    exceptions::PyException,
    "Javascript error."
);

impl JsError {
    pub fn from_exception(scope: &PinScope, exc: &v8::Local<v8::Value>) -> PyErr {
        let exception = exc.cast::<v8::Object>();

        let name = exception
            .get(
                scope,
                v8::String::new(scope, "name").unwrap().cast::<v8::Value>(),
            )
            .map(|s| s.to_rust_string_lossy(scope))
            .unwrap_or_else(|| "Error".to_string());
        let message = exception
            .get(
                scope,
                v8::String::new(scope, "message")
                    .unwrap()
                    .cast::<v8::Value>(),
            )
            .map(|s| s.to_rust_string_lossy(scope))
            .unwrap_or_else(|| "unknown message".to_string());
        let stack = {
            let value = exception
                .get(
                    scope,
                    v8::String::new(scope, "stack").unwrap().cast::<v8::Value>(),
                )
                .unwrap_or_else(|| v8::undefined(scope).cast::<v8::Value>());

            if value.is_null_or_undefined() {
                None
            } else {
                Some(value.to_rust_string_lossy(scope))
            }
        };

        Self::new_err(format!(
            "({}) {}\nStack trace:\n{}",
            name,
            message,
            stack.unwrap_or_else(|| String::new())
        ))
    }
}

pub trait IntoPyException {
    fn into_py_exception(&self, scope: &PinScope) -> PyErr;
}

impl IntoPyException for v8::Local<'_, v8::Value> {
    fn into_py_exception(&self, scope: &PinScope) -> PyErr {
        JsError::from_exception(scope, self)
    }
}
