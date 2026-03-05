use std::collections::HashMap;

use serde_json::Value;

// ---------------------------------------------------------------------------
// ModuleStore trait
// ---------------------------------------------------------------------------

pub trait ModuleStore {
    fn load(&self, fn_oid: u32, url: &str) -> Result<Option<String>, String>;
    fn write(&mut self, fn_oid: u32, url: &str, source: &str);
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
            let rows = client.select(
                "SELECT source \
                 FROM deno_internal.deno_package_modules \
                 WHERE function_oid = $1 AND url = $2",
                None,
                &[pg_oid.into(), url.to_string().into()],
            )?;
            for row in rows {
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
// Import map parsing (pure Rust, no Postgres)
// ---------------------------------------------------------------------------

/// Parse and validate a Deno-style import map JSON string into a `{specifier -> url}` map.
///
/// Expected format: `{"imports": {"lodash": "https://esm.sh/lodash@4.17.23"}}`
///
/// Returns an error if the JSON is malformed, if any key is not a valid JS
/// identifier, or if any URL is not an http/https URL.
pub fn parse_import_map(json: &str) -> Result<HashMap<String, String>, String> {
    let v: Value =
        serde_json::from_str(json).map_err(|e| format!("invalid import_map JSON: {e}"))?;

    let imports = match v.get("imports").and_then(|i| i.as_object()) {
        Some(obj) => obj,
        None => return Ok(HashMap::new()),
    };

    let mut map = HashMap::new();
    for (key, val) in imports {
        validate_js_identifier(key)?;
        let url = val
            .as_str()
            .ok_or_else(|| format!("import_map: value for '{key}' is not a string"))?;
        validate_http_url(url)?;
        map.insert(key.clone(), url.to_string());
    }
    Ok(map)
}

fn validate_js_identifier(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first = chars
        .next()
        .ok_or_else(|| "import_map key is empty".to_string())?;
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return Err(format!(
            "import_map key '{name}' is not a valid JS identifier \
             (must start with a letter, '_', or '$')"
        ));
    }
    for ch in chars {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') {
            return Err(format!(
                "import_map key '{name}' is not a valid JS identifier \
                 (invalid character '{ch}')"
            ));
        }
    }
    Ok(())
}

fn validate_http_url(url: &str) -> Result<(), String> {
    use deno_core::ModuleSpecifier;
    let parsed = ModuleSpecifier::parse(url)
        .map_err(|e| format!("import_map URL '{url}' is invalid: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(format!(
            "import_map URL '{url}' has unsupported scheme '{scheme}' (only http/https allowed)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Fetching and caching
// ---------------------------------------------------------------------------

/// Fetch all modules in the import map (and transitive dependencies) and
/// write them into the provided store using the provided fetcher.
pub fn fetch_and_cache<S: ModuleStore + ?Sized, F: Fetcher + ?Sized>(
    fn_oid_raw: u32,
    import_map: &HashMap<String, String>,
    store: &mut S,
    fetcher: &F,
) {
    use std::collections::HashSet;

    store.clear_for_fn(fn_oid_raw);

    let mut visited: HashSet<String> = HashSet::new();
    for url in import_map.values() {
        fetch_recursive(fn_oid_raw, url, &mut visited, store, fetcher);
    }
}

fn fetch_recursive<S: ModuleStore + ?Sized, F: Fetcher + ?Sized>(
    fn_oid_raw: u32,
    url: &str,
    visited: &mut std::collections::HashSet<String>,
    store: &mut S,
    fetcher: &F,
) {
    if visited.contains(url) {
        return;
    }
    visited.insert(url.to_string());

    let source = fetcher.fetch(url);
    store.write(fn_oid_raw, url, &source);

    for dep_specifier in extract_imports(&source) {
        if let Some(dep_url) = resolve_specifier(&dep_specifier, url) {
            fetch_recursive(fn_oid_raw, &dep_url, visited, store, fetcher);
        }
    }
}

// ---------------------------------------------------------------------------
// Import extraction (pure Rust)
// ---------------------------------------------------------------------------

fn extract_imports(source: &str) -> Vec<String> {
    source.lines().filter_map(extract_from_specifier).collect()
}

fn extract_from_specifier(line: &str) -> Option<String> {
    let from_idx = line.rfind(" from ")?;
    let after = line[from_idx + 6..].trim();
    let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

fn resolve_specifier(specifier: &str, referrer: &str) -> Option<String> {
    use deno_core::ModuleSpecifier;
    if specifier.starts_with("http://") || specifier.starts_with("https://") {
        return Some(specifier.to_string());
    }
    let base = ModuleSpecifier::parse(referrer).ok()?;
    if specifier.starts_with('/') {
        let origin = format!(
            "{}://{}",
            base.scheme(),
            base.host_str().unwrap_or("localhost")
        );
        return Some(format!("{origin}{specifier}"));
    }
    base.join(specifier).ok().map(|u| u.to_string())
}

// ---------------------------------------------------------------------------
// Test utilities
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) fn make_import_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        // (name, input, expected)
        // Ok(Some((key, url))) — parse succeeds, verify this entry is present
        // Ok(None)             — parse succeeds, map is empty
        // Err(())              — parse must return Err
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
            let result = parse_import_map(input);
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
        );

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
        );

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
        );

        assert_eq!(
            store.load(99, "https://esm.sh/other"),
            Ok(Some("keep me".to_string()))
        );
    }

    #[test]
    fn fetch_transitive_dep() {
        // pkg@1 imports ./utils.js, which resolves to https://esm.sh/utils.js
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
        );

        assert!(store.load(0, "https://esm.sh/pkg@1").unwrap().is_some());
        assert!(store.load(0, "https://esm.sh/utils.js").unwrap().is_some());
    }

    #[test]
    fn fetch_deduplicates_urls() {
        // Two import map entries pointing to the same URL — fetched only once.
        let fetcher = make_fetcher!["https://esm.sh/shared@1" => "export const s = 1;"];
        let mut store = HashMapModuleStore::new();

        fetch_and_cache(
            0,
            &make_import_map(&[
                ("a", "https://esm.sh/shared@1"),
                ("b", "https://esm.sh/shared@1"),
            ]),
            &mut store,
            &fetcher,
        );

        // Would panic inside HashMapFetcher if fetched more than once and we
        // hadn't inserted it — but the visited set prevents duplicate fetches.
        assert!(store.load(0, "https://esm.sh/shared@1").unwrap().is_some());
    }

    // --- extract_from_specifier ---------------------------------------------

    #[test]
    fn extract_double_quote() {
        assert_eq!(
            extract_from_specifier(r#"import { foo } from "lodash""#),
            Some("lodash".to_string())
        );
    }

    #[test]
    fn extract_single_quote() {
        assert_eq!(
            extract_from_specifier("export { bar } from 'https://esm.sh/x'"),
            Some("https://esm.sh/x".to_string())
        );
    }

    #[test]
    fn extract_no_from_clause() {
        assert_eq!(extract_from_specifier("const x = 1;"), None);
    }

    #[test]
    fn extract_uses_last_from() {
        // rfind means we pick the last " from " on the line.
        assert_eq!(
            extract_from_specifier(r#"export { from } from "mod""#),
            Some("mod".to_string())
        );
    }

    // --- resolve_specifier --------------------------------------------------

    #[test]
    fn resolve_absolute_passthrough() {
        assert_eq!(
            resolve_specifier("https://esm.sh/other", "https://esm.sh/pkg"),
            Some("https://esm.sh/other".to_string())
        );
    }

    #[test]
    fn resolve_relative() {
        assert_eq!(
            resolve_specifier("./utils.js", "https://esm.sh/pkg/index.js"),
            Some("https://esm.sh/pkg/utils.js".to_string())
        );
    }

    #[test]
    fn resolve_parent_relative() {
        assert_eq!(
            resolve_specifier("../shared.js", "https://esm.sh/pkg/sub/index.js"),
            Some("https://esm.sh/pkg/shared.js".to_string())
        );
    }

    #[test]
    fn resolve_root_relative() {
        assert_eq!(
            resolve_specifier("/v135/lodash@4/index.js", "https://esm.sh/pkg"),
            Some("https://esm.sh/v135/lodash@4/index.js".to_string())
        );
    }
}
