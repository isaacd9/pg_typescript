use std::cell::RefCell;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions};

use crate::loader::PgModuleLoader;

thread_local! {
    static JS_RT: RefCell<Option<JsRuntime>> = RefCell::new(None);
    static TOKIO_RT: RefCell<Option<tokio::runtime::Runtime>> = RefCell::new(None);
}

/// Run `f` with the per-connection JsRuntime, initialising it on first use.
pub fn with_runtime<F, R>(f: F) -> R
where
    F: FnOnce(&mut JsRuntime) -> R,
{
    JS_RT.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(create_runtime());
        }
        f(borrow.as_mut().unwrap())
    })
}

/// Block the current thread on an async future, using a per-connection
/// single-threaded Tokio runtime.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    TOKIO_RT.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("pg_typescript: failed to create tokio runtime"),
            );
        }
    });

    TOKIO_RT.with(|cell| cell.borrow().as_ref().unwrap().block_on(future))
}

fn create_runtime() -> JsRuntime {
    JsRuntime::new(RuntimeOptions {
        module_loader: Some(Rc::new(PgModuleLoader)),
        ..Default::default()
    })
}
