use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use deno_core::{
    error::ModuleLoaderError, ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse,
    ModuleLoader, ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
};
use deno_error::JsErrorBox;
use deno_graph::source::{ResolveError as GraphResolveError, Resolver as GraphResolver};

use crate::fetch::ModuleStore;

// ---------------------------------------------------------------------------
// Per-call loader context (thread-local)
// ---------------------------------------------------------------------------

struct LoaderContext {
    fn_oid: u32,
    import_map: HashMap<String, String>,
    store: Box<dyn ModuleStore>,
    inline_modules: HashMap<String, String>,
}

thread_local! {
    static LOADER_CTX: RefCell<Option<LoaderContext>> = const { RefCell::new(None) };
}

/// RAII guard returned by [`set_loader_context`].
///
/// Clears the thread-local context when dropped, ensuring that a panic or
/// early return cannot leave stale state for the next call on the same thread.
pub struct LoaderContextGuard;

impl Drop for LoaderContextGuard {
    fn drop(&mut self) {
        LOADER_CTX.with(|c| *c.borrow_mut() = None);
    }
}

/// Set the loader context for the current function call.
///
/// Returns a [`LoaderContextGuard`] that clears the context on drop.
#[cfg(all(test, not(feature = "pg_test")))]
pub fn set_loader_context(
    fn_oid: u32,
    import_map: HashMap<String, String>,
    store: Box<dyn ModuleStore>,
) -> LoaderContextGuard {
    set_loader_context_with_inline(fn_oid, import_map, store, HashMap::new())
}

/// Set the loader context for the current function call, with optional
/// in-memory module sources keyed by absolute specifier URL.
pub fn set_loader_context_with_inline(
    fn_oid: u32,
    import_map: HashMap<String, String>,
    store: Box<dyn ModuleStore>,
    inline_modules: HashMap<String, String>,
) -> LoaderContextGuard {
    LOADER_CTX.with(|c| {
        *c.borrow_mut() = Some(LoaderContext {
            fn_oid,
            import_map,
            store,
            inline_modules,
        });
    });
    LoaderContextGuard
}

// ---------------------------------------------------------------------------
// PgModuleLoader
// ---------------------------------------------------------------------------

/// A `ModuleLoader` that resolves bare specifiers via the import map and loads
/// module source from the active [`ModuleStore`].
pub struct PgModuleLoader;

impl ModuleLoader for PgModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // Only allow file:// for the loader entrypoint specifier (resolved with
        // "." as the referrer by deno_core). Any nested file:// import from
        // user code is rejected to prevent reaching synthetic fn_* modules.
        if specifier.starts_with("file://") {
            if referrer != "." {
                return Err(JsErrorBox::generic(format!(
                    "pg_typescript: file:// imports are not allowed from function code: '{specifier}'"
                )));
            }
            return ModuleSpecifier::parse(specifier).map_err(JsErrorBox::from_err);
        }

        // Dispatch based on the trust level of the referrer.
        if referrer.starts_with("file://") {
            resolve_from_main(specifier)
        } else {
            resolve_from_dep(specifier, referrer)
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.as_str();

        let source: String = match load_module_source(url) {
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

        let source = match maybe_transpile_source(module_specifier, source) {
            Ok(source) => source,
            Err(e) => return ModuleLoadResponse::Sync(Err(e)),
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
// Resolution helpers
// ---------------------------------------------------------------------------

/// Shared import-map resolution rules used by both the runtime loader and the
/// validation-time prefetch graph walker.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImportMapResolver<'a> {
    import_map: &'a HashMap<String, String>,
}

impl<'a> ImportMapResolver<'a> {
    pub(crate) fn new(import_map: &'a HashMap<String, String>) -> Self {
        Self { import_map }
    }

    pub(crate) fn resolve_from_main(&self, specifier: &str) -> Result<ModuleSpecifier, JsErrorBox> {
        if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../")
        {
            return Err(JsErrorBox::generic(format!(
                "pg_typescript: relative imports are not allowed in function body: '{specifier}'"
            )));
        }

        let url: &str = match self.import_map.get(specifier) {
            Some(url) => url,
            None if specifier.starts_with("http://") || specifier.starts_with("https://") => {
                if self.import_map.values().any(|value| value == specifier) {
                    specifier
                } else {
                    return Err(JsErrorBox::generic(format!(
                        "pg_typescript: '{specifier}' is not declared in the import map"
                    )));
                }
            }
            None => {
                return Err(JsErrorBox::generic(format!(
                    "pg_typescript: '{specifier}' not found in import map"
                )));
            }
        };

        ModuleSpecifier::parse(url).map_err(JsErrorBox::from_err)
    }

    pub(crate) fn resolve_from_dependency(
        &self,
        specifier: &str,
        referrer: &ModuleSpecifier,
    ) -> Result<ModuleSpecifier, JsErrorBox> {
        if specifier.starts_with("http://") || specifier.starts_with("https://") {
            return ModuleSpecifier::parse(specifier).map_err(JsErrorBox::from_err);
        }

        if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../")
        {
            return referrer.join(specifier).map_err(JsErrorBox::from_err);
        }

        let url = self.import_map.get(specifier).ok_or_else(|| {
            JsErrorBox::generic(format!(
                "pg_typescript: '{specifier}' not found in import map"
            ))
        })?;

        ModuleSpecifier::parse(url).map_err(JsErrorBox::from_err)
    }
}

impl GraphResolver for ImportMapResolver<'_> {
    fn resolve(
        &self,
        specifier_text: &str,
        referrer_range: &deno_graph::Range,
        _kind: deno_graph::source::ResolutionKind,
    ) -> Result<ModuleSpecifier, GraphResolveError> {
        self.resolve_from_dependency(specifier_text, &referrer_range.specifier)
            .map_err(GraphResolveError::from_err)
    }
}

fn with_import_map_resolver<T>(
    f: impl FnOnce(ImportMapResolver<'_>) -> Result<T, JsErrorBox>,
) -> Result<T, JsErrorBox> {
    LOADER_CTX.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx
            .as_ref()
            .ok_or_else(|| JsErrorBox::generic("pg_typescript: no loader context set"))?;
        f(ImportMapResolver::new(&ctx.import_map))
    })
}

/// Resolve an import that originates from the main module body.
///
/// Everything must be explicitly declared in the import map — relative imports
/// are rejected outright, bare specifiers are looked up by key, and absolute
/// http/https URLs must appear as a declared value.
fn resolve_from_main(specifier: &str) -> Result<ModuleSpecifier, ModuleLoaderError> {
    with_import_map_resolver(|resolver| resolver.resolve_from_main(specifier))
}

/// Resolve an import that originates from a transitive dependency.
///
/// Absolute http/https URLs pass through freely; relative specifiers are
/// resolved against the referrer. Bare specifiers fall back to the import map.
fn resolve_from_dep(specifier: &str, referrer: &str) -> Result<ModuleSpecifier, ModuleLoaderError> {
    let base = ModuleSpecifier::parse(referrer)
        .unwrap_or_else(|_| ModuleSpecifier::parse("file:///").unwrap());
    with_import_map_resolver(|resolver| resolver.resolve_from_dependency(specifier, &base))
}

// ---------------------------------------------------------------------------
// Module source retrieval
// ---------------------------------------------------------------------------

fn load_module_source(url: &str) -> Result<Option<String>, String> {
    LOADER_CTX.with(|c| match c.borrow().as_ref() {
        Some(ctx) => {
            if let Some(source) = ctx.inline_modules.get(url) {
                return Ok(Some(source.clone()));
            }
            ctx.store.load(ctx.fn_oid, url)
        }
        None => Err("no loader context set".to_string()),
    })
}

fn maybe_transpile_source(
    module_specifier: &ModuleSpecifier,
    source: String,
) -> Result<String, JsErrorBox> {
    let name = module_specifier.as_str();
    if !should_transpile(name) {
        return Ok(source);
    }

    let (transpiled, _) =
        deno_runtime::transpile::maybe_transpile_source(name.to_string().into(), source.into())?;
    Ok(transpiled.to_string())
}

fn should_transpile(name: &str) -> bool {
    if name.starts_with("node:") {
        return true;
    }

    let Ok(specifier) = ModuleSpecifier::parse(name) else {
        return false;
    };
    let path = specifier.path();
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|ext| {
        path.len() >= ext.len()
            && path[path.len() - ext.len()..].eq_ignore_ascii_case(ext)
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "pg_test")))]
mod tests {
    use deno_core::{ModuleLoader, ResolutionKind};

    use crate::fetch::{make_import_map, HashMapModuleStore, ModuleStore};

    /// Set up a loader context with the given import map entries and a
    /// pre-populated store, returning the RAII guard.
    fn make_ctx(
        map_entries: &[(&str, &str)],
        store: HashMapModuleStore,
    ) -> super::LoaderContextGuard {
        super::set_loader_context(0, make_import_map(map_entries), Box::new(store))
    }

    // --- load_module_source -------------------------------------------------

    #[test]
    fn load_hit() {
        let mut store = HashMapModuleStore::new();
        store.write(0, "https://esm.sh/lodash@4", "export default 42;");
        let _ctx = make_ctx(&[("lodash", "https://esm.sh/lodash@4")], store);

        assert_eq!(
            super::load_module_source("https://esm.sh/lodash@4"),
            Ok(Some("export default 42;".to_string()))
        );
    }

    #[test]
    fn load_miss() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        assert_eq!(
            super::load_module_source("https://esm.sh/missing"),
            Ok(None)
        );
    }

    #[test]
    fn load_no_context() {
        // No context set — must return Err.
        let result = super::load_module_source("https://esm.sh/x");
        assert!(result.is_err());
    }

    // --- resolve_from_main --------------------------------------------------

    #[test]
    fn resolve_main_bare_in_map() {
        let _ctx = make_ctx(
            &[("lodash", "https://esm.sh/lodash@4")],
            HashMapModuleStore::new(),
        );

        let url = super::resolve_from_main("lodash").unwrap();
        assert_eq!(url.as_str(), "https://esm.sh/lodash@4");
    }

    #[test]
    fn resolve_main_bare_not_in_map() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        assert!(super::resolve_from_main("lodash").is_err());
    }

    #[test]
    fn resolve_main_relative_rejected() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        assert!(super::resolve_from_main("./foo").is_err());
        assert!(super::resolve_from_main("../bar").is_err());
        assert!(super::resolve_from_main("/abs").is_err());
    }

    #[test]
    fn resolve_main_absolute_declared() {
        // An absolute URL that appears as a value in the import map is allowed.
        let _ctx = make_ctx(
            &[("lodash", "https://esm.sh/lodash@4")],
            HashMapModuleStore::new(),
        );

        let url = super::resolve_from_main("https://esm.sh/lodash@4").unwrap();
        assert_eq!(url.as_str(), "https://esm.sh/lodash@4");
    }

    #[test]
    fn resolve_main_absolute_undeclared() {
        // An absolute URL not present in the import map must be rejected.
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        assert!(super::resolve_from_main("https://esm.sh/undeclared").is_err());
    }

    // --- resolve_from_dep ---------------------------------------------------

    #[test]
    fn resolve_dep_absolute_passthrough() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        let url = super::resolve_from_dep("https://esm.sh/other@1", "https://esm.sh/pkg/index.js")
            .unwrap();
        assert_eq!(url.as_str(), "https://esm.sh/other@1");
    }

    #[test]
    fn resolve_dep_relative() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        let url = super::resolve_from_dep("./utils.js", "https://esm.sh/pkg/index.js").unwrap();
        assert_eq!(url.as_str(), "https://esm.sh/pkg/utils.js");
    }

    #[test]
    fn resolve_dep_relative_parent() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        let url =
            super::resolve_from_dep("../shared.js", "https://esm.sh/pkg/sub/index.js").unwrap();
        assert_eq!(url.as_str(), "https://esm.sh/pkg/shared.js");
    }

    #[test]
    fn resolve_dep_bare_in_map() {
        let _ctx = make_ctx(
            &[("zod", "https://esm.sh/zod@3")],
            HashMapModuleStore::new(),
        );

        let url = super::resolve_from_dep("zod", "https://esm.sh/some-dep/index.js").unwrap();
        assert_eq!(url.as_str(), "https://esm.sh/zod@3");
    }

    #[test]
    fn resolve_dep_bare_not_in_map() {
        let _ctx = make_ctx(&[], HashMapModuleStore::new());

        assert!(super::resolve_from_dep("unknown", "https://esm.sh/pkg/index.js").is_err());
    }

    #[test]
    fn resolve_file_root_allowed() {
        let loader = super::PgModuleLoader;
        let url = loader
            .resolve(
                "file:///pg_typescript/fn_1_deadbeefdeadbeef.ts",
                ".",
                ResolutionKind::Import,
            )
            .unwrap();
        assert_eq!(
            url.as_str(),
            "file:///pg_typescript/fn_1_deadbeefdeadbeef.ts"
        );
    }

    #[test]
    fn resolve_file_from_function_rejected() {
        let loader = super::PgModuleLoader;
        let err = loader
            .resolve(
                "file:///pg_typescript/fn_2_deadbeefdeadbeef.ts",
                "file:///pg_typescript/fn_1_deadbeefdeadbeef.ts",
                ResolutionKind::Import,
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("file:// imports are not allowed"),
            "unexpected error: {err}"
        );
    }
}
