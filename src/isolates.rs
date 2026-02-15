use std::{
    ffi::c_void,
    hint::unreachable_unchecked,
    mem::ManuallyDrop,
    sync::{
        self, Arc, Mutex,
        atomic::{self, AtomicBool},
    },
};

use dashmap::DashMap;
use once_cell::sync::OnceCell;
use pyo3::{IntoPyObjectExt, exceptions, prelude::*};
use tokio::sync::{mpsc, oneshot};
use v8::{
    CreateParams, External, Function, FunctionCallbackArguments, Global, HandleScope, Isolate,
    PinnedRef, Promise, PromiseResolver, ReturnValue, Script, TryCatch,
};

use crate::{
    bridging::{IntoPyException, PyValue},
    stack::ArcStackItem,
    transmit::*,
};

static PY_ISOLATES: OnceCell<Py<PyAny>> = OnceCell::new();

/// A singleton for V8 Isolates bindings.
#[pyclass(name = "Isolates", frozen)]
pub struct PyIsolates;

#[pymethods]
impl PyIsolates {
    #[new]
    fn py_new(py: Python<'_>) -> &Py<PyAny> {
        PY_ISOLATES.get_or_init(move || {
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
            PyIsolates.into_py_any(py).unwrap()
        })
    }

    #[pyo3(name = "create_isolate", signature = (*, no_python = false))]
    fn py_create_isolate<'py>(
        &self,
        py: Python<'py>,
        no_python: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (isolate_tx, isolate_rx) = mpsc::unbounded_channel::<IsolateRequest>();
            let (python_tx, python_rx) = mpsc::unbounded_channel::<PythonRequest>();

            // js background task
            {
                let py_tx_clone = python_tx.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();

                    let local = tokio::task::LocalSet::new();
                    rt.block_on(local.run_until(isolate_task(isolate_rx, py_tx_clone)));
                });
            }

            // after we spawned those shits, we need to do a handshake with js isolate
            let state = {
                let (tx, rx) = oneshot::channel::<Arc<IsolateState>>();
                isolate_tx
                    .send(IsolateRequest::Hello { reply: tx })
                    .map_err(|err| {
                        exceptions::PyRuntimeError::new_err(format!(
                            "failed to send initial handshake, reason: {}",
                            err.to_string()
                        ))
                    })?;

                rx.await.map_err(|err| {
                    exceptions::PyRuntimeError::new_err(format!(
                        "failed to read initial handshake response, reason: {}",
                        err.to_string()
                    ))
                })?
            };

            let py_isolate = Python::attach(|py| {
                Py::new(
                    py,
                    PyIsolate {
                        isolate_tx,
                        python_tx,
                        state,
                    },
                )
                .unwrap()
            });

            // python task
            if !no_python {
                tokio::task::spawn(pyo3_async_runtimes::tokio::scope(
                    Python::attach(|py| {
                        pyo3_async_runtimes::TaskLocals::new(
                            pyo3_async_runtimes::tokio::get_current_locals(py)
                                .unwrap()
                                .event_loop(py),
                        )
                    }),
                    python_task(python_rx, Python::attach(|py| py_isolate.clone_ref(py))),
                ));
            }

            Ok(py_isolate)
        })
    }

    #[inline(always)]
    fn isolate_session(slf: Py<Self>) -> PyIsolateSession {
        PyIsolateSession::new(slf)
    }
}

#[pyclass(name = "Isolate", frozen)]
pub struct PyIsolate {
    pub isolate_tx: IsolateTx,
    pub python_tx: PythonTx,
    pub state: Arc<IsolateState>,
}

pub enum PyIsolateCreateValue {
    String(String),
    Number(f64),
    Undefined,
    Null,
}

impl PyIsolate {
    pub fn create_value(&self, cv: PyIsolateCreateValue) -> PyResult<Global<v8::Value>> {
        let mut ctx_scope = self.state.ctx_scope.blocking_lock();
        let scope = std::pin::pin!(v8::HandleScope::new(ctx_scope.get_ctx_scope()));
        let scope = &mut scope.init();

        let value = match cv {
            PyIsolateCreateValue::String(s) => v8::String::new(scope, &s)
                .ok_or_else(|| exceptions::PyRuntimeError::new_err("failed to create string"))?
                .cast::<v8::Value>(),
            PyIsolateCreateValue::Number(n) => v8::Number::new(scope, n).cast::<v8::Value>(),
            PyIsolateCreateValue::Undefined => v8::undefined(scope).cast::<v8::Value>(),
            PyIsolateCreateValue::Null => v8::null(scope).cast::<v8::Value>(),
        };

        Ok(Global::new(scope, value))
    }

    pub fn check_closed(&self) -> PyResult<()> {
        if self.state.is_closed() {
            Err(exceptions::PyRuntimeError::new_err("isolate is closed"))
        } else {
            Ok(())
        }
    }
}

#[pymethods]
impl PyIsolate {
    #[pyo3(name = "add_function")]
    fn py_add_fn(&self, name: String, func: Py<PyAny>) -> PyResult<()> {
        self.check_closed()?;

        self.isolate_tx
            .send(IsolateRequest::AddFn { name: name.clone() })
            .map_err(|err| {
                exceptions::PyRuntimeError::new_err(format!(
                    "failed to send isolate add function request, reason: {}",
                    err.to_string()
                ))
            })?;

        self.python_tx
            .send(PythonRequest::AddFn {
                name,
                handler: func,
            })
            .map_err(|err| {
                exceptions::PyRuntimeError::new_err(format!(
                    "failed to send python add function handler request, reason: {}\n{}{}",
                    err.to_string(),
                    "it is also possible that python-side handling is disabled with `no_python=True` ",
                    "during the creation of this isolate"
                ))
            })
    }

    #[pyo3(name = "run")]
    fn py_run<'py>(&self, py: Python<'py>, source: String) -> PyResult<Bound<'py, PyAny>> {
        self.check_closed()?;

        let tx = self.isolate_tx.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let (reply, receive) = oneshot::channel::<IsolateRunResult>();

            tx.send(IsolateRequest::Run { source, reply })
                .map_err(|err| {
                    exceptions::PyRuntimeError::new_err(format!(
                        "failed to send isolate run request, reason: {}",
                        err.to_string()
                    ))
                })?;

            let result = receive.await.map_err(|err| {
                exceptions::PyRuntimeError::new_err(format!(
                    "failed to receive isolate run response, reason: {}",
                    err.to_string()
                ))
            })?;
            match result {
                IsolateRunResult::Promise((data, receiver)) => Python::attach(|py| {
                    pyo3_async_runtimes::tokio::future_into_py(py, async move {
                        let manual = ManuallyDrop::new(data);
                        let result = receiver.await.map_err(|err| {
                            exceptions::PyRuntimeError::new_err(format!(
                                "failed to receive future result from js isolate, reason: {}",
                                err.to_string()
                            ))
                        });
                        let _ = ManuallyDrop::into_inner(manual);

                        result
                    })
                    .map(|res| res.into_py_any(py).unwrap())
                }),
                IsolateRunResult::Value(value) => value,
                IsolateRunResult::Error(err) => Err(err),
            }
        })
    }

    #[inline(always)]
    fn __del__(&self) {
        self.close();
    }

    #[inline(always)]
    fn close(&self) {
        self.python_tx.send(PythonRequest::Close).ok();
        self.isolate_tx.send(IsolateRequest::Close).ok();
    }
}

#[pyclass(name = "IsolateSession", frozen)]
pub struct PyIsolateSession {
    isolates: Py<PyIsolates>,
    isolate: tokio::sync::Mutex<Option<Py<PyIsolate>>>,
    dead: AtomicBool,
}

impl PyIsolateSession {
    #[inline(always)]
    fn new(isolates: Py<PyIsolates>) -> Self {
        Self {
            isolates,
            isolate: tokio::sync::Mutex::new(None),
            dead: AtomicBool::new(false),
        }
    }
}

#[pymethods]
impl PyIsolateSession {
    fn __aenter__<'p>(slf: Py<Self>, py: Python<'p>) -> PyResult<Bound<'p, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let slf = slf.get();
            let mut guard = slf.isolate.lock().await;

            if let Some(me) = guard.as_ref() {
                return Ok(Python::attach(|py| me.clone_ref(py)));
            }

            if !slf.dead.load(atomic::Ordering::SeqCst) {
                let isolates = slf.isolates.get();

                let isolate = Python::attach(move |py| {
                    pyo3_async_runtimes::tokio::into_future(isolates.py_create_isolate(py, false)?)
                })?
                .await?;
                let isolate =
                    Python::attach(move |py| isolate.extract::<Py<PyIsolate>>(py).unwrap());
                guard.replace(Python::attach(|py| isolate.clone_ref(py)));

                slf.dead.store(true, atomic::Ordering::SeqCst);
                Ok(isolate)
            } else {
                Err(exceptions::PyRuntimeError::new_err(
                    "could not find isolate. did you reuse the session?",
                ))
            }
        })
    }

    #[pyo3(signature = (*_args))]
    fn __aexit__<'py>(
        slf: Py<Self>,
        py: Python<'py>,
        _args: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let mut guard = slf.get().isolate.lock().await;
            if let Some(me) = guard.take() {
                let me = me.get();
                Ok(me.close())
            } else {
                Err(exceptions::PyRuntimeError::new_err(
                    "could not find isolate. did you reuse the session?",
                ))
            }
        })
    }
}

/// Isolate execution task.
async fn isolate_task(mut isolate_rx: IsolateRx, py_tx: PythonTx) {
    let isolate = &mut Isolate::new(CreateParams::default());

    let handle_scope = std::pin::pin!(v8::HandleScope::new(isolate));
    let handle_scope = &mut handle_scope.init();
    let context = v8::Context::new(handle_scope, Default::default());

    let mut context_scope_ref = Box::new(v8::ContextScope::new(handle_scope, context));
    let context_scope = context_scope_ref.as_mut();

    // safety: `state` drops before the scope
    let state = Arc::new(IsolateState::new(py_tx, context_scope));

    while let Some(request) = isolate_rx.recv().await {
        match request {
            IsolateRequest::Hello { reply } => {
                reply.send(state.clone()).expect("failed initial handshake");
            }

            IsolateRequest::Run { source, reply } => {
                let maybe_result = 'result_block: {
                    let mut scope = state.ctx_scope.lock().await;
                    let try_catch = std::pin::pin!(TryCatch::new(&mut *scope.get_ctx_scope()));
                    let try_catch = &mut try_catch.init();
                    let Some(script) = Script::compile(
                        try_catch,
                        v8::String::new(try_catch, &source).unwrap(),
                        None,
                    ) else {
                        break 'result_block Err(try_catch
                            .exception()
                            .unwrap()
                            .into_py_exception(try_catch));
                    };

                    let Some(result) = script.run(try_catch) else {
                        break 'result_block Err(try_catch
                            .exception()
                            .unwrap()
                            .into_py_exception(try_catch));
                    };

                    Ok(result)
                };

                if maybe_result.is_err() {
                    reply
                        .send(IsolateRunResult::Error(maybe_result.unwrap_err()))
                        .ok();
                    continue;
                }

                let Ok(result) = maybe_result else {
                    unsafe { unreachable_unchecked() }
                };

                if result.is_promise() {
                    let (reply_to_py, recv_from_js) = oneshot::channel::<Py<PyValue>>();
                    let reply_to_py = Mutex::new(Some(reply_to_py));

                    let data = Arc::new((state.clone(), reply_to_py));

                    let callback = Function::builder(
                        |scope: &mut PinnedRef<HandleScope>,
                         args: FunctionCallbackArguments,
                         _rv: ReturnValue| {
                            let weak = unsafe {
                                sync::Weak::from_raw(args.data().cast::<External>().value()
                                    as *const (
                                        Arc<IsolateState>,
                                        Mutex<Option<oneshot::Sender<Py<PyValue>>>>,
                                    ))
                            };
                            if let Some(weak_data) = weak.upgrade() {
                                let mut guard = weak_data.1.lock().unwrap();
                                let sender = guard.take().unwrap();
                                sender
                                    .send(Python::attach(|py| {
                                        Py::new(
                                            py,
                                            PyValue::new(
                                                weak_data.0.clone(),
                                                Global::new(scope, args.get(0)),
                                            ),
                                        )
                                        .unwrap()
                                    }))
                                    .ok();
                            } else {
                                panic!(
                                    "while running function callback, found reply fn to be gone"
                                );
                            }
                        },
                    )
                    .data(
                        External::new(
                            context_scope,
                            Arc::downgrade(&data).into_raw() as *mut c_void,
                        )
                        .into(),
                    )
                    .build(context_scope)
                    .unwrap();

                    let promise = result.cast::<Promise>();
                    promise.then(context_scope, callback).unwrap();

                    reply
                        .send(IsolateRunResult::Promise((data, recv_from_js)))
                        .ok();
                } else {
                    reply
                        .send(IsolateRunResult::Value(Python::attach(|py| {
                            PyValue::new(state.clone(), Global::new(context_scope, result))
                                .into_py_any(py)
                        })))
                        .ok();
                }

                context_scope.perform_microtask_checkpoint();
            }

            IsolateRequest::AddFn { name } => {
                let weak_item = {
                    let arc_stack_item = ArcStackItem(Arc::new((
                        Arc::downgrade(&state),
                        ijson::ijson!(name.clone()),
                    )));
                    let weak_item = arc_stack_item.get_weak();
                    let mut stack = state.stack.lock().await;
                    stack.push(arc_stack_item);

                    weak_item
                };

                let mut scope = state.ctx_scope.lock().await;
                let ctx_scope = scope.get_ctx_scope();

                let ptr = weak_item.into_raw();
                let external = External::new(context_scope, ptr as *mut c_void).cast::<v8::Value>();

                let func = Function::builder(
                    |scope: &mut PinnedRef<HandleScope>,
                     args: FunctionCallbackArguments,
                     mut rv: ReturnValue| {
                        let promise_resolver = PromiseResolver::new(scope).unwrap();
                        rv.set(promise_resolver.get_promise(scope).cast::<v8::Value>());

                        let global_pr = Global::new(scope, promise_resolver);

                        let weak_data = unsafe {
                            sync::Weak::from_raw(args.data().cast::<External>().value()
                                as *mut (sync::Weak<IsolateState>, ijson::IValue))
                        };

                        let data = weak_data.upgrade().unwrap();
                        let state = data.0.upgrade().unwrap();

                        let (reply_to_js, recv_from_py) = oneshot::channel();
                        state
                            .py_tx
                            .send(PythonRequest::RunFn {
                                name: data.1.as_string().unwrap().to_string(),
                                args: Python::attach(|py| {
                                    Py::new(
                                        py,
                                        PyValue::new(
                                            state.clone(),
                                            Global::new(scope, args.get(0)),
                                        ),
                                    )
                                    .unwrap()
                                }),
                                reply: reply_to_js,
                            })
                            .ok();

                        state.clone().tasks.spawn_local(async move {
                            let result = recv_from_py.await.unwrap();
                            let mut ctx_scope = state.ctx_scope.lock().await;

                            let scope =
                                std::pin::pin!(v8::HandleScope::new(ctx_scope.get_ctx_scope()));
                            let scope = &mut scope.init();

                            let resolver = v8::Local::new(scope, global_pr);

                            let global_res = result.take(scope);
                            let data = v8::Local::new(scope, global_res);

                            resolver.resolve(scope, data);
                        });
                    },
                )
                .data(external)
                .build(ctx_scope)
                .unwrap();

                {
                    let global = context.global(context_scope);
                    let name = v8::String::new(context_scope, &name)
                        .unwrap()
                        .cast::<v8::Value>();
                    global.set(ctx_scope, name, func.cast::<v8::Value>());
                }
            }

            IsolateRequest::Close => break,
        }
    }

    {
        // clean up
        state.close().await;
    }
}

/// Python execution task.
async fn python_task(mut py_rx: PythonRx, py_isolate: Py<PyIsolate>) {
    let functions: DashMap<String, Py<PyAny>> = DashMap::new();

    while let Some(message) = py_rx.recv().await {
        match message {
            PythonRequest::AddFn { name, handler } => {
                functions.insert(name, handler);
            }

            PythonRequest::RunFn { name, args, reply } => {
                if let Some(func) = functions.get(&name) {
                    let result = Python::attach(|py| {
                        pyo3_async_runtimes::tokio::into_future(
                            func.clone_ref(py)
                                .call1(py, (py_isolate.clone_ref(py), args))
                                .unwrap()
                                .into_bound(py),
                        )
                    })
                    .unwrap()
                    .await
                    .unwrap();

                    let result = Python::attach(|py| {
                        result
                            .extract::<Py<PyValue>>(py)
                            .unwrap()
                            .borrow_mut(py)
                            .data
                            .take()
                            .unwrap()
                    });
                    reply.send(result).ok();
                }
            }

            PythonRequest::Close => break,
        }
    }
}
