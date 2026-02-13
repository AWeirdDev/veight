use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

use pyo3::prelude::*;
use tokio::sync::{mpsc, oneshot};
use tokio_util::task::TaskTracker;
use v8::{Global, Isolate};

use crate::{bridging::PyValue, stack::ArcStack};

#[derive(Debug)]
pub enum IsolateRunResult {
    Value(PyResult<Py<PyAny>>),
    Promise(
        (
            Arc<(
                Arc<IsolateState>,
                Mutex<Option<oneshot::Sender<Py<PyValue>>>>,
            )>,
            oneshot::Receiver<Py<PyValue>>,
        ),
    ),
    Error(String),
}

#[derive(Debug)]
pub enum IsolateRequest {
    /// Run Javascript code.
    Run {
        source: String,
        reply: oneshot::Sender<IsolateRunResult>,
    },

    /// Add a runnable Python function.
    AddFn { name: String },
}

pub type IsolateTx = mpsc::UnboundedSender<IsolateRequest>;
pub type IsolateRx = mpsc::UnboundedReceiver<IsolateRequest>;

#[derive(Debug)]
pub enum PythonRequest {
    AddFn {
        name: String,
        handler: Py<PyAny>,
    },
    RunFn {
        name: String,
        args: Py<PyValue>,
        reply: oneshot::Sender<String>,
    },
}

#[repr(transparent)]
#[derive(Debug)]
pub struct ObscuredContextScope(NonNull<c_void>);

impl ObscuredContextScope {
    pub fn new(ctx_scope: &mut v8::ContextScope<'_, '_, v8::HandleScope<'_>>) -> Self {
        let t = Self(unsafe {
            NonNull::new_unchecked(
                ctx_scope as *mut v8::ContextScope<'_, '_, v8::HandleScope<'_>> as _,
            )
        });

        t
    }

    #[inline(always)]
    pub unsafe fn get_ctx_scope_unchecked(
        &self,
    ) -> &mut v8::ContextScope<'static, 'static, v8::HandleScope<'static>> {
        unsafe {
            &mut *(self.0.as_ptr()
                as *mut v8::ContextScope<'static, 'static, v8::HandleScope<'static>>)
        }
    }

    #[inline(always)]
    pub fn get_ctx_scope(
        &mut self, // this ensures there's only ONE mut holder
    ) -> &mut v8::ContextScope<'static, 'static, v8::HandleScope<'static>> {
        unsafe { self.get_ctx_scope_unchecked() }
    }
}

unsafe impl Send for ObscuredContextScope {}
unsafe impl Sync for ObscuredContextScope {}

impl Drop for ObscuredContextScope {
    fn drop(&mut self) {
        let _ = self.get_ctx_scope();
    }
}

#[derive(Debug)]
pub struct IsolateState {
    pub tasks: TaskTracker,
    pub py_tx: PythonTx,
    pub stack: tokio::sync::Mutex<ArcStack>,
    pub ctx_scope: tokio::sync::Mutex<ObscuredContextScope>,
}

impl IsolateState {
    #[inline(always)]
    pub fn new(
        py_tx: PythonTx,
        ctx_scope: &mut v8::ContextScope<'_, '_, v8::HandleScope<'_>>,
    ) -> Self {
        Self {
            tasks: TaskTracker::new(),
            py_tx,
            stack: tokio::sync::Mutex::new(ArcStack::new()),
            ctx_scope: tokio::sync::Mutex::new(ObscuredContextScope::new(ctx_scope)),
        }
    }
}

pub type PythonTx = mpsc::UnboundedSender<PythonRequest>;
pub type PythonRx = mpsc::UnboundedReceiver<PythonRequest>;

/// [`Global`] with [`Send`] and [`Sync`].
///
/// # Dropping
/// Dropping is **NOT** implemented.
/// You must drop it yourself using [`ObscuredGlobal::take`].
#[repr(transparent)]
#[derive(Debug)]
pub struct ObscuredGlobal<T>(NonNull<T>);

impl<T> ObscuredGlobal<T> {
    #[inline(always)]
    pub fn new(gl: Global<T>) -> Self {
        Self(gl.into_raw())
    }

    #[inline(always)]
    #[must_use]
    pub fn take(self, isolate: &mut Isolate) -> Global<T> {
        unsafe { Global::from_raw(isolate, self.0) }
    }
}

unsafe impl<T> Send for ObscuredGlobal<T> {}
unsafe impl<T> Sync for ObscuredGlobal<T> {}

impl<T> ToString for ObscuredGlobal<T> {
    #[inline(always)]
    fn to_string(&self) -> String {
        format!("{:p}", self.0)
    }
}
