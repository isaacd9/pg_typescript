use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Once;

use deno_core::{op2, JsRuntime};
use deno_runtime::deno_permissions::{
    Permissions, PermissionsContainer, PermissionsOptions, RuntimePermissionDescriptorParser,
};
use deno_runtime::deno_web::{BlobStore, InMemoryBroadcastChannel};
use deno_runtime::worker::{MainWorker, WorkerOptions, WorkerServiceOptions};
use deno_runtime::FeatureChecker;
use node_resolver::errors::PackageFolderResolveError;
use node_resolver::{InNpmPackageChecker, NpmPackageFolderResolver, UrlOrPathRef};

use crate::loader::PgModuleLoader;

const STARTUP_SNAPSHOT: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pg_typescript_runtime.snap"));

thread_local! {
    static JS_RT: RefCell<Option<MainWorker>> = RefCell::new(None);
    static TOKIO_RT: RefCell<Option<tokio::runtime::Runtime>> = RefCell::new(None);
}

static RUSTLS_PROVIDER_INIT: Once = Once::new();

#[derive(Clone, Debug, Default)]
pub struct RuntimePermissions {
    pub allow_read: Option<Vec<String>>,
    pub allow_write: Option<Vec<String>>,
    pub allow_net: Option<Vec<String>>,
    pub allow_env: Option<Vec<String>>,
    pub allow_run: Option<Vec<String>>,
    pub allow_ffi: Option<Vec<String>>,
    pub allow_sys: Option<Vec<String>>,
    pub allow_import: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
struct PgInNpmPackageChecker;

impl InNpmPackageChecker for PgInNpmPackageChecker {
    fn in_npm_package(&self, _specifier: &deno_core::url::Url) -> bool {
        false
    }
}

#[derive(Clone, Debug, Default)]
struct PgNpmPackageFolderResolver;

impl NpmPackageFolderResolver for PgNpmPackageFolderResolver {
    fn resolve_package_folder_from_package(
        &self,
        _specifier: &str,
        _referrer: &UrlOrPathRef,
    ) -> Result<PathBuf, PackageFolderResolveError> {
        Ok(PathBuf::from("/__pg_typescript_no_npm__"))
    }

    fn resolve_types_package_folder(
        &self,
        _types_package_name: &str,
        _maybe_package_version: Option<&deno_semver::Version>,
        _maybe_referrer: Option<&UrlOrPathRef>,
    ) -> Option<PathBuf> {
        None
    }
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

deno_core::extension!(
    pg_typescript_console,
    ops = [op_pg_console_log],
    esm_entry_point = "ext:pg_typescript_console/console_bridge.js",
    esm = [ dir "src/js", "console_bridge.js" ],
);
deno_core::extension!(
    pg_typescript_runtime_state,
    state = |state| {
        if !state.has::<deno_runtime::ops::bootstrap::SnapshotOptions>() {
            state.put(deno_runtime::ops::bootstrap::SnapshotOptions::default());
        }
    }
);

const CONSOLE_HOOK_JS: &str = r#"
(() => {
  const op = globalThis?.Deno?.core?.ops?.op_pg_console_log
    ?? globalThis?.__pg_op_console_log;
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

fn install_console_hook(rt: &mut JsRuntime) {
    rt.execute_script("pg_typescript:console_hook", CONSOLE_HOOK_JS)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to install console hook: {e}"));
}

/// Re-apply the console hook in case runtime bootstrap code replaced console methods.
pub fn ensure_console_hook(rt: &mut JsRuntime) {
    install_console_hook(rt);
}

/// Run `f` with the per-connection runtime, initialising it on first use.
pub fn with_runtime<F, R>(f: F) -> R
where
    F: FnOnce(&mut JsRuntime) -> R,
{
    JS_RT.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(create_runtime());
        }
        let worker = borrow.as_mut().unwrap();
        f(&mut worker.js_runtime)
    })
}

/// Eagerly initialize the per-connection runtime.
///
/// This is used by `_PG_init` when `typescript.prewarm_runtime` is enabled so
/// first function execution does not pay runtime bootstrap latency.
pub fn prewarm_runtime() {
    with_runtime(|_| ());
}

/// Apply effective permissions to the runtime before module load/evaluation.
pub fn set_runtime_permissions(rt: &mut JsRuntime, permissions: &RuntimePermissions) {
    let container = build_permissions_container(permissions);
    rt.op_state()
        .borrow_mut()
        .put::<PermissionsContainer>(container);
}

fn ensure_tokio_rt() {
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
}

/// Block the current thread on an async future, using a per-connection
/// single-threaded Tokio runtime.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    ensure_tokio_rt();
    TOKIO_RT.with(|cell| cell.borrow().as_ref().unwrap().block_on(future))
}

/// Enter the per-connection Tokio runtime context while executing `f`.
///
/// This is needed for synchronous JS entrypoints that may invoke ops which
/// call `tokio::spawn` before we later `block_on` the returned promise.
pub fn with_tokio_context<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    ensure_tokio_rt();
    TOKIO_RT.with(|cell| {
        let borrow = cell.borrow();
        let _guard = borrow.as_ref().unwrap().enter();
        f()
    })
}

fn build_permissions_container(permissions: &RuntimePermissions) -> PermissionsContainer {
    let parser = RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
    let options = PermissionsOptions {
        allow_env: permissions.allow_env.clone(),
        allow_net: permissions.allow_net.clone(),
        allow_ffi: permissions.allow_ffi.clone(),
        allow_read: permissions.allow_read.clone(),
        allow_run: permissions.allow_run.clone(),
        allow_sys: permissions.allow_sys.clone(),
        allow_write: permissions.allow_write.clone(),
        allow_import: permissions.allow_import.clone(),
        prompt: false,
        ..Default::default()
    };

    let perms = Permissions::from_options(&parser, &options)
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: invalid permissions config: {e}"));

    PermissionsContainer::new(Arc::new(parser), perms)
}

fn create_runtime() -> MainWorker {
    RUSTLS_PROVIDER_INIT.call_once(|| {
        // rustls 0.23 requires a process-level crypto provider before TLS use.
        let _ =
            deno_runtime::deno_tls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    });

    let permissions = build_permissions_container(&RuntimePermissions::default());
    let main_module = deno_core::resolve_url("file:///pg_typescript/runtime_bootstrap.mjs")
        .expect("pg_typescript: invalid runtime bootstrap specifier");

    let services: WorkerServiceOptions<
        PgInNpmPackageChecker,
        PgNpmPackageFolderResolver,
        sys_traits::impls::RealSys,
    > = WorkerServiceOptions {
        blob_store: Arc::new(BlobStore::default()),
        broadcast_channel: InMemoryBroadcastChannel::default(),
        deno_rt_native_addon_loader: None,
        feature_checker: Arc::new(FeatureChecker::default()),
        fs: Arc::new(deno_runtime::deno_fs::RealFs),
        module_loader: Rc::new(PgModuleLoader),
        node_services: None,
        npm_process_state_provider: None,
        permissions,
        root_cert_store_provider: Default::default(),
        fetch_dns_resolver: deno_runtime::deno_fetch::dns::Resolver::default(),
        shared_array_buffer_store: Default::default(),
        compiled_wasm_module_store: Default::default(),
        v8_code_cache: Default::default(),
        bundle_provider: None,
    };

    let mut worker = MainWorker::bootstrap_from_options(
        &main_module,
        services,
        WorkerOptions {
            startup_snapshot: Some(STARTUP_SNAPSHOT),
            extensions: vec![
                pg_typescript_runtime_state::init(),
                pg_typescript_console::init(),
            ],
            ..Default::default()
        },
    );

    install_console_hook(&mut worker.js_runtime);

    worker
}
