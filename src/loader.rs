use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
    error::ModuleLoaderError,
};
use deno_error::JsErrorBox;

// Only pull in Postgres / pgrx symbols in real (non-unit-test) builds.
// Unit tests link without the Postgres shared library; any reference to
// Postgres C symbols causes a dyld "symbol not found" abort at load time.
#[cfg(not(test))]
use pgrx::prelude::*;
#[cfg(not(test))]
use pgrx::spi::SpiError;
#[cfg(not(test))]
use pgrx::pg_sys;

// ---------------------------------------------------------------------------
// Per-call loader context (thread-local)
// ---------------------------------------------------------------------------

thread_local! {
    /// The (function_oid as u32, import_map) active for the current call.
    static LOADER_CTX: RefCell<Option<(u32, HashMap<String, String>)>> =
        RefCell::new(None);
}

/// RAII guard returned by [`set_loader_context`].
///
/// Clears the thread-local loader context when dropped, ensuring that a panic
/// or early return inside `execute_typescript_fn` cannot leave stale context
/// for the next call on the same thread.
pub struct LoaderContextGuard;

impl Drop for LoaderContextGuard {
    fn drop(&mut self) {
        LOADER_CTX.with(|c| *c.borrow_mut() = None);
    }
}

/// Set the loader context for the current function call.
///
/// Returns a [`LoaderContextGuard`] that clears the context on drop.
/// `fn_oid` is passed as a `u32` so this function can be called from
/// plhandler.rs in both test and non-test builds without the type diverging
/// across the cfg boundary.
pub fn set_loader_context(fn_oid: u32, import_map: HashMap<String, String>) -> LoaderContextGuard {
    LOADER_CTX.with(|c| *c.borrow_mut() = Some((fn_oid, import_map)));
    LoaderContextGuard
}

// ---------------------------------------------------------------------------
// PgModuleLoader
// ---------------------------------------------------------------------------

/// A `ModuleLoader` that resolves bare specifiers via the import map and loads
/// module source from `deno_internal.deno_package_modules`.
pub struct PgModuleLoader;

impl ModuleLoader for PgModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // file:// specifiers are always our own main module being normalised by
        // deno_core — pass through unconditionally.
        if specifier.starts_with("file://") {
            return ModuleSpecifier::parse(specifier).map_err(JsErrorBox::from_err);
        }

        // Determine whether this resolution originates from the main module
        // (file:// referrer) or from a transitive dependency (http/https referrer).
        // The two cases have different trust levels.
        if referrer.starts_with("file://") {
            // ---------------------------------------------------------------
            // Imports from the main module body.
            // Everything must be explicitly declared in the import map.
            // ---------------------------------------------------------------

            // Relative imports are not allowed in the function body.
            if specifier.starts_with('/')
                || specifier.starts_with("./")
                || specifier.starts_with("../")
            {
                return Err(JsErrorBox::generic(format!(
                    "pg_typescript: relative imports are not allowed in function body: '{specifier}'"
                )));
            }

            let url = LOADER_CTX.with(|c| {
                let ctx = c.borrow();
                let (_, import_map) = ctx
                    .as_ref()
                    .ok_or_else(|| JsErrorBox::generic("pg_typescript: no loader context set"))?;

                // Bare specifier → direct key lookup.
                if let Some(url) = import_map.get(specifier) {
                    return Ok(url.clone());
                }

                // Absolute URL → must appear as a declared value in the import map.
                if specifier.starts_with("http://") || specifier.starts_with("https://") {
                    if import_map.values().any(|v| v == specifier) {
                        return Ok(specifier.to_string());
                    }
                    return Err(JsErrorBox::generic(format!(
                        "pg_typescript: '{specifier}' is not declared in the import map"
                    )));
                }

                Err(JsErrorBox::generic(format!(
                    "pg_typescript: '{specifier}' not found in import map"
                )))
            })?;

            ModuleSpecifier::parse(&url).map_err(JsErrorBox::from_err)
        } else {
            // ---------------------------------------------------------------
            // Imports from a transitive dependency (http/https referrer).
            // Absolute URLs pass through; relative ones resolve against the referrer.
            // ---------------------------------------------------------------
            if specifier.starts_with("http://") || specifier.starts_with("https://") {
                return ModuleSpecifier::parse(specifier).map_err(JsErrorBox::from_err);
            }

            if specifier.starts_with('/')
                || specifier.starts_with("./")
                || specifier.starts_with("../")
            {
                let base = ModuleSpecifier::parse(referrer)
                    .unwrap_or_else(|_| ModuleSpecifier::parse("file:///").unwrap());
                return base.join(specifier).map_err(JsErrorBox::from_err);
            }

            // Bare specifier from a transitive dep — fall back to the import map.
            let url = LOADER_CTX.with(|c| {
                let ctx = c.borrow();
                let (_, import_map) = ctx
                    .as_ref()
                    .ok_or_else(|| JsErrorBox::generic("pg_typescript: no loader context set"))?;
                import_map
                    .get(specifier)
                    .cloned()
                    .ok_or_else(|| {
                        JsErrorBox::generic(format!(
                            "pg_typescript: '{specifier}' not found in import map"
                        ))
                    })
            })?;

            ModuleSpecifier::parse(&url).map_err(JsErrorBox::from_err)
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.as_str().to_string();

        let source: String = match load_module_source(url.clone()) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                    "pg_typescript: module not cached: {url}"
                ))));
            }
            Err(e) => {
                return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                    "pg_typescript: error loading {url}: {e}"
                ))));
            }
        };

        ModuleLoadResponse::Sync(Ok(ModuleSource::new(
            ModuleType::JavaScript,
            ModuleSourceCode::String(source.into()),
            module_specifier,
            None,
        )))
    }

    fn get_source_map(&self, _specifier: &str) -> Option<Cow<'_, [u8]>> {
        None
    }
}

// ---------------------------------------------------------------------------
// Module source retrieval — gated on non-test so Postgres stays out of the
// unit-test binary (which is not linked against libpq / Postgres shared libs).
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn load_module_source(url: String) -> Result<Option<String>, String> {
    let raw_oid: u32 = match LOADER_CTX.with(|c| c.borrow().as_ref().map(|(id, _)| *id)) {
        Some(id) => id,
        None => return Err("no loader context set".to_string()),
    };
    let fn_oid = pg_sys::Oid::from(raw_oid);

    Spi::connect(|client| {
        let rows = client.select(
            "SELECT source \
             FROM deno_internal.deno_package_modules \
             WHERE function_oid = $1 AND url = $2",
            None,
            &[fn_oid.into(), url.into()],
        )?;
        for row in rows {
            let src: Option<String> = row["source"].value()?;
            return Ok(src);
        }
        Ok::<Option<String>, SpiError>(None)
    })
    .map_err(|e| format!("{e:?}"))
}

#[cfg(test)]
fn load_module_source(_url: String) -> Result<Option<String>, String> {
    // Unit tests only run functions with empty import maps, so load() is
    // never called.  Return an error as a safety net.
    Err("module loading not available in unit tests".to_string())
}
