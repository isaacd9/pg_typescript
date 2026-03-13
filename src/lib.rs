use pgrx::guc::{GucContext, GucFlags, GucRegistry};
use pgrx::prelude::*;

pgrx::pg_module_magic!(name, version);

mod convert;
mod extensions;
mod fetch;
mod guc;
mod loader;
mod module_store;
mod permissions;
mod plhandler;
mod runtime;

use crate::guc::{BoolGucParser, GucParser, ImportMapParser, MaxImportsParser, PermissionParser};

/// GUC for DO-block import maps. Use `SET LOCAL typescript.import_map = '{"imports": {...}}'`
/// before a DO block so the setting reverts automatically at transaction end.
/// Per-function import maps are stored in proconfig via `CREATE FUNCTION … SET`.
/// Default: unset (`None`), treated as no import map.
pub(crate) static IMPORT_MAP_GUC: ImportMapParser = ImportMapParser::new();
/// Superuser cap for allowed import-map URL prefixes. Values:
/// - `off|none|deny|false` => deny all imports
/// - `*|all|on|true` => allow all imports
/// - `a,b,c` => allowlist of URL prefixes
///
/// Default: unset (`None`), treated as allow all imports.
pub(crate) static MAX_IMPORTS_GUC: MaxImportsParser = MaxImportsParser::new();

/// Userset permission request knobs (function-level via `CREATE FUNCTION ... SET`, or
/// session/local for DO blocks). Values:
/// - `off|none|deny|false` => deny
/// - `*|all|on|true` => allow all
/// - `a,b,c` => allowlist
///
/// Default for each `allow_*` GUC: unset (`None`), treated as deny.
pub(crate) static ALLOW_READ_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_WRITE_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_NET_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_ENV_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_RUN_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_FFI_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_SYS_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_IMPORT_GUC: PermissionParser = PermissionParser::new();
/// Request access to `_pg.execute()` from a function or DO block.
/// Default: unset (`None`), treated as deny.
pub(crate) static ALLOW_PG_EXECUTE_GUC: BoolGucParser = BoolGucParser::new();

/// Superuser caps for each permission. `allow_*` requests must be fully
/// satisfiable by `max_allow_*`; otherwise execution fails with an error.
/// Default for each `max_allow_*` GUC: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_READ_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_WRITE_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_NET_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_ENV_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_RUN_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_FFI_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_SYS_GUC: PermissionParser = PermissionParser::new();
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_IMPORT_GUC: PermissionParser = PermissionParser::new();
/// Superuser cap for `_pg.execute()` access.
/// Default: unset (`None`), treated as deny.
pub(crate) static MAX_ALLOW_PG_EXECUTE_GUC: BoolGucParser = BoolGucParser::new();

// Register the GUC for per-function import maps.
#[pg_guard]
pub unsafe extern "C-unwind" fn _PG_init() {
    GucRegistry::define_string_guc(
        c"typescript.import_map",
        c"Deno-style import map JSON for pg_typescript functions, e.g. {\"imports\":{\"lodash\":\"https://esm.sh/lodash@4.17.23\"}}",
        c"",
        IMPORT_MAP_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_imports",
        c"Superuser max import URL cap: off|*|comma-list of http(s) URL prefixes",
        c"",
        MAX_IMPORTS_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"typescript.allow_read",
        c"Requested read permission: off|*|comma-list",
        c"",
        ALLOW_READ_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_write",
        c"Requested write permission: off|*|comma-list",
        c"",
        ALLOW_WRITE_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_net",
        c"Requested network permission: off|*|comma-list",
        c"",
        ALLOW_NET_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_env",
        c"Requested environment permission: off|*|comma-list",
        c"",
        ALLOW_ENV_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_run",
        c"Requested subprocess permission: off|*|comma-list",
        c"",
        ALLOW_RUN_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_ffi",
        c"Requested FFI permission: off|*|comma-list",
        c"",
        ALLOW_FFI_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_sys",
        c"Requested system-information permission: off|*|comma-list",
        c"",
        ALLOW_SYS_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_import",
        c"Requested import permission: off|*|comma-list",
        c"",
        ALLOW_IMPORT_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.allow_pg_execute",
        c"Requested access to _pg.execute(): off|on",
        c"",
        ALLOW_PG_EXECUTE_GUC.inner(),
        GucContext::Userset,
        GucFlags::default(),
    );

    GucRegistry::define_string_guc(
        c"typescript.max_allow_read",
        c"Superuser max read permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_READ_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_write",
        c"Superuser max write permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_WRITE_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_net",
        c"Superuser max network permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_NET_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_env",
        c"Superuser max environment permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_ENV_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_run",
        c"Superuser max subprocess permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_RUN_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_ffi",
        c"Superuser max FFI permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_FFI_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_sys",
        c"Superuser max system-information permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_SYS_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_import",
        c"Superuser max import permission cap: off|*|comma-list",
        c"",
        MAX_ALLOW_IMPORT_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_string_guc(
        c"typescript.max_allow_pg_execute",
        c"Superuser max _pg.execute() cap: off|on",
        c"",
        MAX_ALLOW_PG_EXECUTE_GUC.inner(),
        GucContext::Suset,
        GucFlags::default(),
    );

    // Don't initialize V8 in the postmaster process.
    if unsafe { pg_sys::IsUnderPostmaster } {
        runtime::prewarm_runtime();
    }
}

// Internal schema, module cache table, and cleanup trigger.
pgrx::extension_sql!(
    r#"
    CREATE SCHEMA deno_internal;
    REVOKE ALL ON SCHEMA deno_internal FROM PUBLIC;

    CREATE TABLE deno_internal.deno_package_modules (
        function_oid  oid  NOT NULL,
        url           text NOT NULL,
        source        text NOT NULL,
        PRIMARY KEY (function_oid, url)
    );
    REVOKE ALL ON deno_internal.deno_package_modules FROM PUBLIC;
    -- Runtime function execution needs read-only access to cached module source.
    -- Keep table-level SELECT for language execution, but gate row visibility so
    -- users can only see cache rows for functions they can execute.
    GRANT USAGE ON SCHEMA deno_internal TO PUBLIC;
    GRANT SELECT ON deno_internal.deno_package_modules TO PUBLIC;
    ALTER TABLE deno_internal.deno_package_modules ENABLE ROW LEVEL SECURITY;
    CREATE POLICY deno_package_modules_select_if_executable
      ON deno_internal.deno_package_modules
      FOR SELECT
      USING (pg_catalog.has_function_privilege(function_oid, 'EXECUTE'));

    CREATE OR REPLACE FUNCTION deno_internal.cleanup_modules()
    RETURNS event_trigger LANGUAGE plpgsql AS $$
    DECLARE obj record;
    BEGIN
        FOR obj IN
            SELECT objid
            FROM pg_event_trigger_dropped_objects()
            WHERE object_type IN ('function', 'routine', 'procedure')
        LOOP
            DELETE FROM deno_internal.deno_package_modules
            WHERE function_oid = obj.objid;
        END LOOP;
    END;
    $$;

    CREATE EVENT TRIGGER typescript_drop_cleanup
        ON sql_drop
        WHEN TAG IN ('DROP FUNCTION', 'DROP ROUTINE')
        EXECUTE FUNCTION deno_internal.cleanup_modules();
    "#,
    name = "create_internal_schema",
    bootstrap,
);

// Handler function stubs + language registration (must come after pgrx-generated stubs).
pgrx::extension_sql!(
    r#"
    CREATE FUNCTION typescript_call_handler()
      RETURNS language_handler
      LANGUAGE C STRICT
      AS 'MODULE_PATHNAME', 'typescript_call_handler';

    CREATE FUNCTION typescript_inline_handler(internal)
      RETURNS void
      LANGUAGE C STRICT
      AS 'MODULE_PATHNAME', 'typescript_inline_handler';

    CREATE FUNCTION typescript_validator(oid)
      RETURNS void
      LANGUAGE C STRICT
      AS 'MODULE_PATHNAME', 'typescript_validator';

    CREATE TRUSTED LANGUAGE typescript
      HANDLER   typescript_call_handler
      INLINE    typescript_inline_handler
      VALIDATOR typescript_validator;
    "#,
    name = "register_language",
    finalize,
);

#[cfg(any(test, feature = "pg_test"))]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
