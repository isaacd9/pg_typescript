use std::ffi::CStr;

use deno_core::v8;
use pgrx::pg_catalog::pg_proc::PgProc;
use pgrx::prelude::*;
use pgrx::{fcinfo, pg_sys};

use crate::convert::{PgDatum, PgDatumSeed};
use crate::runtime::{block_on, with_runtime};

// ---------------------------------------------------------------------------
// Call handler — invoked for every call to a LANGUAGE typescript function.
// ---------------------------------------------------------------------------

#[pg_guard]
#[no_mangle]
pub unsafe extern "C-unwind" fn typescript_call_handler(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    let fn_oid = unsafe { (*(*fcinfo).flinfo).fn_oid };

    let proc = PgProc::new(fn_oid).unwrap_or_else(|| {
        pgrx::error!("pg_typescript: pg_proc entry not found for oid {fn_oid:?}");
    });

    let source = proc.prosrc();
    let ret_type = proc.prorettype();
    let arg_types = proc.proargtypes();
    let arg_names = proc.proargnames();
    let nargs = proc.pronargs();

    // Build friendly parameter names (fall back to "_0", "_1", …).
    let param_names: Vec<String> = (0..nargs)
        .map(|i| {
            arg_names
                .get(i)
                .and_then(|opt: &Option<String>| opt.as_deref())
                .filter(|s: &&str| !s.is_empty())
                .map(|s: &str| s.to_string())
                .unwrap_or_else(|| format!("_{i}"))
        })
        .collect();

    // Collect argument datums for direct V8 serialization.
    let args: Vec<PgDatum> = (0..nargs)
        .map(|i| unsafe {
            let nd = fcinfo::pg_get_nullable_datum(fcinfo, i);
            PgDatum { datum: nd.value, isnull: nd.isnull, oid: arg_types[i] }
        })
        .collect();

    let (datum, is_null) =
        execute_typescript_fn(&source, &param_names, &args, PgDatumSeed { oid: ret_type });

    if is_null {
        return unsafe { fcinfo::pg_return_null(fcinfo) };
    } else {
        datum
    }
}

// ---------------------------------------------------------------------------
// Validator — called at CREATE FUNCTION time to check JS syntax.
// ---------------------------------------------------------------------------

#[pg_guard]
#[no_mangle]
pub unsafe extern "C-unwind" fn typescript_validator(fn_oid: pg_sys::Oid) {
    let proc: PgProc = match PgProc::new(fn_oid) {
        Some(p) => p,
        None => return,
    };

    let source = proc.prosrc();
    let nargs = proc.pronargs();
    let param_names: Vec<String> = (0..nargs).map(|i| format!("_{i}")).collect();
    let params = param_names.join(", ");

    let transformed = transform_imports(&source);
    // async function so that `await` inside transformed bodies is valid syntax.
    let check_js = format!("(() => {{ async function __check({params}) {{ {transformed} }} }})();");

    with_runtime(|rt| {
        if let Err(e) = rt.execute_script("<typescript:validator>", check_js) {
            pgrx::error!("pg_typescript: syntax error: {e}");
        }
    });
}

// ---------------------------------------------------------------------------
// Inline handler — called for DO $$ … $$ LANGUAGE typescript blocks.
// ---------------------------------------------------------------------------

#[pg_guard]
#[no_mangle]
pub unsafe extern "C-unwind" fn typescript_inline_handler(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    unsafe {
        // arg 0 is an InlineCodeBlock*.
        let nd = fcinfo::pg_get_nullable_datum(fcinfo, 0);
        if nd.isnull {
            return fcinfo::pg_return_void();
        }
        let icb = nd.value.cast_mut_ptr::<pg_sys::InlineCodeBlock>();
        let source = CStr::from_ptr((*icb).source_text)
            .to_str()
            .unwrap_or("")
            .to_string();

        execute_inline_block(&source);
        fcinfo::pg_return_void()
    }
}

// ---------------------------------------------------------------------------
// Core execution logic
// ---------------------------------------------------------------------------

/// Execute a TypeScript function body.
///
/// `args` is serialized directly into V8 values — no JSON string round-trip.
/// `seed` drives how the resolved return value is converted to `R`.
fn execute_typescript_fn<A, S, R>(
    source: &str,
    param_names: &[String],
    args: &[A],
    seed: S,
) -> R
where
    A: serde::Serialize,
    S: for<'de> serde::de::DeserializeSeed<'de, Value = R>,
{
    let transformed = transform_imports(source);
    let params = param_names.join(", ");

    with_runtime(|rt| {
        // Define the async function (expression, not an IIFE — we call it
        // separately via the V8 API so we can pass args as native V8 values).
        let fn_js = format!("(async function({params}) {{ {transformed} }})");
        let fn_global = rt
            .execute_script("<typescript:def>", fn_js)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

        // Call the function with directly serialized args → returns a Promise global.
        let promise_global = call_fn_with_args(rt, fn_global, args);

        // Drain the event loop so any async work (await, microtasks) completes.
        block_on(rt.run_event_loop(Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

        // Resolve the now-settled Promise.
        let resolve_fut = rt.resolve(promise_global);
        let resolved = block_on(rt.with_event_loop_promise(resolve_fut, Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

        global_to(rt, resolved, seed)
    })
}

/// Execute a DO block (anonymous JavaScript).
fn execute_inline_block(source: &str) {
    let transformed = transform_imports(source);
    with_runtime(|rt| {
        // Always wrap in an async IIFE so `await` works and we get a Promise back.
        let js = format!("(async () => {{ {transformed} }})()");
        let result_global = rt
            .execute_script("<typescript:inline>", js)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

        let resolve_fut = rt.resolve(result_global);
        block_on(rt.with_event_loop_promise(resolve_fut, Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: event loop error in DO block: {e}"));
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Call a V8 function with args serialized directly from Rust values.
///
/// Must be a plain `fn` (not a closure) for the same `deno_core::scope!`
/// lifetime reason as `global_to`.  Returns a Global holding the Promise
/// produced by the async function call.
fn call_fn_with_args<A: serde::Serialize>(
    rt: &mut deno_core::JsRuntime,
    fn_global: v8::Global<v8::Value>,
    args: &[A],
) -> v8::Global<v8::Value> {
    deno_core::scope!(scope, rt);
    let fn_local = v8::Local::new(scope, fn_global);
    let fn_obj = v8::Local::<v8::Function>::try_from(fn_local)
        .unwrap_or_else(|_| pgrx::error!("pg_typescript: script did not return a function"));

    let v8_args: Vec<v8::Local<v8::Value>> = args
        .iter()
        .map(|arg| {
            deno_core::serde_v8::to_v8(scope, arg)
                .unwrap_or_else(|e| pgrx::error!("pg_typescript: arg serialize: {e}"))
        })
        .collect();

    let recv = v8::undefined(scope).into();
    let result = fn_obj
        .call(scope, recv, &v8_args)
        .unwrap_or_else(|| pgrx::error!("pg_typescript: function call returned None"));

    v8::Global::new(scope, result)
}

/// Deserialize a resolved V8 global using `seed`.
///
/// Must be a plain `fn` (not a closure) so that `deno_core::scope!` temporaries
/// live long enough — see the `eval_js_value` pattern in deno_core's examples.
fn global_to<S, R>(
    rt: &mut deno_core::JsRuntime,
    global: v8::Global<v8::Value>,
    seed: S,
) -> R
where
    S: for<'de> serde::de::DeserializeSeed<'de, Value = R>,
{
    deno_core::scope!(scope, rt);
    let local = v8::Local::new(scope, global);
    let mut de = deno_core::serde_v8::Deserializer::new(scope, local, None);
    seed.deserialize(&mut de)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: deserialize: {e}"))
}

// ---------------------------------------------------------------------------
// Source transformation helpers
// ---------------------------------------------------------------------------

/// Convert static ES `import` statements to dynamic `import()` calls that can
/// live inside a function body.
///
/// Handles the common forms:
/// - `import x from "pkg"` → `const x = (await import("URL")).default;`
/// - `import { a, b } from "pkg"` → `const { a, b } = await import("URL");`
/// - `import * as ns from "pkg"` → `const ns = await import("URL");`
fn transform_imports(source: &str) -> String {
    let mut out = String::with_capacity(source.len());

    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(transformed) = try_transform_import_line(trimmed) {
            let indent = &line[..line.len() - trimmed.len()];
            out.push_str(indent);
            out.push_str(&transformed);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }

    out
}

fn try_transform_import_line(line: &str) -> Option<String> {
    if !line.starts_with("import ") {
        return None;
    }

    let from_pos = line.rfind(" from ")?;
    let bindings = line[7..from_pos].trim();

    let after_from = line[from_pos + 6..].trim();
    let mut chars = after_from.chars();
    let quote = chars.next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = chars.as_str();
    let end = rest.find(quote)?;
    let specifier = &rest[..end];

    let url = if specifier.starts_with("http://") || specifier.starts_with("https://") {
        specifier.to_string()
    } else {
        format!("https://esm.sh/{specifier}")
    };

    if bindings.starts_with('{') {
        let destructure = bindings.replace(" as ", ": ");
        return Some(format!(r#"const {destructure} = await import("{url}");"#));
    }

    if bindings.starts_with("* as ") {
        let ns = &bindings[5..];
        return Some(format!(r#"const {ns} = await import("{url}");"#));
    }

    if bindings.contains(',') {
        let comma = bindings.find(',')?;
        let default_name = bindings[..comma].trim();
        let named = bindings[comma + 1..].trim();
        return Some(format!(
            "const __mod_{default_name} = await import(\"{url}\");\
             \nconst {default_name} = __mod_{default_name}.default;\
             \nconst {named} = __mod_{default_name};"
        ));
    }

    Some(format!(
        r#"const {bindings} = (await import("{url}")).default;"#
    ))
}

// Plain Rust unit tests — run with `cargo test`, no postgres needed.
#[cfg(test)]
mod unit_tests {
    /// Seed used in tests: deserializes any V8 value into `(serde_json::Value, bool)`.
    struct JsonSeed;

    impl<'de> serde::de::DeserializeSeed<'de> for JsonSeed {
        type Value = (serde_json::Value, bool);

        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            use serde::de::Deserialize;
            let v = serde_json::Value::deserialize(deserializer)?;
            let is_null = v.is_null();
            Ok((v, is_null))
        }
    }

    fn run(source: &str, params: &[&str], args: &[serde_json::Value]) -> (serde_json::Value, bool) {
        let param_names: Vec<String> = params.iter().map(|s| s.to_string()).collect();
        super::execute_typescript_fn(source, &param_names, args, JsonSeed)
    }

    #[test]
    fn test_call_number() {
        let (val, is_null) =
            run("return a + b;", &["a", "b"], &[serde_json::json!(1), serde_json::json!(2)]);
        assert!(!is_null);
        assert_eq!(val, serde_json::json!(3));
    }

    #[test]
    fn test_call_string() {
        let (val, is_null) =
            run("return `Hello, ${name}!`;", &["name"], &[serde_json::json!("world")]);
        assert!(!is_null);
        assert_eq!(val, serde_json::json!("Hello, world!"));
    }

    #[test]
    fn test_call_bool() {
        let (val, is_null) =
            run("return a > b;", &["a", "b"], &[serde_json::json!(3.0), serde_json::json!(1.5)]);
        assert!(!is_null);
        assert_eq!(val, serde_json::json!(true));
    }

    #[test]
    fn test_call_null_return() {
        let (_val, is_null) = run("return null;", &[], &[]);
        assert!(is_null);
    }

    #[test]
    fn test_call_object() {
        let (val, is_null) = run("return { x: n * 2 };", &["n"], &[serde_json::json!(21)]);
        assert!(!is_null);
        assert_eq!(val, serde_json::json!({ "x": 42 }));
    }

    #[test]
    fn test_async_number() {
        let (val, is_null) =
            run("return await Promise.resolve(n * 2);", &["n"], &[serde_json::json!(21)]);
        assert!(!is_null);
        assert_eq!(val, serde_json::json!(42));
    }

    #[test]
    fn test_async_string() {
        let (val, is_null) = run(
            "const greeting = await Promise.resolve(`Hello, ${name}!`);
             return greeting;",
            &["name"],
            &[serde_json::json!("world")],
        );
        assert!(!is_null);
        assert_eq!(val, serde_json::json!("Hello, world!"));
    }

    #[test]
    fn test_async_chained_awaits() {
        let (val, is_null) = run(
            "const a = await Promise.resolve(x + 1);
             const b = await Promise.resolve(a * 2);
             return b;",
            &["x"],
            &[serde_json::json!(4)],
        );
        assert!(!is_null);
        assert_eq!(val, serde_json::json!(10));
    }

    #[test]
    fn test_async_null_return() {
        let (_val, is_null) = run("return await Promise.resolve(null);", &[], &[]);
        assert!(is_null);
    }

    #[test]
    fn test_async_object() {
        let (val, is_null) = run(
            "const doubled = await Promise.resolve(n * 2);
             return { original: n, doubled };",
            &["n"],
            &[serde_json::json!(7)],
        );
        assert!(!is_null);
        assert_eq!(val, serde_json::json!({ "original": 7, "doubled": 14 }));
    }
}

// SQL-level integration tests — run with `cargo pgrx test pg18`.
#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_simple_add() {
        Spi::run(
            "CREATE FUNCTION test_add(a int, b int) RETURNS int \
             LANGUAGE typescript AS $$ return a + b; $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>("SELECT test_add(1, 2)")
            .unwrap()
            .unwrap();
        assert_eq!(result, 3);
    }

    #[pg_test]
    fn test_string_function() {
        Spi::run(
            "CREATE FUNCTION test_greet(name text) RETURNS text \
             LANGUAGE typescript AS $$ return `Hello, ${name}!`; $$",
        )
        .unwrap();
        let result = Spi::get_one::<String>("SELECT test_greet('world')")
            .unwrap()
            .unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[pg_test]
    fn test_bool_return() {
        Spi::run(
            "CREATE FUNCTION test_gt(a float8, b float8) RETURNS bool \
             LANGUAGE typescript AS $$ return a > b; $$",
        )
        .unwrap();
        let t = Spi::get_one::<bool>("SELECT test_gt(3.0, 1.5)")
            .unwrap()
            .unwrap();
        assert!(t);
    }
}
