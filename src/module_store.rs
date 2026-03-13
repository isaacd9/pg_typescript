//! Shared module storage used by both cache population and runtime loading.

use std::collections::HashMap;

/// Storage for fetched module source, scoped by PostgreSQL function OID.
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
