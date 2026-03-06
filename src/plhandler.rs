use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::CStr;
use std::hash::{Hash, Hasher};

use deno_core::v8;
use pgrx::pg_catalog::pg_proc::PgProc;
use pgrx::prelude::*;
use pgrx::{fcinfo, pg_sys};

use crate::convert::{PgDatum, PgDatumSeed};
use crate::fetch;
use crate::guc::{import_url_allowed, GucParser, ImportUrlCap};
use crate::loader;
use crate::permissions::{
    read_function_config, read_function_permissions, read_inline_permissions,
};
use crate::runtime::{
    block_on, ensure_console_hook, set_runtime_permissions, with_runtime, with_tokio_context,
    RuntimePermissions,
};

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
    let nargs = proc.pronargs();
    let param_names = build_param_names(&proc.proargnames(), nargs);
    let (import_map, _max_imports) = read_import_map(&proc);
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

    let artifact = ModuleInputs {
        cache_oid: u32::from(fn_oid),
        source: &source,
        param_names: &param_names,
        import_map: &import_map,
        specifier_prefix: "fn",
    }
    .prepare();
    let (datum, is_null) = artifact.execute(
        ExecutionConfig::new(permissions, make_module_store()),
        &args,
        PgDatumSeed { oid: ret_type },
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
    let param_names = build_param_names(&proc.proargnames(), nargs);
    let (import_map, _max_imports) = read_import_map(&proc);
    let permissions = read_function_permissions(&proc);

    // If the function declares an import map, fetch all dependencies now.
    #[cfg(not(test))]
    if !import_map.is_empty() {
        fetch::fetch_and_cache(
            u32::from(fn_oid),
            &import_map,
            &mut fetch::PgModuleStore,
            &fetch::UreqFetcher,
            &_max_imports,
        )
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));
    }

    // Prepare the same artifact the call handler will execute so the cache key
    // and synthetic specifier match exactly across validation and execution.
    let artifact = ModuleInputs {
        cache_oid: u32::from(fn_oid),
        source: &source,
        param_names: &param_names,
        import_map: &import_map,
        specifier_prefix: "fn",
    }
    .prepare();
    artifact.preload(ExecutionConfig::new(permissions, make_module_store()));

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

        let (import_map, _max_imports) = read_inline_import_map();
        let permissions = read_inline_permissions();

        // Fetch and cache all dependencies before execution — network access
        // happens here, never during the execute step.
        #[cfg(not(test))]
        if !import_map.is_empty() {
            fetch::fetch_and_cache(
                DO_BLOCK_OID,
                &import_map,
                &mut fetch::PgModuleStore,
                &fetch::UreqFetcher,
                &_max_imports,
            )
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));
        }

        let param_names: &[String] = &[];
        let artifact = ModuleInputs {
            cache_oid: DO_BLOCK_OID,
            source: &source,
            param_names,
            import_map: &import_map,
            specifier_prefix: "do",
        }
        .prepare();
        artifact.execute_void(ExecutionConfig::new(permissions, make_module_store()));
        fcinfo::pg_return_void()
    }
}

// ---------------------------------------------------------------------------
// Core execution
// ---------------------------------------------------------------------------

// Synthetic OID used as the FN_CACHE key for DO $$ ... $$ blocks (which have no pg OID).
const DO_BLOCK_OID: u32 = 0;

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

/// Build the JS parameter name list from proargnames, falling back to `_i` for unnamed params.
fn build_param_names(arg_names: &[Option<String>], nargs: usize) -> Vec<String> {
    (0..nargs)
        .map(|i| {
            arg_names
                .get(i)
                .and_then(|opt| opt.as_deref())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("_{i}"))
        })
        .collect()
}

/// Return a module store appropriate for the current build target.
fn make_module_store() -> impl fetch::ModuleStore {
    #[cfg(any(not(test), feature = "pg_test"))]
    {
        fetch::PgModuleStore
    }
    #[cfg(all(test, not(feature = "pg_test")))]
    {
        fetch::HashMapModuleStore::new()
    }
}

struct ExecutionConfig {
    permissions: RuntimePermissions,
    store: Box<dyn fetch::ModuleStore>,
}

impl ExecutionConfig {
    fn new(permissions: RuntimePermissions, store: impl fetch::ModuleStore + 'static) -> Self {
        Self {
            permissions,
            store: Box::new(store),
        }
    }
}

struct ModuleInputs<'a> {
    cache_oid: u32,
    source: &'a str,
    param_names: &'a [String],
    import_map: &'a HashMap<String, String>,
    specifier_prefix: &'static str,
}

impl<'a> ModuleInputs<'a> {
    fn prepare(self) -> ModuleArtifact {
        let params = self.param_names.join(", ");
        let module_source = assemble_module(self.source, self.import_map, &params);
        let source_hash = hash_str(&module_source);
        let specifier = format!(
            "file:///pg_typescript/{}_{source_hash:016x}.ts",
            self.specifier_prefix
        );

        ModuleArtifact {
            cache_oid: self.cache_oid,
            source_hash,
            specifier,
            module_source,
            import_map: self.import_map.clone(),
        }
    }
}

/// Prepared module artifact shared by validation and execution.
///
/// This captures the synthetic entrypoint source, cache key, and loader inputs.
struct ModuleArtifact {
    cache_oid: u32,
    source_hash: u64,
    specifier: String,
    module_source: String,
    import_map: HashMap<String, String>,
}

impl ModuleArtifact {
    fn preload(self, exec: ExecutionConfig) {
        self.with_loaded_fn(exec, |_rt, _fn_global| ());
    }

    fn execute<A, S, R>(self, exec: ExecutionConfig, args: &[A], seed: S) -> R
    where
        A: serde::Serialize,
        S: for<'de> serde::de::DeserializeSeed<'de, Value = R>,
    {
        self.with_loaded_fn(exec, |rt, fn_global| {
            let promise_global = with_tokio_context(|| call_fn_with_args(rt, fn_global, args));
            let resolve_fut = rt.resolve(promise_global);
            let resolved = block_on(rt.with_event_loop_promise(resolve_fut, Default::default()))
                .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));

            global_to(rt, resolved, seed)
        })
    }

    fn execute_void(self, exec: ExecutionConfig) {
        self.with_loaded_fn(exec, |rt, fn_global| {
            let no_args: &[serde_json::Value] = &[];
            let promise_global = call_fn_with_args(rt, fn_global, no_args);
            let resolve_fut = rt.resolve(promise_global);
            block_on(rt.with_event_loop_promise(resolve_fut, Default::default())).unwrap_or_else(
                |e| pgrx::error!("pg_typescript: event loop error in DO block: {e}"),
            );
        });
    }

    fn with_loaded_fn<F, R>(self, exec: ExecutionConfig, f: F) -> R
    where
        F: FnOnce(&mut deno_core::JsRuntime, v8::Global<v8::Value>) -> R,
    {
        let ExecutionConfig { permissions, store } = exec;
        with_runtime(|rt| {
            ensure_console_hook(rt);
            set_runtime_permissions(rt, &permissions);
            let fn_global = self.load_or_get_cached(rt, store);
            f(rt, fn_global)
        })
    }

    fn load_or_get_cached(
        self,
        rt: &mut deno_core::JsRuntime,
        store: Box<dyn fetch::ModuleStore>,
    ) -> v8::Global<v8::Value> {
        if let Some(f) =
            FN_CACHE.with(|c| c.borrow().get(&(self.cache_oid, self.source_hash)).cloned())
        {
            return f;
        }

        self.load_and_cache(rt, store)
    }

    fn load_and_cache(
        self,
        rt: &mut deno_core::JsRuntime,
        store: Box<dyn fetch::ModuleStore>,
    ) -> v8::Global<v8::Value> {
        let Self {
            cache_oid,
            source_hash,
            specifier,
            module_source,
            import_map,
        } = self;

        let specifier_url = deno_core::resolve_url(&specifier)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid specifier: {e}"));
        let mut inline_modules = HashMap::new();
        inline_modules.insert(specifier.clone(), module_source);
        let _ctx =
            loader::set_loader_context_with_inline(cache_oid, import_map, store, inline_modules);

        let module_id = block_on(rt.load_side_es_module(&specifier_url))
            .unwrap_or_else(|e| report_module_load_error(e));

        let evaluate = rt.mod_evaluate(module_id);
        block_on(rt.with_event_loop_promise(evaluate, Default::default()))
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: module evaluation failed: {e}"));

        let namespace = rt
            .get_module_namespace(module_id)
            .unwrap_or_else(|e| pgrx::error!("pg_typescript: get_module_namespace: {e}"));
        let f = extract_default_export(rt, namespace);
        FN_CACHE.with(|c| c.borrow_mut().insert((cache_oid, source_hash), f.clone()));
        f
    }
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
    use std::fmt::Write as _;
    let mut module = String::new();
    for key in import_map.keys() {
        writeln!(module, "import * as {key} from \"{key}\";").unwrap();
    }
    write!(
        module,
        "\nexport default async function({params}) {{\n{body}\n}}\n"
    )
    .unwrap();
    module
}

/// Replace synthetic function specifiers with a stable placeholder path.
fn normalize_syntax_error_line(line: &str) -> String {
    let mut out = line
        .strip_prefix("Uncaught SyntaxError: ")
        .unwrap_or(line)
        .to_string();

    if let Some(start) = out.find("file:///pg_typescript/") {
        if let Some(end_rel) = out[start..].find(".ts:") {
            let end = start + end_rel + 3;
            out.replace_range(start..end, "file:///pg_typescript/input.ts");
        }
    }

    out
}

fn format_module_load_error(msg: &str) -> String {
    let first_line = msg.lines().next().unwrap_or(msg);
    if first_line.starts_with("Uncaught SyntaxError: ")
        || (first_line.contains(" at file:///pg_typescript/") && first_line.contains(".ts:"))
    {
        let normalized = normalize_syntax_error_line(first_line);
        return format!("pg_typescript: syntax error: {normalized}");
    }

    format!("pg_typescript: module load error: {msg}")
}

fn report_module_load_error<E: std::fmt::Display>(error: E) -> ! {
    let msg = error.to_string();
    pgrx::error!("{}", format_module_load_error(&msg));
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

/// Read `typescript.max_imports` from GUC and parse it into an import-URL cap.
fn read_max_imports_cap() -> ImportUrlCap {
    crate::MAX_IMPORTS_GUC
        .parse_setting("GUC typescript.max_imports")
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
}

fn format_import_urls(values: &[String]) -> String {
    format!("[{}]", values.join(","))
}

fn enforce_import_map_cap(
    import_map: &HashMap<String, String>,
    max_imports: &ImportUrlCap,
    requested_source: &str,
) {
    let mut requested: Vec<String> = import_map.values().cloned().collect();
    requested.sort();
    requested.dedup();

    let mut disallowed = Vec::new();
    for url in &requested {
        match import_url_allowed(url, max_imports) {
            Ok(true) => {}
            Ok(false) => disallowed.push(url.clone()),
            Err(e) => pgrx::error!("pg_typescript: {e}"),
        }
    }

    if !disallowed.is_empty() {
        pgrx::error!(
            "pg_typescript: {requested_source} cannot be fulfilled by GUC typescript.max_imports: requested {} includes disallowed values {}",
            format_import_urls(&requested),
            format_import_urls(&disallowed),
        );
    }
}

/// Read the `typescript.import_map` value from a function's proconfig and
/// parse it into a specifier → URL map, enforcing `typescript.max_imports`.
fn read_import_map(proc: &PgProc) -> (HashMap<String, String>, ImportUrlCap) {
    let max_imports = read_max_imports_cap();
    let map = crate::IMPORT_MAP_GUC
        .parse_raw(
            read_function_config(proc, "typescript.import_map"),
            "function setting typescript.import_map",
        )
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));
    enforce_import_map_cap(&map, &max_imports, "function setting typescript.import_map");
    (map, max_imports)
}

/// Read the `typescript.import_map` GUC (set via `SET LOCAL typescript.import_map = '...'`)
/// and parse it for use by DO blocks, enforcing `typescript.max_imports`.
fn read_inline_import_map() -> (HashMap<String, String>, ImportUrlCap) {
    let max_imports = read_max_imports_cap();
    let map = crate::IMPORT_MAP_GUC
        .parse_setting("GUC typescript.import_map")
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"));
    enforce_import_map_cap(&map, &max_imports, "GUC typescript.import_map");
    (map, max_imports)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "pg_test")))]
mod unit_tests {
    use crate::fetch::ModuleStore;
    use pgrx::FromDatum;
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
        permissions: super::RuntimePermissions,
        store: crate::fetch::HashMapModuleStore,
    ) -> (Value, bool) {
        let param_names: Vec<String> = params.iter().map(|s| s.to_string()).collect();
        let fn_oid = pgrx::pg_sys::Oid::from(0u32);
        let artifact = super::ModuleInputs {
            cache_oid: u32::from(fn_oid),
            source,
            param_names: &param_names,
            import_map: &import_map,
            specifier_prefix: "fn",
        }
        .prepare();
        artifact.execute(
            super::ExecutionConfig::new(permissions, store),
            args,
            JsonSeed,
        )
    }

    /// Run a function body with no import map (pure JS, no packages).
    fn run(source: &str, params: &[&str], args: &[Value]) -> (Value, bool) {
        run_impl(
            source,
            params,
            args,
            Default::default(),
            super::RuntimePermissions::default(),
            crate::fetch::HashMapModuleStore::new(),
        )
    }

    /// Run a function body with explicit runtime permissions.
    fn run_with_permissions(
        source: &str,
        params: &[&str],
        args: &[Value],
        permissions: super::RuntimePermissions,
    ) -> (Value, bool) {
        run_impl(
            source,
            params,
            args,
            Default::default(),
            permissions,
            crate::fetch::HashMapModuleStore::new(),
        )
    }

    /// Run a function body and deserialize directly into a SQL return type via OID.
    fn run_with_return_oid(
        source: &str,
        ret_oid: pgrx::pg_sys::Oid,
    ) -> (pgrx::pg_sys::Datum, bool) {
        let fn_oid = pgrx::pg_sys::Oid::from(0u32);
        let params: Vec<String> = vec![];
        let args: Vec<Value> = vec![];
        let import_map = Default::default();
        let artifact = super::ModuleInputs {
            cache_oid: u32::from(fn_oid),
            source,
            param_names: &params,
            import_map: &import_map,
            specifier_prefix: "fn",
        }
        .prepare();
        artifact.execute(
            super::ExecutionConfig::new(
                super::RuntimePermissions::default(),
                crate::fetch::HashMapModuleStore::new(),
            ),
            &args,
            crate::convert::PgDatumSeed { oid: ret_oid },
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
                let (val, is_null) = run_impl(
                    $src,
                    &[$($p),*],
                    &args,
                    import_map,
                    super::RuntimePermissions::default(),
                    store,
                );
                let expected: Option<Value> = $expected;
                match expected {
                    Some(exp) => assert_eq!(val, exp),
                    None => assert!(is_null),
                }
            }
        };
    }

    macro_rules! ts_test_with_permissions {
        ($name:ident, $src:expr, [$($p:literal),*], [$($a:expr),*], $expected:expr, $perms:expr) => {
            #[test]
            fn $name() {
                let args: Vec<Value> = vec![$($a),*];
                let (val, is_null) = run_with_permissions($src, &[$($p),*], &args, $perms);
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
        let specifier =
            deno_core::resolve_url(&format!("file:///pg_typescript/test_syntax_{hash:016x}.ts"))
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
    fn normalize_syntax_error_line_is_stable() {
        let first_line = super::normalize_syntax_error_line(
            "Uncaught SyntaxError: Expression expected at file:///pg_typescript/fn_123_abcdeffedcba1234.ts:4:13"
        );
        assert_eq!(
            first_line,
            "Expression expected at file:///pg_typescript/input.ts:4:13"
        );
    }

    #[test]
    fn format_module_load_error_recognizes_syntax_without_uncaught_prefix() {
        let msg = "Expression expected at file:///pg_typescript/fn_1573d7cbfe76600e.ts:4:13\n\n  const x = ;\n            ~";
        assert_eq!(
            super::format_module_load_error(msg),
            "pg_typescript: syntax error: Expression expected at file:///pg_typescript/input.ts:4:13"
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

    // --- ModuleArtifact::execute_void ---------------------------------------

    #[test]
    fn inline_block_runs() {
        let param_names: &[String] = &[];
        let import_map = Default::default();
        let artifact = super::ModuleInputs {
            cache_oid: super::DO_BLOCK_OID,
            source: "const x = 1 + 1;",
            param_names,
            import_map: &import_map,
            specifier_prefix: "do",
        }
        .prepare();
        artifact.execute_void(super::ExecutionConfig::new(
            super::RuntimePermissions::default(),
            crate::fetch::HashMapModuleStore::new(),
        ));
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
        let param_names: &[String] = &[];
        let artifact = super::ModuleInputs {
            cache_oid: super::DO_BLOCK_OID,
            source: "const result = math.add(1, 2);",
            param_names,
            import_map: &import_map,
            specifier_prefix: "do",
        }
        .prepare();
        artifact.execute_void(super::ExecutionConfig::new(
            super::RuntimePermissions::default(),
            store,
        ));
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

    // --- runtime permissions -----------------------------------------------

    ts_test!(
        permissions_env_denied_by_default,
        "try { Deno.env.get(name); return 'allowed'; } catch { return 'denied'; }",
        ["name"],
        [json!("PATH")],
        Some(json!("denied"))
    );
    ts_test!(
        permissions_net_fetch_example_com_denied_by_default,
        "try { await fetch('https://example.com/'); return 'allowed'; } catch { return 'denied'; }",
        [],
        [],
        Some(json!("denied"))
    );
    ts_test_with_permissions!(
        permissions_net_fetch_example_com_allowed,
        "try {
           const res = await fetch('https://example.com/');
           return res.status > 0;
         } catch (e) {
           // Network resolution/TLS can still fail in CI; ensure this is not
           // a permissions rejection when allow_net includes example.com.
           return !String(e).includes('Requires net access');
         }",
        [],
        [],
        Some(json!(true)),
        super::RuntimePermissions {
            allow_net: Some(vec!["example.com".to_string()]),
            ..Default::default()
        }
    );
    ts_test_with_permissions!(
        permissions_env_allow_all,
        "try { Deno.env.get(name); return 'allowed'; } catch { return 'denied'; }",
        ["name"],
        [json!("PATH")],
        Some(json!("allowed")),
        super::RuntimePermissions {
            allow_env: Some(vec![]),
            ..Default::default()
        }
    );
    ts_test_with_permissions!(
        permissions_env_allow_list_hit,
        "try { Deno.env.get(name); return 'allowed'; } catch { return 'denied'; }",
        ["name"],
        [json!("PATH")],
        Some(json!("allowed")),
        super::RuntimePermissions {
            allow_env: Some(vec!["PATH".to_string()]),
            ..Default::default()
        }
    );
    ts_test_with_permissions!(
        permissions_env_allow_list_miss,
        "try { Deno.env.get(name); return 'allowed'; } catch { return 'denied'; }",
        ["name"],
        [json!("USER")],
        Some(json!("denied")),
        super::RuntimePermissions {
            allow_env: Some(vec!["PATH".to_string()]),
            ..Default::default()
        }
    );

    // --- SQL return type strictness ----------------------------------------

    #[test]
    fn strict_sql_return_int4_ok() {
        let (datum, is_null) = run_with_return_oid("return 42;", pgrx::pg_sys::INT4OID);
        assert!(!is_null);
        let out = unsafe { i32::from_datum(datum, false).expect("int4 datum should decode") };
        assert_eq!(out, 42);
    }

    #[test]
    fn strict_sql_return_bool_ok() {
        let (datum, is_null) = run_with_return_oid("return true;", pgrx::pg_sys::BOOLOID);
        assert!(!is_null);
        let out = unsafe { bool::from_datum(datum, false).expect("bool datum should decode") };
        assert!(out);
    }

    #[test]
    fn strict_sql_return_bool_rejects_number() {
        let err = std::panic::catch_unwind(|| {
            let _ = run_with_return_oid("return 1;", pgrx::pg_sys::BOOLOID);
        });
        assert!(err.is_err(), "expected return type mismatch to panic");
    }

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
