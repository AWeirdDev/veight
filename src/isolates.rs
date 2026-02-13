use std::{
    ffi::c_void,
    hint::unreachable_unchecked,
    mem::ManuallyDrop,
    sync::{self, Arc, Mutex},
};

use dashmap::DashMap;
use once_cell::sync::OnceCell;
use pyo3::{IntoPyObjectExt, exceptions, prelude::*};
use tokio::sync::{mpsc, oneshot};
use v8::{
    External, Function, FunctionCallbackArguments, Global, HandleScope, Isolate, PinnedRef,
    Promise, PromiseResolver, ReturnValue, Script, TryCatch,
};

use crate::{bridging::PyValue, stack::ArcStackItem, transmit::*};

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

    #[pyo3(name = "create_isolate")]
    fn py_create_isolate<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pyo3_async_runtimes::tokio::future_into_py(py, async {
            let (isolate_tx, isolate_rx) = mpsc::unbounded_channel::<IsolateRequest>();
            let (python_tx, python_rx) = mpsc::unbounded_channel::<PythonRequest>();

            // background tasks
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

                tokio::task::spawn(pyo3_async_runtimes::tokio::scope(
                    Python::attach(|py| {
                        pyo3_async_runtimes::TaskLocals::new(
                            pyo3_async_runtimes::tokio::get_current_locals(py)
                                .unwrap()
                                .event_loop(py),
                        )
                    }),
                    python_task(python_rx),
                ));
            }

            Ok(PyIsolate {
                isolate_tx,
                python_tx,
            })
        })
    }
}

#[pyclass(name = "Isolate", frozen)]
pub struct PyIsolate {
    isolate_tx: IsolateTx,
    python_tx: PythonTx,
}

#[pymethods]
impl PyIsolate {
    #[pyo3(name = "add_function")]
    fn py_add_fn(&self, name: String, func: Py<PyAny>) -> PyResult<()> {
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
                    "failed to send python add function handler request, reason: {}",
                    err.to_string()
                ))
            })
    }

    #[pyo3(name = "run")]
    fn py_run<'py>(&self, py: Python<'py>, source: String) -> PyResult<Bound<'py, PyAny>> {
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
                IsolateRunResult::Error(err) => Err(exceptions::PyRuntimeError::new_err(err)),
            }
        })
    }
}

/// Isolate execution task.
async fn isolate_task(mut isolate_rx: IsolateRx, py_tx: PythonTx) {
    let isolate = &mut Isolate::new(Default::default());

    let handle_scope = std::pin::pin!(v8::HandleScope::new(isolate));
    let handle_scope = &mut handle_scope.init();
    let context = v8::Context::new(handle_scope, Default::default());

    let mut context_scope_ref = Box::new(v8::ContextScope::new(handle_scope, context));
    let context_scope = context_scope_ref.as_mut();

    // safety: `state` drops before the scope
    let state = Arc::new(IsolateState::new(py_tx, context_scope));

    while let Some(request) = isolate_rx.recv().await {
        match request {
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
                            .to_rust_string_lossy(try_catch));
                    };

                    let Some(result) = script.run(try_catch) else {
                        break 'result_block Err(try_catch
                            .exception()
                            .unwrap()
                            .to_rust_string_lossy(try_catch));
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

                            let data = v8::String::new(scope, &result).unwrap().cast::<v8::Value>();
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
        }
    }

    {
        // println!("cleaning up...");
        state.tasks.wait().await;
        state.tasks.close();
    }
}

/// Python execution task.
async fn python_task(mut py_rx: PythonRx) {
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
                                .call1(py, (args,))
                                .unwrap()
                                .into_bound(py),
                        )
                    })
                    .unwrap()
                    .await;
                    reply.send(result.unwrap().to_string()).ok();
                }
            }
        }
    }
}
