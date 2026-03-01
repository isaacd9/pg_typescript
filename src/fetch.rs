use std::collections::HashMap;

use serde_json::Value;

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
    let v: Value = serde_json::from_str(json)
        .map_err(|e| format!("invalid import_map JSON: {e}"))?;

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
    use pgrx::{Spi, pg_sys};

    let fn_oid = pg_sys::Oid::from(fn_oid_raw);

    // Delete all previously cached modules for this function so that stale
    // entries from a prior definition (with a different import map) are removed.
    Spi::run_with_args(
        "DELETE FROM deno_internal.deno_package_modules WHERE function_oid = $1",
        &[fn_oid.into()],
    )
    .unwrap_or_else(|e| pgrx::error!("pg_typescript: failed to clear module cache: {e:?}"));

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
