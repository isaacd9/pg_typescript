use deno_core::{op2, JsRuntime};

#[op2(fast)]
pub fn op_pg_console_log(#[string] level: &str, #[string] msg: &str) {
    emit_console_line(level, msg);
}

#[cfg(any(not(test), feature = "pg_test"))]
fn emit_console_line(level: &str, msg: &str) {
    match level {
        "warn" | "error" => pgrx::warning!("[pg_typescript:{level}] {msg}"),
        "info" => pgrx::info!("[pg_typescript:{level}] {msg}"),
        _ => pgrx::log!("[pg_typescript:{level}] {msg}"),
    }
}

#[cfg(all(test, not(feature = "pg_test")))]
fn emit_console_line(level: &str, msg: &str) {
    eprintln!("[pg_typescript:{level}] {msg}");
}

deno_core::extension!(
    pg_typescript_console,
    ops = [op_pg_console_log],
    esm_entry_point = "ext:pg_typescript_console/console_bridge.js",
    esm = [ dir "src/js", "console_bridge.js" ],
);

const CONSOLE_HOOK_JS: &str = include_str!("../js/console_hook.js");

pub fn install_console_hook(rt: &mut JsRuntime) {
    rt.execute_script("pg_typescript:console_hook", CONSOLE_HOOK_JS)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to install console hook: {e}"));
}

/// Re-apply the console hook in case runtime bootstrap code replaced console methods.
pub fn ensure_console_hook(rt: &mut JsRuntime) {
    install_console_hook(rt);
}
