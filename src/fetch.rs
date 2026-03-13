use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use deno_core::futures::executor::block_on;
use deno_error::JsErrorBox;
use deno_graph::source::{LoadError, LoadFuture, LoadOptions, LoadResponse, Loader};
use deno_graph::{BuildOptions, GraphKind, ModuleGraph, ModuleSpecifier};

use crate::guc::{import_url_allowed, ImportUrlCap};
use crate::loader::ImportMapResolver;

// ---------------------------------------------------------------------------
// ModuleStore trait
// ---------------------------------------------------------------------------

pub trait ModuleStore {
    /// Return the cached source for `url` within the namespace of `fn_oid`.
    ///
    /// `Ok(None)` means the module is not present in the store.
    fn load(&self, fn_oid: u32, url: &str) -> Result<Option<String>, String>;

    /// Insert or replace the cached source for `url` within `fn_oid`.
    fn write(&mut self, fn_oid: u32, url: &str, source: &str);

    /// Remove all cached modules associated with `fn_oid`.
    fn clear_for_fn(&mut self, fn_oid: u32);
}

// ---------------------------------------------------------------------------
// HashMapModuleStore — in-memory store, used in tests
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct HashMapModuleStore(HashMap<(u32, String), String>);

impl HashMapModuleStore {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self(HashMap::new())
    }
}

#[allow(dead_code)]
impl Default for HashMapModuleStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleStore for HashMapModuleStore {
    fn load(&self, fn_oid: u32, url: &str) -> Result<Option<String>, String> {
        Ok(self.0.get(&(fn_oid, url.to_string())).cloned())
    }

    fn write(&mut self, fn_oid: u32, url: &str, source: &str) {
        self.0.insert((fn_oid, url.to_string()), source.to_string());
    }

    fn clear_for_fn(&mut self, fn_oid: u32) {
        self.0.retain(|(oid, _), _| *oid != fn_oid);
    }
}

// ---------------------------------------------------------------------------
// PgModuleStore — Postgres-backed store.
//
// Excluded from test builds: pgrx::Spi links against _CacheMemoryContext and
// other PostgreSQL globals that are not present in the unit-test binary.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct PgModuleStore;

impl ModuleStore for PgModuleStore {
    fn load(&self, fn_oid: u32, url: &str) -> Result<Option<String>, String> {
        use pgrx::spi::SpiError;
        use pgrx::{pg_sys, Spi};

        let pg_oid = pg_sys::Oid::from(fn_oid);
        Spi::connect(|client| {
            let mut rows = client.select(
                "SELECT source \
                 FROM deno_internal.deno_package_modules \
                 WHERE function_oid = $1 AND url = $2",
                None,
                &[pg_oid.into(), url.to_string().into()],
            )?;
            if let Some(row) = rows.next() {
                let src: Option<String> = row["source"].value()?;
                return Ok(src);
            }
            Ok::<Option<String>, SpiError>(None)
        })
        .map_err(|e| format!("{e:?}"))
    }

    fn write(&mut self, fn_oid: u32, url: &str, source: &str) {
        use pgrx::{pg_sys, Spi};

        let pg_oid = pg_sys::Oid::from(fn_oid);
        Spi::run_with_args(
            "INSERT INTO deno_internal.deno_package_modules (function_oid, url, source) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (function_oid, url) DO UPDATE SET source = EXCLUDED.source",
            &[
                pg_oid.into(),
                url.to_string().into(),
                source.to_string().into(),
            ],
        )
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to cache module {url}: {e:?}"));
    }

    fn clear_for_fn(&mut self, fn_oid: u32) {
        use pgrx::{pg_sys, Spi};

        let pg_oid = pg_sys::Oid::from(fn_oid);
        Spi::run_with_args(
            "DELETE FROM deno_internal.deno_package_modules WHERE function_oid = $1",
            &[pg_oid.into()],
        )
        .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to clear module cache: {e:?}"));
    }
}

// ---------------------------------------------------------------------------
// Fetcher trait
// ---------------------------------------------------------------------------

pub trait Fetcher {
    /// Fetch the module source for `url`, or raise a user-visible error if it
    /// cannot be retrieved.
    fn fetch(&self, url: &str) -> String;
}

// ---------------------------------------------------------------------------
// HashMapFetcher — pre-canned responses for tests
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct HashMapFetcher(HashMap<String, String>);

impl HashMapFetcher {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    #[allow(dead_code)]
    pub fn insert(&mut self, url: impl Into<String>, source: impl Into<String>) {
        self.0.insert(url.into(), source.into());
    }
}

#[allow(dead_code)]
impl Default for HashMapFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for HashMapFetcher {
    fn fetch(&self, url: &str) -> String {
        self.0
            .get(url)
            .cloned()
            .unwrap_or_else(|| panic!("HashMapFetcher: no entry for '{url}'"))
    }
}

// ---------------------------------------------------------------------------
// UreqFetcher — real HTTP client
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct UreqFetcher;

impl Fetcher for UreqFetcher {
    fn fetch(&self, url: &str) -> String {
        match ureq::get(url).call() {
            Ok(resp) => match resp.into_string() {
                Ok(s) => s,
                Err(e) => pgrx::error!("pg_typescript: failed to read response from {url}: {e}"),
            },
            Err(e) => pgrx::error!("pg_typescript: failed to fetch {url}: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Fetching and caching
// ---------------------------------------------------------------------------

/// Fetch all import-map entrypoints, then let `deno_graph` parse each module
/// and recursively discover the static dependency graph.
pub fn fetch_and_cache<S: ModuleStore + ?Sized, F: Fetcher + ?Sized>(
    fn_oid_raw: u32,
    import_map: &HashMap<String, String>,
    store: &mut S,
    fetcher: &F,
    max_imports: &ImportUrlCap,
) -> Result<(), String> {
    store.clear_for_fn(fn_oid_raw);

    let roots = import_map
        .values()
        .map(|url| {
            ModuleSpecifier::parse(url)
                .map_err(|e| format!("invalid import URL '{url}' in import_map: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let resolver = ImportMapResolver::new(import_map);
    let loader = PrefetchLoader::new(fn_oid_raw, store, fetcher, max_imports);
    let mut graph = ModuleGraph::new(GraphKind::CodeOnly);

    block_on(graph.build(
        roots,
        Vec::new(),
        &loader,
        BuildOptions {
            resolver: Some(&resolver),
            // Mirror the previous prefetch semantics: only follow static imports.
            skip_dynamic_deps: true,
            ..Default::default()
        },
    ));

    graph.valid().map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Graph loader
// ---------------------------------------------------------------------------

struct PrefetchLoader<'a, S: ModuleStore + ?Sized, F: Fetcher + ?Sized> {
    fn_oid: u32,
    store: RefCell<&'a mut S>,
    fetcher: &'a F,
    max_imports: &'a ImportUrlCap,
}

impl<'a, S: ModuleStore + ?Sized, F: Fetcher + ?Sized> PrefetchLoader<'a, S, F> {
    fn new(fn_oid: u32, store: &'a mut S, fetcher: &'a F, max_imports: &'a ImportUrlCap) -> Self {
        Self {
            fn_oid,
            store: RefCell::new(store),
            fetcher,
            max_imports,
        }
    }

    fn load_module(&self, specifier: &ModuleSpecifier) -> Result<Option<LoadResponse>, LoadError> {
        let url = specifier.as_str();

        if !import_url_allowed(url, self.max_imports)
            .map_err(|e| graph_load_error(format!("pg_typescript: {e}")))?
        {
            return Err(graph_load_error(format!(
                "import URL '{url}' is not allowed by GUC typescript.max_imports"
            )));
        }

        if let Some(source) = self
            .store
            .borrow()
            .load(self.fn_oid, url)
            .map_err(|e| graph_load_error(format!("pg_typescript: error loading {url}: {e}")))?
        {
            return Ok(Some(module_response(specifier.clone(), source)));
        }

        let source = self.fetcher.fetch(url);
        self.store.borrow_mut().write(self.fn_oid, url, &source);

        Ok(Some(module_response(specifier.clone(), source)))
    }
}

impl<S: ModuleStore + ?Sized, F: Fetcher + ?Sized> std::fmt::Debug for PrefetchLoader<'_, S, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrefetchLoader").finish()
    }
}

impl<S: ModuleStore + ?Sized, F: Fetcher + ?Sized> Loader for PrefetchLoader<'_, S, F> {
    fn load(&self, specifier: &ModuleSpecifier, _options: LoadOptions) -> LoadFuture {
        let result = self.load_module(specifier);
        Box::pin(async move { result })
    }
}

fn module_response(specifier: ModuleSpecifier, source: String) -> LoadResponse {
    LoadResponse::Module {
        content: Arc::from(source.into_bytes()),
        mtime: None,
        specifier,
        maybe_headers: None,
    }
}

fn graph_load_error(message: String) -> LoadError {
    LoadError::Other(Arc::new(JsErrorBox::generic(message)))
}

// ---------------------------------------------------------------------------
// Test utilities
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "pg_test")))]
pub(crate) fn make_import_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, not(feature = "pg_test")))]
mod tests {
    use super::*;
    use crate::guc::{GucParser, ImportMapParser, MaxImportsParser};

    fn parse_import_map_for_test(json: &str) -> Result<HashMap<String, String>, String> {
        ImportMapParser::new().parse_raw(Some(json.to_string()), "test import_map")
    }

    fn parse_max_imports_for_test(raw: Option<String>) -> Result<ImportUrlCap, String> {
        MaxImportsParser::new().parse_raw(raw, "test")
    }

    macro_rules! make_fetcher {
        ($( $url:expr => $src:expr ),* $(,)?) => {{
            let mut f = HashMapFetcher::new();
            $( f.insert($url, $src); )*
            f
        }};
    }

    // --- parse_import_map ---------------------------------------------------

    #[test]
    fn parse_import_map_cases() {
        let cases: &[(&str, &str, Result<Option<(&str, &str)>, ()>)] = &[
            (
                "single entry",
                r#"{"imports":{"lodash":"https://esm.sh/lodash@4"}}"#,
                Ok(Some(("lodash", "https://esm.sh/lodash@4"))),
            ),
            (
                "multiple entries - a",
                r#"{"imports":{"a":"https://esm.sh/a","b":"https://esm.sh/b"}}"#,
                Ok(Some(("a", "https://esm.sh/a"))),
            ),
            (
                "multiple entries - b",
                r#"{"imports":{"a":"https://esm.sh/a","b":"https://esm.sh/b"}}"#,
                Ok(Some(("b", "https://esm.sh/b"))),
            ),
            ("missing imports key", r#"{}"#, Ok(None)),
            ("empty imports", r#"{"imports":{}}"#, Ok(None)),
            ("invalid json", "not json", Err(())),
            ("value not string", r#"{"imports":{"a":42}}"#, Err(())),
            (
                "key starts with digit",
                r#"{"imports":{"1bad":"https://esm.sh/x"}}"#,
                Err(()),
            ),
            (
                "key with hyphen",
                r#"{"imports":{"my-pkg":"https://esm.sh/x"}}"#,
                Err(()),
            ),
            (
                "empty key",
                r#"{"imports":{"":"https://esm.sh/x"}}"#,
                Err(()),
            ),
            (
                "file url",
                r#"{"imports":{"pkg":"file:///local/pkg"}}"#,
                Err(()),
            ),
            (
                "ftp url",
                r#"{"imports":{"pkg":"ftp://example.com/pkg"}}"#,
                Err(()),
            ),
            ("invalid url", r#"{"imports":{"pkg":"://broken"}}"#, Err(())),
        ];

        for (name, input, expected) in cases {
            let result = parse_import_map_for_test(input);
            match expected {
                Ok(Some((key, url))) => {
                    let map =
                        result.unwrap_or_else(|e| panic!("[{name}] expected Ok, got Err: {e}"));
                    assert_eq!(map.get(*key), Some(&url.to_string()), "[{name}]");
                }
                Ok(None) => {
                    let map =
                        result.unwrap_or_else(|e| panic!("[{name}] expected Ok, got Err: {e}"));
                    assert!(map.is_empty(), "[{name}] expected empty map");
                }
                Err(()) => {
                    assert!(result.is_err(), "[{name}] expected Err, got Ok");
                }
            }
        }
    }

    // --- fetch_and_cache ----------------------------------------------------

    #[test]
    fn fetch_single_module() {
        let fetcher =
            make_fetcher!["https://esm.sh/lodash@4" => "export const add = (a, b) => a + b;"];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            42,
            &make_import_map(&[("lodash", "https://esm.sh/lodash@4")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert_eq!(
            store.load(42, "https://esm.sh/lodash@4"),
            Ok(Some("export const add = (a, b) => a + b;".to_string()))
        );
    }

    #[test]
    fn fetch_clears_stale_entries() {
        let fetcher = make_fetcher!["https://esm.sh/lodash@4" => "new source"];
        let mut store = HashMapModuleStore::new();
        store.write(42, "https://esm.sh/stale", "old");

        fetch_and_cache(
            42,
            &make_import_map(&[("lodash", "https://esm.sh/lodash@4")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert_eq!(store.load(42, "https://esm.sh/stale"), Ok(None));
        assert!(store.load(42, "https://esm.sh/lodash@4").unwrap().is_some());
    }

    #[test]
    fn fetch_does_not_clear_other_oid() {
        let fetcher = make_fetcher!["https://esm.sh/lodash@4" => "source"];
        let mut store = HashMapModuleStore::new();
        store.write(99, "https://esm.sh/other", "keep me");

        fetch_and_cache(
            42,
            &make_import_map(&[("lodash", "https://esm.sh/lodash@4")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert_eq!(
            store.load(99, "https://esm.sh/other"),
            Ok(Some("keep me".to_string()))
        );
    }

    #[test]
    fn fetch_transitive_dep() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg@1" => "export { x } from './utils.js';",
            "https://esm.sh/utils.js" => "export const x = 1;",
        ];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[("pkg", "https://esm.sh/pkg@1")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert!(store.load(0, "https://esm.sh/pkg@1").unwrap().is_some());
        assert!(store.load(0, "https://esm.sh/utils.js").unwrap().is_some());
    }

    #[test]
    fn fetch_side_effect_import() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg@1" => "import './side_effect.js'; export const x = 1;",
            "https://esm.sh/side_effect.js" => "globalThis.side = true;",
        ];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[("pkg", "https://esm.sh/pkg@1")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert!(store
            .load(0, "https://esm.sh/side_effect.js")
            .unwrap()
            .is_some());
    }

    #[test]
    fn fetch_export_all_dep() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg@1" => "export * from './utils.js';",
            "https://esm.sh/utils.js" => "export const x = 1;",
        ];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[("pkg", "https://esm.sh/pkg@1")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert!(store.load(0, "https://esm.sh/utils.js").unwrap().is_some());
    }

    #[test]
    fn fetch_multiline_import() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg@1" => "import {\n  x,\n} from './utils.js';\nexport { x };",
            "https://esm.sh/utils.js" => "export const x = 1;",
        ];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[("pkg", "https://esm.sh/pkg@1")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert!(store.load(0, "https://esm.sh/utils.js").unwrap().is_some());
    }

    #[test]
    fn fetch_root_relative_dep() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg/index.js" => "export { x } from '/shared.js';",
            "https://esm.sh/shared.js" => "export const x = 1;",
        ];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[("pkg", "https://esm.sh/pkg/index.js")]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert!(store.load(0, "https://esm.sh/shared.js").unwrap().is_some());
    }

    #[test]
    fn fetch_transitive_bare_specifier_from_import_map() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg@1" => "export { z } from 'zod';",
            "https://esm.sh/zod@3" => "export const z = 3;",
        ];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[
                ("pkg", "https://esm.sh/pkg@1"),
                ("zod", "https://esm.sh/zod@3"),
            ]),
            &mut store,
            &fetcher,
            &ImportUrlCap::AllowAll,
        )
        .unwrap();

        assert!(store.load(0, "https://esm.sh/zod@3").unwrap().is_some());
    }

    // --- max_imports --------------------------------------------------------

    #[test]
    fn parse_max_imports_default_is_allow_all() {
        assert_eq!(
            parse_max_imports_for_test(None).unwrap(),
            ImportUrlCap::AllowAll
        );
    }

    #[test]
    fn parse_max_imports_keywords() {
        assert_eq!(
            parse_max_imports_for_test(Some("*".to_string())).unwrap(),
            ImportUrlCap::AllowAll
        );
        assert_eq!(
            parse_max_imports_for_test(Some("none".to_string())).unwrap(),
            ImportUrlCap::Deny
        );
    }

    #[test]
    fn parse_max_imports_url_list_normalizes_and_deduplicates() {
        assert_eq!(
            parse_max_imports_for_test(Some(
                " https://esm.sh , https://esm.sh/ , https://deno.land/x/ ".to_string(),
            ))
            .unwrap(),
            ImportUrlCap::AllowList(vec![
                "https://esm.sh/".to_string(),
                "https://deno.land/x/".to_string(),
            ])
        );
    }

    #[test]
    fn parse_max_imports_rejects_non_http_scheme() {
        let err = parse_max_imports_for_test(Some("file:///tmp".to_string())).unwrap_err();
        assert!(err.contains("unsupported scheme"), "err={err}");
    }

    #[test]
    fn fetch_rejects_disallowed_transitive_url() {
        let fetcher = make_fetcher![
            "https://esm.sh/pkg@1" => "export { x } from 'https://deno.land/x/mod.ts';",
            "https://deno.land/x/mod.ts" => "export const x = 1;",
        ];
        let mut store = HashMapModuleStore::new();
        let cap = ImportUrlCap::AllowList(vec!["https://esm.sh/".to_string()]);

        let err = fetch_and_cache(
            0,
            &make_import_map(&[("pkg", "https://esm.sh/pkg@1")]),
            &mut store,
            &fetcher,
            &cap,
        )
        .unwrap_err();

        assert!(
            err.contains("not allowed by GUC typescript.max_imports"),
            "err={err}"
        );
    }
}
