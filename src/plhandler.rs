use std::ffi::CStr;

use deno_core::v8;
use pgrx::pg_catalog::pg_proc::PgProc;
use pgrx::prelude::*;
use pgrx::{fcinfo, pg_sys};

use crate::convert::{datum_to_json, json_to_datum};
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

    // Collect argument datums → JSON.
    let args_json: Vec<serde_json::Value> = (0..nargs)
        .map(|i| unsafe {
            let nd = fcinfo::pg_get_nullable_datum(fcinfo, i);
            datum_to_json(nd.value, nd.isnull, arg_types[i])
        })
        .collect();

    let args_array = serde_json::Value::Array(args_json);

    let (result_json, is_null) = execute_typescript_fn(&source, &param_names, args_array);

    if is_null {
        unsafe { fcinfo::pg_return_null(fcinfo) }
    } else {
        let (datum, null) = json_to_datum(&result_json, ret_type);
        if null {
            unsafe { fcinfo::pg_return_null(fcinfo) }
        } else {
            datum
        }
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
    let check_js = format!("(() => {{ function __check({params}) {{ {transformed} }} }})();");

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

/// Execute a TypeScript function body with pre-converted JSON arguments.
/// Returns `(result_json, is_null)`.
fn execute_typescript_fn(
    source: &str,
    param_names: &[String],
    args: serde_json::Value,
) -> (serde_json::Value, bool) {
    let args_json_str = serde_json::to_string(&args).unwrap_or_else(|_| "[]".to_string());
    let transformed = transform_imports(source);
    let has_async = transformed.contains("await ") || transformed.contains("import(");

    with_runtime(|rt| {
        if has_async {
            call_async(rt, &transformed, param_names, &args_json_str)
        } else {
            call_sync(rt, &transformed, param_names, &args_json_str)
        }
    })
}

/// Define and call an async function body, then drain the event loop.
fn call_async(
    rt: &mut deno_core::JsRuntime,
    transformed: &str,
    param_names: &[String],
    args_json_str: &str,
) -> (serde_json::Value, bool) {
    let params = param_names.join(", ");
    let call_js = format!(
        r#"
        globalThis.__pg_result = undefined;
        globalThis.__pg_error  = undefined;
        (async function({params}) {{ {transformed} }})(...{args_json_str})
            .then(r  => {{ globalThis.__pg_result = JSON.stringify(r); }})
            .catch(e => {{ globalThis.__pg_error  = String(e);         }});
        "#
    );
    if let Err(e) = rt.execute_script("<typescript:call_schedule>", call_js) {
        pgrx::error!("pg_typescript: call error: {e}");
    }

    if let Err(e) = block_on(rt.run_event_loop(Default::default())) {
        pgrx::error!("pg_typescript: event loop error: {e}");
    }

    if let Ok(err_global) =
        rt.execute_script("<typescript:check_error>", "globalThis.__pg_error")
    {
        deno_core::scope!(scope, rt);
        let err_local = v8::Local::new(scope, err_global);
        if !err_local.is_undefined() && !err_local.is_null() {
            pgrx::error!("pg_typescript: {}", err_local.to_rust_string_lossy(scope));
        }
    }

    let result_global = rt
        .execute_script("<typescript:get_result>", "globalThis.__pg_result")
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: get_result failed: {e}"));

    deno_core::scope!(scope, rt);
    let local = v8::Local::new(scope, result_global);
    json_from_v8(scope, local)
}

/// Define and call a synchronous function body, returning the result directly.
fn call_sync(
    rt: &mut deno_core::JsRuntime,
    transformed: &str,
    param_names: &[String],
    args_json_str: &str,
) -> (serde_json::Value, bool) {
    let params = param_names.join(", ");
    let call_js = format!(
        r#"
        (() => {{
            const __r = (function({params}) {{ {transformed} }})(...{args_json_str});
            return __r === undefined || __r === null ? null : JSON.stringify(__r);
        }})()
        "#
    );

    let result_global = rt
        .execute_script("<typescript:call>", call_js)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

    deno_core::scope!(scope, rt);
    let local = v8::Local::new(scope, result_global);
    json_from_v8(scope, local)
}

/// Extract a JSON-serialised value from a V8 local (the result of `JSON.stringify`).
fn json_from_v8(
    scope: &mut deno_core::v8::ContextScope<'_, '_, deno_core::v8::HandleScope<'_>>,
    local: v8::Local<'_, v8::Value>,
) -> (serde_json::Value, bool) {
    if local.is_null_or_undefined() {
        return (serde_json::Value::Null, true);
    }
    let json_str = local.to_rust_string_lossy(scope);
    let value = serde_json::from_str(&json_str).unwrap_or(serde_json::Value::Null);
    (value, false)
}

/// Execute a DO block (anonymous JavaScript).
fn execute_inline_block(source: &str) {
    let transformed = transform_imports(source);
    let has_async = transformed.contains("await ") || transformed.contains("import(");

    with_runtime(|rt| {
        let js = if has_async {
            format!("(async () => {{ {transformed} }})();")
        } else {
            format!("(() => {{ {transformed} }})();")
        };

        if let Err(e) = rt.execute_script("<typescript:inline>", js) {
            pgrx::error!("pg_typescript: {e}");
        }

        if has_async {
            if let Err(e) = block_on(rt.run_event_loop(Default::default())) {
                pgrx::error!("pg_typescript: event loop error in DO block: {e}");
            }
        }
    });
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

    Some(format!(r#"const {bindings} = (await import("{url}")).default;"#))
}

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
