use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::hash::{Hash, Hasher};

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::SourceMapOption;
use deno_core::v8;
use pgrx::pg_catalog::pg_proc::PgProc;
use pgrx::prelude::*;
use pgrx::{fcinfo, pg_sys};

use crate::convert::{PgDatum, PgDatumSeed};
use crate::fetch;
use crate::loader;
use crate::runtime::{block_on, set_runtime_permissions, with_runtime, RuntimePermissions};

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
    let permissions = read_function_permissions(&proc);

    let args: Vec<PgDatum> = (0..nargs)
        .map(|i| unsafe {
            let nd = fcinfo::pg_get_nullable_datum(fcinfo, i);
            PgDatum {
                datum: nd.value,
                isnull: nd.isnull,
                oid: arg_types[i],
            }
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
        &permissions,
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
    let fn_oid: pg_sys::Oid =
        unsafe { pg_sys::Oid::from(fcinfo::pg_get_nullable_datum(fcinfo, 0).value.value() as u32) };

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
    let permissions = read_function_permissions(&proc);

    // If the function declares an import map, fetch all dependencies now.
    #[cfg(not(test))]
    if !import_map.is_empty() {
        fetch::fetch_and_cache(
            u32::from(fn_oid),
            &import_map,
            &mut fetch::PgModuleStore,
            &fetch::UreqFetcher,
        );
    }

    // Assemble and transpile the same module source the call handler will use so that the
    // specifier and hash match and we can pre-warm FN_CACHE.
    let module_source = assemble_module(&source, &import_map, &params);
    let module_source = transpile_module(&module_source);
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
        set_runtime_permissions(rt, &permissions);
        let _ctx = loader::set_loader_context(oid_raw, import_map.clone(), Box::new(store));

        let module_id = block_on(rt.load_side_es_module_from_code(&specifier, module_source))
            .unwrap_or_else(|e| {
                // Only report the first line — the rest is a V8 stack trace that
                // contains an unstable OID/hash (e.g. "at file:///pg_typescript/fn_NNNN_...").
                let msg = e.to_string();
                let first_line = msg.lines().next().unwrap_or(&msg);
                pgrx::error!("pg_typescript: syntax error: {first_line}");
            });

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
        let permissions = read_inline_permissions();

        // Fetch and cache all dependencies before execution — network access
        // happens here, never during the execute step.
        #[cfg(not(test))]
        if !import_map.is_empty() {
            fetch::fetch_and_cache(
                0u32,
                &import_map,
                &mut fetch::PgModuleStore,
                &fetch::UreqFetcher,
            );
        }

        #[cfg(not(test))]
        let store = fetch::PgModuleStore;
        #[cfg(test)]
        let store = fetch::HashMapModuleStore::new();

        execute_inline_block(&source, &import_map, &permissions, store);
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
    permissions: &RuntimePermissions,
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
    let module_source = transpile_module(&module_source);
    let source_hash = hash_str(&module_source);
    let oid_raw = u32::from(fn_oid);

    with_runtime(|rt| {
        set_runtime_permissions(rt, permissions);
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
                block_on(rt.with_event_loop_promise(evaluate, Default::default())).unwrap_or_else(
                    |e| pgrx::error!("pg_typescript: module evaluation failed: {e}"),
                );

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
    permissions: &RuntimePermissions,
    store: MS,
) {
    let module_source = assemble_module(source, import_map, "");
    let module_source = transpile_module(&module_source);
    let source_hash = hash_str(&module_source);

    with_runtime(|rt| {
        set_runtime_permissions(rt, permissions);
        let fn_global = FN_CACHE.with(|c| c.borrow().get(&(0u32, source_hash)).cloned());

        let fn_global = match fn_global {
            Some(f) => f,
            None => {
                // Set loader context only on cache miss.
                let _ctx = loader::set_loader_context(0u32, import_map.clone(), Box::new(store));

                let specifier_str = format!("file:///pg_typescript/do_{source_hash:016x}.mjs");
                let specifier = deno_core::resolve_url(&specifier_str)
                    .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid specifier: {e}"));

                let module_id =
                    block_on(rt.load_side_es_module_from_code(&specifier, module_source))
                        .unwrap_or_else(|e| pgrx::error!("pg_typescript: module load error: {e}"));

                let evaluate = rt.mod_evaluate(module_id);
                block_on(rt.with_event_loop_promise(evaluate, Default::default())).unwrap_or_else(
                    |e| pgrx::error!("pg_typescript: module evaluation failed: {e}"),
                );

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
fn assemble_module(body: &str, import_map: &HashMap<String, String>, params: &str) -> String {
    let mut module = String::new();
    for key in import_map.keys() {
        module.push_str(&format!("import * as {key} from \"{key}\";\n"));
    }
    module.push_str(&format!(
        "\nexport default async function({params}) {{\n{body}\n}}\n"
    ));
    module
}

/// Transpile TypeScript/TSX syntax to JavaScript. This does not type-check.
fn transpile_module(source: &str) -> String {
    let specifier = deno_core::resolve_url("file:///pg_typescript/input.ts")
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid transpile specifier: {e}"));

    let parsed = deno_ast::parse_module(ParseParams {
        specifier,
        text: source.into(),
        media_type: MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .unwrap_or_else(|e| {
        let msg = e.to_string();
        let first_line = msg.lines().next().unwrap_or(&msg);
        pgrx::error!("pg_typescript: syntax error: {first_line}");
    });

    parsed
        .transpile(
            &deno_ast::TranspileOptions {
                imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
                decorators: deno_ast::DecoratorsTranspileOption::Ecma,
                ..Default::default()
            },
            &deno_ast::TranspileModuleOptions {
                module_kind: Some(deno_ast::ModuleKind::Esm),
            },
            &deno_ast::EmitOptions {
                source_map: SourceMapOption::None,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| {
            let msg = e.to_string();
            let first_line = msg.lines().next().unwrap_or(&msg);
            pgrx::error!("pg_typescript: syntax error: {first_line}");
        })
        .into_source()
        .text
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
fn global_to<S, R>(rt: &mut deno_core::JsRuntime, global: v8::Global<v8::Value>, seed: S) -> R
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
    let json = read_function_config(proc, "typescript.import_map");

    match json {
        Some(ref j) => {
            fetch::parse_import_map(j).unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
        }
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
        Some(j) if !j.is_empty() => {
            fetch::parse_import_map(j).unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
        }
        _ => HashMap::new(),
    }
}

#[derive(Clone, Debug)]
enum PermissionValue {
    Deny,
    AllowAll,
    AllowList(Vec<String>),
}

impl Default for PermissionValue {
    fn default() -> Self {
        Self::Deny
    }
}

#[derive(Clone, Debug, Default)]
struct PermissionSpec {
    read: PermissionValue,
    write: PermissionValue,
    net: PermissionValue,
    env: PermissionValue,
    run: PermissionValue,
    ffi: PermissionValue,
    sys: PermissionValue,
    import: PermissionValue,
}

fn read_function_config(proc: &PgProc, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    proc.proconfig().unwrap_or_default().into_iter().find_map(|kv| {
        kv.strip_prefix(&prefix).map(|v| v.to_string())
    })
}

fn read_function_permissions(proc: &PgProc) -> RuntimePermissions {
    let requested = PermissionSpec {
        read: parse_permission_setting(
            read_function_config(proc, "typescript.allow_read"),
            "function setting typescript.allow_read",
        ),
        write: parse_permission_setting(
            read_function_config(proc, "typescript.allow_write"),
            "function setting typescript.allow_write",
        ),
        net: parse_permission_setting(
            read_function_config(proc, "typescript.allow_net"),
            "function setting typescript.allow_net",
        ),
        env: parse_permission_setting(
            read_function_config(proc, "typescript.allow_env"),
            "function setting typescript.allow_env",
        ),
        run: parse_permission_setting(
            read_function_config(proc, "typescript.allow_run"),
            "function setting typescript.allow_run",
        ),
        ffi: parse_permission_setting(
            read_function_config(proc, "typescript.allow_ffi"),
            "function setting typescript.allow_ffi",
        ),
        sys: parse_permission_setting(
            read_function_config(proc, "typescript.allow_sys"),
            "function setting typescript.allow_sys",
        ),
        import: parse_permission_setting(
            read_function_config(proc, "typescript.allow_import"),
            "function setting typescript.allow_import",
        ),
    };

    effective_permissions(requested, read_max_permissions())
}

fn read_inline_permissions() -> RuntimePermissions {
    let requested = PermissionSpec {
        read: parse_permission_setting(
            guc_value(crate::ALLOW_READ_GUC.get()),
            "GUC typescript.allow_read",
        ),
        write: parse_permission_setting(
            guc_value(crate::ALLOW_WRITE_GUC.get()),
            "GUC typescript.allow_write",
        ),
        net: parse_permission_setting(
            guc_value(crate::ALLOW_NET_GUC.get()),
            "GUC typescript.allow_net",
        ),
        env: parse_permission_setting(
            guc_value(crate::ALLOW_ENV_GUC.get()),
            "GUC typescript.allow_env",
        ),
        run: parse_permission_setting(
            guc_value(crate::ALLOW_RUN_GUC.get()),
            "GUC typescript.allow_run",
        ),
        ffi: parse_permission_setting(
            guc_value(crate::ALLOW_FFI_GUC.get()),
            "GUC typescript.allow_ffi",
        ),
        sys: parse_permission_setting(
            guc_value(crate::ALLOW_SYS_GUC.get()),
            "GUC typescript.allow_sys",
        ),
        import: parse_permission_setting(
            guc_value(crate::ALLOW_IMPORT_GUC.get()),
            "GUC typescript.allow_import",
        ),
    };

    effective_permissions(requested, read_max_permissions())
}

fn read_max_permissions() -> PermissionSpec {
    PermissionSpec {
        read: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_READ_GUC.get()),
            "GUC typescript.max_allow_read",
        ),
        write: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_WRITE_GUC.get()),
            "GUC typescript.max_allow_write",
        ),
        net: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_NET_GUC.get()),
            "GUC typescript.max_allow_net",
        ),
        env: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_ENV_GUC.get()),
            "GUC typescript.max_allow_env",
        ),
        run: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_RUN_GUC.get()),
            "GUC typescript.max_allow_run",
        ),
        ffi: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_FFI_GUC.get()),
            "GUC typescript.max_allow_ffi",
        ),
        sys: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_SYS_GUC.get()),
            "GUC typescript.max_allow_sys",
        ),
        import: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_IMPORT_GUC.get()),
            "GUC typescript.max_allow_import",
        ),
    }
}

fn guc_value(value: Option<std::ffi::CString>) -> Option<String> {
    value
        .and_then(|cstr| cstr.to_str().ok().map(|s| s.to_string()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_permission_setting(raw: Option<String>, source: &str) -> PermissionValue {
    let Some(value) = raw else {
        return PermissionValue::Deny;
    };

    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "off" | "none" | "deny" | "false" | "0" => PermissionValue::Deny,
        "*" | "all" | "on" | "true" | "1" => PermissionValue::AllowAll,
        _ => PermissionValue::AllowList(parse_permission_list(&value, source)),
    }
}

fn parse_permission_list(value: &str, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for raw in value.split(',') {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        if seen.insert(item.to_string()) {
            out.push(item.to_string());
        }
    }

    if out.is_empty() {
        pgrx::error!("pg_typescript: invalid empty permission list in {source}");
    }

    out
}

fn effective_permissions(requested: PermissionSpec, max: PermissionSpec) -> RuntimePermissions {
    RuntimePermissions {
        allow_read: to_runtime_allowlist(intersect_permission(requested.read, max.read)),
        allow_write: to_runtime_allowlist(intersect_permission(requested.write, max.write)),
        allow_net: to_runtime_allowlist(intersect_permission(requested.net, max.net)),
        allow_env: to_runtime_allowlist(intersect_permission(requested.env, max.env)),
        allow_run: to_runtime_allowlist(intersect_permission(requested.run, max.run)),
        allow_ffi: to_runtime_allowlist(intersect_permission(requested.ffi, max.ffi)),
        allow_sys: to_runtime_allowlist(intersect_permission(requested.sys, max.sys)),
        allow_import: to_runtime_allowlist(intersect_permission(requested.import, max.import)),
    }
}

fn intersect_permission(requested: PermissionValue, max: PermissionValue) -> PermissionValue {
    match max {
        PermissionValue::Deny => PermissionValue::Deny,
        PermissionValue::AllowAll => requested,
        PermissionValue::AllowList(max_list) => match requested {
            PermissionValue::Deny => PermissionValue::Deny,
            PermissionValue::AllowAll => PermissionValue::AllowList(max_list),
            PermissionValue::AllowList(req_list) => {
                let cap: HashSet<String> = max_list.into_iter().collect();
                let mut out = Vec::new();
                let mut seen = HashSet::new();
                for item in req_list {
                    if cap.contains(&item) && seen.insert(item.clone()) {
                        out.push(item);
                    }
                }
                if out.is_empty() {
                    PermissionValue::Deny
                } else {
                    PermissionValue::AllowList(out)
                }
            }
        },
    }
}

fn to_runtime_allowlist(value: PermissionValue) -> Option<Vec<String>> {
    match value {
        PermissionValue::AllowAll => Some(vec![]),
        PermissionValue::AllowList(values) => Some(values),
        PermissionValue::Deny => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit_tests {
    use crate::fetch::ModuleStore;
    use serde_json::{json, Value};

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
        super::execute_typescript_fn(
            fn_oid,
            source,
            &import_map,
            &super::RuntimePermissions::default(),
            &param_names,
            args,
            JsonSeed,
            store,
        )
    }

    /// Run a function body with no import map (pure JS, no packages).
    fn run(source: &str, params: &[&str], args: &[Value]) -> (Value, bool) {
        run_impl(
            source,
            params,
            args,
            Default::default(),
            crate::fetch::HashMapModuleStore::new(),
        )
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
        let specifier = deno_core::resolve_url(&format!(
            "file:///pg_typescript/test_syntax_{hash:016x}.mjs"
        ))
        .unwrap();
        let result = with_runtime(|rt| {
            let _ctx = crate::loader::set_loader_context(
                0,
                Default::default(),
                Box::new(crate::fetch::HashMapModuleStore::new()),
            );
            block_on(rt.load_side_es_module_from_code(&specifier, module_source))
        });
        assert!(
            result.is_err(),
            "expected syntax error to cause module load failure"
        );
    }

    #[test]
    fn transpile_parse_error_first_line_is_stable() {
        let module_source = super::assemble_module("  const x = ;", &Default::default(), "");
        let err = deno_ast::parse_module(deno_ast::ParseParams {
            specifier: deno_core::resolve_url("file:///pg_typescript/input.ts").unwrap(),
            text: module_source.into(),
            media_type: deno_ast::MediaType::TypeScript,
            capture_tokens: false,
            scope_analysis: false,
            maybe_syntax: None,
        })
        .err()
        .expect("expected parse error");

        let first_line = err
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            first_line,
            "Expression expected at file:///pg_typescript/input.ts:3:13"
        );
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
        assert_eq!(
            out,
            "\nexport default async function(a, b) {\nreturn a + b;\n}\n"
        );
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
            &super::RuntimePermissions::default(),
            crate::fetch::HashMapModuleStore::new(),
        );
    }

    #[test]
    fn inline_block_with_module() {
        let import_map = crate::fetch::make_import_map(&[("math", "https://esm.sh/math@1")]);
        let mut store = crate::fetch::HashMapModuleStore::new();
        store.write(
            0,
            "https://esm.sh/math@1",
            "export function add(a, b) { return a + b; }",
        );
        super::execute_inline_block(
            "const result = math.add(1, 2);",
            &import_map,
            &super::RuntimePermissions::default(),
            store,
        );
    }

    // --- sync / async execution ---------------------------------------------

    ts_test!(
        sync_add,
        "return a + b;",
        ["a", "b"],
        [json!(1), json!(2)],
        Some(json!(3))
    );
    ts_test!(
        sync_typescript_annotations,
        "type NumBox = { value: number };
         const box: NumBox = { value: n + 1 };
         return box.value;",
        ["n"],
        [json!(41)],
        Some(json!(42))
    );
    ts_test!(
        sync_string_template,
        "return `Hello, ${name}!`;",
        ["name"],
        [json!("world")],
        Some(json!("Hello, world!"))
    );
    ts_test!(
        sync_bool_comparison,
        "return a > b;",
        ["a", "b"],
        [json!(3.0), json!(1.5)],
        Some(json!(true))
    );
    ts_test!(sync_null_return, "return null;", [], [], None);
    ts_test!(
        sync_object_return,
        "return { x: n * 2 };",
        ["n"],
        [json!(21)],
        Some(json!({ "x": 42 }))
    );
    ts_test!(
        async_number,
        "return await Promise.resolve(n * 2);",
        ["n"],
        [json!(21)],
        Some(json!(42))
    );
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
    ts_test!(
        async_null_return,
        "return await Promise.resolve(null);",
        [],
        [],
        None
    );
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
