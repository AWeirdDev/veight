use std::{
    ffi::c_void,
    mem,
    ptr::NonNull,
    sync::{Arc, Mutex, atomic},
};

use pyo3::prelude::*;
use tokio::sync::{mpsc, oneshot};
use tokio_util::task::TaskTracker;
use v8::{Global, Isolate};

use crate::{bridging::PyValue, stack::ArcStack};

#[derive(Debug)]
pub enum IsolateRunResult {
    /// The script returned a transmitable (between Python and Javascript) value.
    Value(PyResult<Py<PyAny>>),

    /// The script returned a resolvible promise.
    Promise(
        (
            Arc<(
                Arc<IsolateState>,
                Mutex<Option<oneshot::Sender<Py<PyValue>>>>,
            )>,
            oneshot::Receiver<Py<PyValue>>,
        ),
    ),

    /// The script threw an error.
    Error(PyErr),
}

#[derive(Debug)]
pub enum IsolateRequest {
    Hello {
        reply: oneshot::Sender<Arc<IsolateState>>,
    },

    /// Run Javascript code.
    Run {
        source: String,
        reply: oneshot::Sender<IsolateRunResult>,
    },

    /// Add a runnable Python function.
    AddFn { name: String },

    /// Close the isolate.
    Close,
}

pub type IsolateTx = mpsc::UnboundedSender<IsolateRequest>;
pub type IsolateRx = mpsc::UnboundedReceiver<IsolateRequest>;

#[derive(Debug)]
pub enum PythonRequest {
    /// Add a new Python function.
    AddFn { name: String, handler: Py<PyAny> },

    /// Run a Python function asynchronously.
    RunFn {
        name: String,
        args: Py<PyValue>,
        reply: oneshot::Sender<ObscuredGlobal<v8::Value>>,
    },

    /// Close Python-side handling.
    Close,
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
    pub closed: atomic::AtomicBool,
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
            closed: atomic::AtomicBool::new(false),
        }
    }

    #[inline(always)]
    pub fn is_closed(&self) -> bool {
        self.closed.load(atomic::Ordering::SeqCst)
    }

    #[inline(always)]
    pub async fn close(&self) {
        self.closed.store(true, atomic::Ordering::SeqCst);

        // first we close the tasks
        {
            self.tasks.wait().await;
            self.tasks.close();
        }

        // then we drop the stack
        {
            let mut stack = self.stack.lock().await;
            stack.drop_all();
        }

        // we don't need to care about ctx_scope at all
        // it gets dropped eventually from the task itself
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

    #[inline(always)]
    #[must_use]
    pub fn with<R>(&self, isolate: &mut Isolate, f: impl FnOnce(&T) -> R) -> R {
        let glob = unsafe { Global::from_raw(isolate, self.0) };
        let res = f(glob.open(isolate));

        // v8 literally does this
        mem::forget(glob);

        res
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
