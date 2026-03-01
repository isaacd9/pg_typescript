use std::collections::HashMap;

use serde_json::Value;

// ---------------------------------------------------------------------------
// Import map parsing (pure Rust, no Postgres)
// ---------------------------------------------------------------------------

/// Parse a Deno-style import map JSON string into a `{specifier -> url}` map.
///
/// Expected format: `{"imports": {"lodash": "https://esm.sh/lodash@4.17.23"}}`
pub fn parse_import_map(json: &str) -> HashMap<String, String> {
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let mut map = HashMap::new();
    if let Some(imports) = v.get("imports").and_then(|i| i.as_object()) {
        for (key, val) in imports {
            if let Some(url) = val.as_str() {
                map.insert(key.clone(), url.to_string());
            }
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Fetching and caching — Postgres / pgrx only in non-test builds.
// Unit test binaries do not link against Postgres shared libraries, so any
// direct reference to Postgres C symbols causes a dyld abort at load time.
// ---------------------------------------------------------------------------

/// Fetch all modules in the import map (and transitive dependencies) and
/// write the sources into `deno_internal.deno_package_modules`.
///
/// `fn_oid_raw` is the function OID as a plain `u32` to avoid pulling
/// `pgrx::pg_sys::Oid` into the public interface.
#[cfg(not(test))]
pub fn fetch_and_cache(fn_oid_raw: u32, import_map: &HashMap<String, String>) {
    use std::collections::HashSet;
    use pgrx::pg_sys;

    let fn_oid = pg_sys::Oid::from(fn_oid_raw);
    let mut visited: HashSet<String> = HashSet::new();
    for url in import_map.values() {
        fetch_recursive(fn_oid, url, &mut visited);
    }
}

#[cfg(test)]
pub fn fetch_and_cache(_fn_oid_raw: u32, _import_map: &HashMap<String, String>) {
    // No-op in unit tests: there is no Postgres database to write to.
}

// ---------------------------------------------------------------------------
// Implementation (non-test only)
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn fetch_recursive(
    fn_oid: pgrx::pg_sys::Oid,
    url: &str,
    visited: &mut std::collections::HashSet<String>,
) {
    if visited.contains(url) {
        return;
    }
    visited.insert(url.to_string());

    let source = fetch_url(url);
    write_module(fn_oid, url, &source);

    for dep_specifier in extract_imports(&source) {
        if let Some(dep_url) = resolve_specifier(&dep_specifier, url) {
            fetch_recursive(fn_oid, &dep_url, visited);
        }
    }
}

#[cfg(not(test))]
fn fetch_url(url: &str) -> String {
    match ureq::get(url).call() {
        Ok(resp) => match resp.into_string() {
            Ok(s) => s,
            Err(e) => pgrx::error!("pg_typescript: failed to read response from {url}: {e}"),
        },
        Err(e) => pgrx::error!("pg_typescript: failed to fetch {url}: {e}"),
    }
}

#[cfg(not(test))]
fn write_module(fn_oid: pgrx::pg_sys::Oid, url: &str, source: &str) {
    use pgrx::Spi;
    Spi::run_with_args(
        "INSERT INTO deno_internal.deno_package_modules (function_oid, url, source) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (function_oid, url) DO UPDATE SET source = EXCLUDED.source",
        &[fn_oid.into(), url.to_string().into(), source.to_string().into()],
    )
    .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to cache module {url}: {e:?}"));
}

// ---------------------------------------------------------------------------
// Import extraction (pure Rust, used only in non-test builds)
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn extract_imports(source: &str) -> Vec<String> {
    source.lines().filter_map(extract_from_specifier).collect()
}

#[cfg(not(test))]
fn extract_from_specifier(line: &str) -> Option<String> {
    let from_idx = line.rfind(" from ")?;
    let after = line[from_idx + 6..].trim();
    let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = &after[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

#[cfg(not(test))]
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
