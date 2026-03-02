use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};

use deno_core::v8;
use pgrx::pg_catalog::pg_proc::PgProc;
use pgrx::prelude::*;
use pgrx::{fcinfo, pg_sys};

use crate::convert::{PgDatum, PgDatumSeed};
use crate::fetch;
use crate::loader;
use crate::runtime::{block_on, with_runtime};

// ---------------------------------------------------------------------------
// PostgreSQL V1 function-info records for our three PL handler entry points.
//
// PostgreSQL 18 calls  dlsym(so, "pg_finfo_<prosrc>")  to check the calling
// convention of every C function.  Without these declarations PostgreSQL
// cannot find the info record and raises "could not find function information
// for function …" when the extension is installed.
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn pg_finfo_typescript_call_handler() -> &'static pg_sys::Pg_finfo_record {
    const INFO: pg_sys::Pg_finfo_record = pg_sys::Pg_finfo_record { api_version: 1 };
    &INFO
}

#[no_mangle]
pub extern "C" fn pg_finfo_typescript_inline_handler() -> &'static pg_sys::Pg_finfo_record {
    const INFO: pg_sys::Pg_finfo_record = pg_sys::Pg_finfo_record { api_version: 1 };
    &INFO
}

#[no_mangle]
pub extern "C" fn pg_finfo_typescript_validator() -> &'static pg_sys::Pg_finfo_record {
    const INFO: pg_sys::Pg_finfo_record = pg_sys::Pg_finfo_record { api_version: 1 };
    &INFO
}

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

    let import_map = read_import_map(&proc);

    let args: Vec<PgDatum> = (0..nargs)
        .map(|i| unsafe {
            let nd = fcinfo::pg_get_nullable_datum(fcinfo, i);
            PgDatum { datum: nd.value, isnull: nd.isnull, oid: arg_types[i] }
        })
        .collect();

    #[cfg(not(test))]
    let store = fetch::PgModuleStore;
    #[cfg(test)]
    let store = fetch::HashMapModuleStore::new();

    let (datum, is_null) = execute_typescript_fn(
        fn_oid,
        &source,
        &import_map,
        &param_names,
        &args,
        PgDatumSeed { oid: ret_type },
        store,
    );

    if is_null {
        unsafe { fcinfo::pg_return_null(fcinfo) }
    } else {
        datum
    }
}

// ---------------------------------------------------------------------------
// Validator — called at CREATE FUNCTION time.
// ---------------------------------------------------------------------------

#[pg_guard]
#[no_mangle]
pub unsafe extern "C-unwind" fn typescript_validator(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    // Postgres passes the to-be-validated function's OID as arg 0.
    let fn_oid: pg_sys::Oid = unsafe {
        pg_sys::Oid::from(fcinfo::pg_get_nullable_datum(fcinfo, 0).value.value() as u32)
    };

    let proc: PgProc = match PgProc::new(fn_oid) {
        Some(p) => p,
        None => return fcinfo::pg_return_void(),
    };

    let source = proc.prosrc();
    let nargs = proc.pronargs();
    let arg_names = proc.proargnames();
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
    let params = param_names.join(", ");
    let import_map = read_import_map(&proc);

    // If the function declares an import map, fetch all dependencies now.
    #[cfg(not(test))]
    if !import_map.is_empty() {
        fetch::fetch_and_cache(u32::from(fn_oid), &import_map, &mut fetch::PgModuleStore, &fetch::UreqFetcher);
    }

    // Assemble the same module source the call handler will use so that the
    // specifier and hash match and we can pre-warm FN_CACHE.
    let module_source = assemble_module(&source, &import_map, &params);
    let source_hash = hash_str(&module_source);
    let oid_raw = u32::from(fn_oid);

    #[cfg(not(test))]
    let store = fetch::PgModuleStore;
    #[cfg(test)]
    let store = fetch::HashMapModuleStore::new();

    // Use the same fn_ specifier as the call handler. Loading as a side module
    // (not main) lets multiple functions coexist in the same long-lived runtime.
    // V8 always eagerly parses ES module bodies, so syntax errors are caught here.
    // Pre-warming FN_CACHE means the first SELECT call skips module loading entirely.
    let specifier_str = format!("file:///pg_typescript/fn_{fn_oid}_{source_hash:016x}.mjs");
    let specifier = deno_core::resolve_url(&specifier_str)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid specifier: {e}"));

    with_runtime(|rt| {
        let _ctx = loader::set_loader_context(oid_raw, import_map.clone(), Box::new(store));

        let module_id =
            block_on(rt.load_side_es_module_from_code(&specifier, module_source))
                .unwrap_or_else(|e| pgrx::error!("pg_typescript: syntax error: {e}"));

        let evaluate = rt.mod_evaluate(module_id);
        block_on(rt.with_event_loop_promise(evaluate, Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: module evaluation failed: {e}"));

        let namespace = rt
            .get_module_namespace(module_id)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: get_module_namespace: {e}"));
        let f = extract_default_export(rt, namespace);

        FN_CACHE.with(|c| c.borrow_mut().insert((oid_raw, source_hash), f));
    });

    fcinfo::pg_return_void()
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
        let nd = fcinfo::pg_get_nullable_datum(fcinfo, 0);
        if nd.isnull {
            return fcinfo::pg_return_void();
        }
        let icb = nd.value.cast_mut_ptr::<pg_sys::InlineCodeBlock>();
        let source = CStr::from_ptr((*icb).source_text)
            .to_str()
            .unwrap_or("")
            .to_string();

        let import_map = read_inline_import_map();

        // Fetch and cache all dependencies before execution — network access
        // happens here, never during the execute step.
        #[cfg(not(test))]
        if !import_map.is_empty() {
            fetch::fetch_and_cache(0u32, &import_map, &mut fetch::PgModuleStore, &fetch::UreqFetcher);
        }

        #[cfg(not(test))]
        let store = fetch::PgModuleStore;
        #[cfg(test)]
        let store = fetch::HashMapModuleStore::new();

        execute_inline_block(&source, &import_map, store);
        fcinfo::pg_return_void()
    }
}

// ---------------------------------------------------------------------------
// Core execution
// ---------------------------------------------------------------------------

// Per-connection cache: (fn_oid, source_hash) → compiled default-export function.
// Keying on source_hash means ALTER FUNCTION (which changes the source) automatically
// gets a fresh entry, while repeated calls reuse the already-compiled module.
thread_local! {
    static FN_CACHE: RefCell<HashMap<(u32, u64), v8::Global<v8::Value>>> =
        RefCell::new(HashMap::new());
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Execute a TypeScript function body as an ES module.
///
/// The import map (possibly empty) is used to:
/// 1. Generate `import * as <key> from "<key>"` statements.
/// 2. Configure the module loader so it can resolve bare specifiers and
///    fetch sources from `deno_internal.deno_package_modules`.
fn execute_typescript_fn<MS, A, S, R>(
    fn_oid: pg_sys::Oid,
    source: &str,
    import_map: &HashMap<String, String>,
    param_names: &[String],
    args: &[A],
    seed: S,
    store: MS,
) -> R
where
    MS: fetch::ModuleStore + 'static,
    A: serde::Serialize,
    S: for<'de> serde::de::DeserializeSeed<'de, Value = R>,
{
    let params = param_names.join(", ");
    let module_source = assemble_module(source, import_map, &params);
    let source_hash = hash_str(&module_source);
    let oid_raw = u32::from(fn_oid);

    with_runtime(|rt| {
        // Look up (oid, source_hash) in the per-connection cache.
        // Cache hit: skip module loading, compilation, and loader context setup entirely.
        let fn_global = FN_CACHE.with(|c| c.borrow().get(&(oid_raw, source_hash)).cloned());

        let fn_global = match fn_global {
            Some(f) => f,
            None => {
                // Set loader context only on cache miss — the module loader is
                // only called during initial load, never when calling a cached function.
                let _ctx = loader::set_loader_context(oid_raw, import_map.clone(), Box::new(store));

                // Specifier is stable per (function, source version): ALTER FUNCTION changes
                // the source, which changes the hash and therefore triggers a fresh load.
                let specifier_str =
                    format!("file:///pg_typescript/fn_{fn_oid}_{source_hash:016x}.mjs");
                let specifier = deno_core::resolve_url(&specifier_str)
                    .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid specifier: {e}"));

                let module_id =
                    block_on(rt.load_side_es_module_from_code(&specifier, module_source))
                        .unwrap_or_else(|e| pgrx::error!("pg_typescript: module load error: {e}"));

                // Evaluate the module; future must be awaited so errors are not silently lost.
                let evaluate = rt.mod_evaluate(module_id);
                block_on(rt.with_event_loop_promise(evaluate, Default::default()))
                    .unwrap_or_else(|e| {
                        pgrx::error!("pg_typescript: module evaluation failed: {e}")
                    });

                let namespace = rt
                    .get_module_namespace(module_id)
                    .unwrap_or_else(|e| pgrx::error!("pg_typescript: get_module_namespace: {e}"));
                let f = extract_default_export(rt, namespace);

                FN_CACHE.with(|c| c.borrow_mut().insert((oid_raw, source_hash), f.clone()));
                f
            }
        };

        let promise_global = call_fn_with_args(rt, fn_global, args);
        let resolve_fut = rt.resolve(promise_global);
        let resolved = block_on(rt.with_event_loop_promise(resolve_fut, Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

        global_to(rt, resolved, seed)
    })
}

/// Execute a DO block as an ES module. Follows the same load/cache/call path
/// as regular functions, using OID 0 as the synthetic key in FN_CACHE.
fn execute_inline_block<MS: fetch::ModuleStore + 'static>(
    source: &str,
    import_map: &HashMap<String, String>,
    store: MS,
) {
    let module_source = assemble_module(source, import_map, "");
    let source_hash = hash_str(&module_source);

    with_runtime(|rt| {
        let fn_global = FN_CACHE.with(|c| c.borrow().get(&(0u32, source_hash)).cloned());

        let fn_global = match fn_global {
            Some(f) => f,
            None => {
                // Set loader context only on cache miss.
                let _ctx = loader::set_loader_context(0u32, import_map.clone(), Box::new(store));

                let specifier_str =
                    format!("file:///pg_typescript/do_{source_hash:016x}.mjs");
                let specifier = deno_core::resolve_url(&specifier_str)
                    .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid specifier: {e}"));

                let module_id =
                    block_on(rt.load_side_es_module_from_code(&specifier, module_source))
                        .unwrap_or_else(|e| pgrx::error!("pg_typescript: module load error: {e}"));

                let evaluate = rt.mod_evaluate(module_id);
                block_on(rt.with_event_loop_promise(evaluate, Default::default()))
                    .unwrap_or_else(|e| {
                        pgrx::error!("pg_typescript: module evaluation failed: {e}")
                    });

                let namespace = rt
                    .get_module_namespace(module_id)
                    .unwrap_or_else(|e| pgrx::error!("pg_typescript: get_module_namespace: {e}"));
                let f = extract_default_export(rt, namespace);

                FN_CACHE.with(|c| c.borrow_mut().insert((0u32, source_hash), f.clone()));
                f
            }
        };

        let no_args: &[serde_json::Value] = &[];
        let promise_global = call_fn_with_args(rt, fn_global, no_args);
        let resolve_fut = rt.resolve(promise_global);
        block_on(rt.with_event_loop_promise(resolve_fut, Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: event loop error in DO block: {e}"));
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Assemble a complete ES module from the user's function body and import map.
///
/// Example output:
/// ```javascript
/// import * as lodash from "lodash";
/// import * as zod from "zod";
///
/// export default async function(input) {
///   return lodash.capitalize(zod.string().parse(input));
/// }
/// ```
fn assemble_module(
    body: &str,
    import_map: &HashMap<String, String>,
    params: &str,
) -> String {
    let mut module = String::new();
    for key in import_map.keys() {
        module.push_str(&format!("import * as {key} from \"{key}\";\n"));
    }
    module.push_str(&format!(
        "\nexport default async function({params}) {{\n{body}\n}}\n"
    ));
    module
}

/// Extract the `default` export from a module namespace object.
fn extract_default_export(
    rt: &mut deno_core::JsRuntime,
    namespace: v8::Global<v8::Object>,
) -> v8::Global<v8::Value> {
    deno_core::scope!(scope, rt);
    let ns_obj = v8::Local::new(scope, namespace);
    let default_key = v8::String::new(scope, "default").unwrap();
    let default_val = ns_obj
        .get(scope, default_key.into())
        .unwrap_or_else(|| pgrx::error!("pg_typescript: module has no default export"));
    if default_val.is_undefined() || default_val.is_null() {
        pgrx::error!("pg_typescript: default export is undefined");
    }
    v8::Global::new(scope, default_val)
}

/// Call a V8 function with args serialized directly from Rust values.
fn call_fn_with_args<A: serde::Serialize>(
    rt: &mut deno_core::JsRuntime,
    fn_global: v8::Global<v8::Value>,
    args: &[A],
) -> v8::Global<v8::Value> {
    deno_core::scope!(scope, rt);
    let fn_local = v8::Local::new(scope, fn_global);
    let fn_obj = v8::Local::<v8::Function>::try_from(fn_local)
        .unwrap_or_else(|_| pgrx::error!("pg_typescript: default export is not a function"));

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
// Import map helpers
// ---------------------------------------------------------------------------

/// Read the `typescript.import_map` value from a function's proconfig and
/// parse it into a specifier → URL map.
fn read_import_map(proc: &PgProc) -> HashMap<String, String> {
    let json = proc
        .proconfig()
        .unwrap_or_default()
        .into_iter()
        .find_map(|kv| kv.strip_prefix("typescript.import_map=").map(|v| v.to_string()));

    match json {
        Some(ref j) => fetch::parse_import_map(j)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}")),
        None => HashMap::new(),
    }
}

/// Read the `typescript.import_map` GUC (set via `SET LOCAL typescript.import_map = '...'`)
/// and parse it for use by DO blocks.
fn read_inline_import_map() -> HashMap<String, String> {
    let json = crate::IMPORT_MAP_GUC
        .get()
        .and_then(|cstr| cstr.to_str().ok().map(|s| s.to_string()));
    match json.as_deref() {
        Some(j) if !j.is_empty() => fetch::parse_import_map(j)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}")),
        _ => HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit_tests {
    use crate::fetch::ModuleStore;
    use serde_json::{Value, json};

    struct JsonSeed;

    impl<'de> serde::de::DeserializeSeed<'de> for JsonSeed {
        type Value = (Value, bool);

        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            use serde::de::Deserialize;
            let v = Value::deserialize(deserializer)?;
            let is_null = v.is_null();
            Ok((v, is_null))
        }
    }

    fn run_impl(
        source: &str,
        params: &[&str],
        args: &[Value],
        import_map: std::collections::HashMap<String, String>,
        store: crate::fetch::HashMapModuleStore,
    ) -> (Value, bool) {
        let param_names: Vec<String> = params.iter().map(|s| s.to_string()).collect();
        let fn_oid = pgrx::pg_sys::Oid::from(0u32);
        super::execute_typescript_fn(fn_oid, source, &import_map, &param_names, args, JsonSeed, store)
    }

    /// Run a function body with no import map (pure JS, no packages).
    fn run(source: &str, params: &[&str], args: &[Value]) -> (Value, bool) {
        run_impl(source, params, args, Default::default(), crate::fetch::HashMapModuleStore::new())
    }

    macro_rules! ts_test {
        // Plain: no imports, pure JS.
        ($name:ident, $src:expr, [$($p:literal),*], [$($a:expr),*], $expected:expr) => {
            #[test]
            fn $name() {
                let args: Vec<Value> = vec![$($a),*];
                let (val, is_null) = run($src, &[$($p),*], &args);
                let expected: Option<Value> = $expected;
                match expected {
                    Some(exp) => assert_eq!(val, exp),
                    None => assert!(is_null),
                }
            }
        };
        // With modules: pre-populate the store and import map.
        // Syntax:  imports { "key" => "url", ... }  modules { "url" => "source", ... }
        ($name:ident, $src:expr, [$($p:literal),*], [$($a:expr),*], $expected:expr,
         imports { $($spec:literal => $url:literal),* $(,)? },
         modules { $($murl:literal => $msrc:literal),* $(,)? }) => {
            #[test]
            fn $name() {
                let args: Vec<Value> = vec![$($a),*];
                let import_map = crate::fetch::make_import_map(&[$(($spec, $url)),*]);
                let mut store = crate::fetch::HashMapModuleStore::new();
                $( store.write(0, $murl, $msrc); )*
                let (val, is_null) = run_impl($src, &[$($p),*], &args, import_map, store);
                let expected: Option<Value> = $expected;
                match expected {
                    Some(exp) => assert_eq!(val, exp),
                    None => assert!(is_null),
                }
            }
        };
    }

    // --- module load error on syntax errors ---------------------------------

    #[test]
    fn syntax_error_detected_at_module_load() {
        use crate::runtime::{block_on, with_runtime};
        let module_source = super::assemble_module("const x = ;", &Default::default(), "");
        let hash = super::hash_str(&module_source);
        let specifier = deno_core::resolve_url(
            &format!("file:///pg_typescript/test_syntax_{hash:016x}.mjs"),
        )
        .unwrap();
        let result = with_runtime(|rt| {
            let _ctx = crate::loader::set_loader_context(
                0,
                Default::default(),
                Box::new(crate::fetch::HashMapModuleStore::new()),
            );
            block_on(rt.load_side_es_module_from_code(&specifier, module_source))
        });
        assert!(result.is_err(), "expected syntax error to cause module load failure");
    }

    // --- assemble_module ----------------------------------------------------

    #[test]
    fn assemble_no_imports_no_params() {
        let out = super::assemble_module("return 42;", &Default::default(), "");
        assert_eq!(out, "\nexport default async function() {\nreturn 42;\n}\n");
    }

    #[test]
    fn assemble_with_params() {
        let out = super::assemble_module("return a + b;", &Default::default(), "a, b");
        assert_eq!(out, "\nexport default async function(a, b) {\nreturn a + b;\n}\n");
    }

    #[test]
    fn assemble_with_import() {
        let map = crate::fetch::make_import_map(&[("math", "https://esm.sh/math@1")]);
        let out = super::assemble_module("return math.add(1, 2);", &map, "");
        assert!(out.contains("import * as math from \"math\";"));
        assert!(out.contains("export default async function()"));
        assert!(out.contains("return math.add(1, 2);"));
    }

    // --- execute_inline_block -----------------------------------------------

    #[test]
    fn inline_block_runs() {
        super::execute_inline_block(
            "const x = 1 + 1;",
            &Default::default(),
            crate::fetch::HashMapModuleStore::new(),
        );
    }

    #[test]
    fn inline_block_with_module() {
        let import_map = crate::fetch::make_import_map(&[("math", "https://esm.sh/math@1")]);
        let mut store = crate::fetch::HashMapModuleStore::new();
        store.write(0, "https://esm.sh/math@1", "export function add(a, b) { return a + b; }");
        super::execute_inline_block("const result = math.add(1, 2);", &import_map, store);
    }

    // --- sync / async execution ---------------------------------------------

    ts_test!(sync_add, "return a + b;", ["a", "b"], [json!(1), json!(2)], Some(json!(3)));
    ts_test!(sync_string_template, "return `Hello, ${name}!`;", ["name"], [json!("world")], Some(json!("Hello, world!")));
    ts_test!(sync_bool_comparison, "return a > b;", ["a", "b"], [json!(3.0), json!(1.5)], Some(json!(true)));
    ts_test!(sync_null_return, "return null;", [], [], None);
    ts_test!(sync_object_return, "return { x: n * 2 };", ["n"], [json!(21)], Some(json!({ "x": 42 })));
    ts_test!(async_number, "return await Promise.resolve(n * 2);", ["n"], [json!(21)], Some(json!(42)));
    ts_test!(
        async_string,
        "const greeting = await Promise.resolve(`Hello, ${name}!`);
         return greeting;",
        ["name"],
        [json!("world")],
        Some(json!("Hello, world!"))
    );
    ts_test!(
        async_chained_awaits,
        "const a = await Promise.resolve(x + 1);
         const b = await Promise.resolve(a * 2);
         return b;",
        ["x"],
        [json!(4)],
        Some(json!(10))
    );
    ts_test!(async_null_return, "return await Promise.resolve(null);", [], [], None);
    ts_test!(
        async_object,
        "const doubled = await Promise.resolve(n * 2);
         return { original: n, doubled };",
        ["n"],
        [json!(7)],
        Some(json!({ "original": 7, "doubled": 14 }))
    );

    // --- module loading -----------------------------------------------------

    ts_test!(
        module_named_export,
        "return math.add(a, b);",
        ["a", "b"], [json!(3), json!(4)], Some(json!(7)),
        imports { "math" => "https://esm.sh/math@1" },
        modules { "https://esm.sh/math@1" => "export function add(a, b) { return a + b; }" }
    );
    ts_test!(
        module_two_imports,
        "return fmt.greet(str.shout(name));",
        ["name"], [json!("world")], Some(json!("Hello, WORLD!")),
        imports {
            "str" => "https://esm.sh/str@1",
            "fmt" => "https://esm.sh/fmt@1",
        },
        modules {
            "https://esm.sh/str@1" => "export function shout(s) { return s.toUpperCase(); }",
            "https://esm.sh/fmt@1" => "export function greet(s) { return `Hello, ${s}!`; }",
        }
    );
    ts_test!(
        module_async_usage,
        "return await Promise.resolve(math.multiply(a, b));",
        ["a", "b"], [json!(6), json!(7)], Some(json!(42)),
        imports { "math" => "https://esm.sh/math@1" },
        modules { "https://esm.sh/math@1" => "export function multiply(a, b) { return a * b; }" }
    );
    ts_test!(
        module_transitive_dep,
        "return utils.double(n);",
        ["n"], [json!(21)], Some(json!(42)),
        imports { "utils" => "https://esm.sh/utils@1" },
        modules {
            // utils re-exports from math, which is a transitive dep
            "https://esm.sh/utils@1" =>
                "import { multiply } from './math.js';
                 export function double(n) { return multiply(n, 2); }",
            "https://esm.sh/math.js" =>
                "export function multiply(a, b) { return a * b; }",
        }
    );
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;
    use serde_json::json;

    // --- basic type round-trips ---------------------------------------------

    #[pg_test]
    fn test_simple_add() {
        Spi::run(
            "CREATE FUNCTION test_add(a int, b int) RETURNS int \
             LANGUAGE typescript AS $$ return a + b; $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>("SELECT test_add(1, 2)").unwrap().unwrap();
        assert_eq!(result, 3);
    }

    #[pg_test]
    fn test_string_function() {
        Spi::run(
            "CREATE FUNCTION test_greet(name text) RETURNS text \
             LANGUAGE typescript AS $$ return `Hello, ${name}!`; $$",
        )
        .unwrap();
        let result = Spi::get_one::<String>("SELECT test_greet('world')").unwrap().unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[pg_test]
    fn test_bool_return() {
        Spi::run(
            "CREATE FUNCTION test_gt(a float8, b float8) RETURNS bool \
             LANGUAGE typescript AS $$ return a > b; $$",
        )
        .unwrap();
        assert!(Spi::get_one::<bool>("SELECT test_gt(3.0, 1.5)").unwrap().unwrap());
        assert!(!Spi::get_one::<bool>("SELECT test_gt(1.5, 3.0)").unwrap().unwrap());
    }

    #[pg_test]
    fn test_float8_arithmetic() {
        Spi::run(
            "CREATE FUNCTION test_div(a float8, b float8) RETURNS float8 \
             LANGUAGE typescript AS $$ return a / b; $$",
        )
        .unwrap();
        let result = Spi::get_one::<f64>("SELECT test_div(1.0, 4.0)").unwrap().unwrap();
        assert!((result - 0.25).abs() < 1e-10);
    }

    // --- NULL handling ------------------------------------------------------

    #[pg_test]
    fn test_null_return() {
        Spi::run(
            "CREATE FUNCTION test_null_ret() RETURNS int \
             LANGUAGE typescript AS $$ return null; $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>("SELECT test_null_ret()").unwrap();
        assert!(result.is_none(), "expected SQL NULL");
    }

    #[pg_test]
    fn test_null_arg() {
        // Without STRICT, Postgres calls the function even when the arg is NULL.
        // JS receives null/undefined; nullish-coalescing returns the fallback.
        Spi::run(
            "CREATE FUNCTION test_null_arg(x int) RETURNS int \
             LANGUAGE typescript AS $$ return x ?? -1; $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>("SELECT test_null_arg(NULL)").unwrap().unwrap();
        assert_eq!(result, -1);
        // Non-null arg should pass through normally.
        let result = Spi::get_one::<i32>("SELECT test_null_arg(42)").unwrap().unwrap();
        assert_eq!(result, 42);
    }

    // --- async / await ------------------------------------------------------

    #[pg_test]
    fn test_async_await() {
        Spi::run(
            "CREATE FUNCTION test_async_double(n int) RETURNS int \
             LANGUAGE typescript AS $$ return await Promise.resolve(n * 2); $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>("SELECT test_async_double(21)").unwrap().unwrap();
        assert_eq!(result, 42);
    }

    // --- JSONB round-trips --------------------------------------------------

    #[pg_test]
    fn test_jsonb_return() {
        Spi::run(
            "CREATE FUNCTION test_jsonb_ret(n int) RETURNS jsonb \
             LANGUAGE typescript AS $$ return { value: n, doubled: n * 2 }; $$",
        )
        .unwrap();
        let result = Spi::get_one::<pgrx::JsonB>("SELECT test_jsonb_ret(21)").unwrap().unwrap();
        assert_eq!(result.0["value"], json!(21));
        assert_eq!(result.0["doubled"], json!(42));
    }

    #[pg_test]
    fn test_jsonb_arg() {
        Spi::run(
            "CREATE FUNCTION test_jsonb_sum(data jsonb) RETURNS int \
             LANGUAGE typescript AS $$ return data.x + data.y; $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>(
            r#"SELECT test_jsonb_sum('{"x": 10, "y": 32}'::jsonb)"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result, 42);
    }

    // --- DO blocks ----------------------------------------------------------

    #[pg_test]
    fn test_do_block() {
        // Verify the inline handler runs JS and that errors surface correctly.
        Spi::run(
            "DO $$ \
               const x = 40 + 2; \
               if (x !== 42) throw new Error(`expected 42, got ${x}`); \
             $$ LANGUAGE typescript",
        )
        .unwrap();
    }

    #[pg_test]
    fn test_do_block_async() {
        Spi::run(
            "DO $$ \
               const result = await Promise.resolve(21 * 2); \
               if (result !== 42) throw new Error(`expected 42, got ${result}`); \
             $$ LANGUAGE typescript",
        )
        .unwrap();
    }

    // --- validator ----------------------------------------------------------

    #[pg_test]
    fn test_validator_rejects_syntax_error() {
        // Body-level syntax error: caught because the validator loads the source
        // as an ES module, which V8 always eagerly parses (no lazy parsing).
        // pgrx::error! bypasses Spi::run's Result so we use PgTryBuilder to catch it.
        let caught = PgTryBuilder::new(|| {
            let _ = Spi::run(
                "CREATE FUNCTION bad_fn() RETURNS void \
                 LANGUAGE typescript AS $$ const x = ; $$",
            );
            false
        })
        .catch_others(|_| true)
        .execute();
        assert!(caught, "expected CREATE FUNCTION to fail on syntax error");
    }

    #[pg_test]
    fn test_validator_accepts_valid_function() {
        // Validator should pass and the function should be callable.
        Spi::run(
            "CREATE FUNCTION test_identity(x int) RETURNS int \
             LANGUAGE typescript AS $$ return x; $$",
        )
        .unwrap();
        let result = Spi::get_one::<i32>("SELECT test_identity(99)").unwrap().unwrap();
        assert_eq!(result, 99);
    }
}
