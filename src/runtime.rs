use std::cell::RefCell;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions, op2};

use crate::loader::PgModuleLoader;

thread_local! {
    static JS_RT: RefCell<Option<JsRuntime>> = RefCell::new(None);
    static TOKIO_RT: RefCell<Option<tokio::runtime::Runtime>> = RefCell::new(None);
}

#[op2(fast)]
fn op_pg_console_log(#[string] level: &str, #[string] msg: &str) {
    emit_console_line(level, msg);
}

#[cfg(not(test))]
fn emit_console_line(level: &str, msg: &str) {
    match level {
        "warn" | "error" => pgrx::warning!("[pg_typescript:{level}] {msg}"),
        "info" => pgrx::info!("[pg_typescript:{level}] {msg}"),
        _ => pgrx::log!("[pg_typescript:{level}] {msg}"),
    }
}

#[cfg(test)]
fn emit_console_line(level: &str, msg: &str) {
    eprintln!("[pg_typescript:{level}] {msg}");
}

deno_core::extension!(pg_typescript_console, ops = [op_pg_console_log]);

const CONSOLE_HOOK_JS: &str = r#"
(() => {
  const op = globalThis?.Deno?.core?.ops?.op_pg_console_log;
  if (typeof op !== "function" || typeof globalThis.console === "undefined") return;

  const stringify = (value) => {
    if (typeof value === "string") return value;
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  };

  const bind = (level) => (...args) => {
    const msg = args.map(stringify).join(" ");
    op(level, msg);
  };

  console.debug = bind("debug");
  console.log = bind("log");
  console.info = bind("info");
  console.warn = bind("warn");
  console.error = bind("error");
})();
"#;

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
    let mut runtime = JsRuntime::new(RuntimeOptions {
        module_loader: Some(Rc::new(PgModuleLoader)),
        extensions: vec![pg_typescript_console::init()],
        ..Default::default()
    });

    runtime
        .execute_script("pg_typescript:console_hook", CONSOLE_HOOK_JS)
        .expect("pg_typescript: failed to install console hook");

    runtime
}
