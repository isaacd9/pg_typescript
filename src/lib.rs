use pgrx::guc::{GucContext, GucFlags, GucRegistry};
use pgrx::prelude::*;

pgrx::pg_module_magic!(name, version);

mod convert;
mod extensions;
mod fetch;
mod guc;
mod loader;
mod permissions;
mod plhandler;
mod runtime;

use crate::guc::{BoolGucParser, GucParser, ImportMapParser, MaxImportsParser, PermissionParser};

// ---------------------------------------------------------------------------
// GUC statics
// ---------------------------------------------------------------------------

pub(crate) static IMPORT_MAP_GUC: ImportMapParser = ImportMapParser::new();
pub(crate) static MAX_IMPORTS_GUC: MaxImportsParser = MaxImportsParser::new();

pub(crate) static ALLOW_READ_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_WRITE_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_NET_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_ENV_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_RUN_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_FFI_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_SYS_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_IMPORT_GUC: PermissionParser = PermissionParser::new();
pub(crate) static ALLOW_PG_EXECUTE_GUC: BoolGucParser = BoolGucParser::new();

pub(crate) static MAX_ALLOW_READ_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_WRITE_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_NET_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_ENV_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_RUN_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_FFI_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_SYS_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_IMPORT_GUC: PermissionParser = PermissionParser::new();
pub(crate) static MAX_ALLOW_PG_EXECUTE_GUC: BoolGucParser = BoolGucParser::new();

// ---------------------------------------------------------------------------
// GUC registration
// ---------------------------------------------------------------------------

macro_rules! register_guc {
    ($name:expr, $desc:expr, $guc:expr, $ctx:expr) => {
        GucRegistry::define_string_guc($name, $desc, c"", $guc.inner(), $ctx, GucFlags::default());
    };
}

#[pg_guard]
pub unsafe extern "C-unwind" fn _PG_init() {
    register_guc!(c"typescript.import_map",
        c"Deno-style import map JSON for pg_typescript functions",
        IMPORT_MAP_GUC, GucContext::Userset);
    register_guc!(c"typescript.max_imports",
        c"Superuser max import URL cap: off|*|comma-list of http(s) URL prefixes",
        MAX_IMPORTS_GUC, GucContext::Suset);

    // Userset allow_* permission knobs (off|*|comma-list, default deny).
    register_guc!(c"typescript.allow_read",       c"Requested read permission: off|*|comma-list",               ALLOW_READ_GUC,       GucContext::Userset);
    register_guc!(c"typescript.allow_write",      c"Requested write permission: off|*|comma-list",              ALLOW_WRITE_GUC,      GucContext::Userset);
    register_guc!(c"typescript.allow_net",        c"Requested network permission: off|*|comma-list",            ALLOW_NET_GUC,        GucContext::Userset);
    register_guc!(c"typescript.allow_env",        c"Requested environment permission: off|*|comma-list",        ALLOW_ENV_GUC,        GucContext::Userset);
    register_guc!(c"typescript.allow_run",        c"Requested subprocess permission: off|*|comma-list",         ALLOW_RUN_GUC,        GucContext::Userset);
    register_guc!(c"typescript.allow_ffi",        c"Requested FFI permission: off|*|comma-list",                ALLOW_FFI_GUC,        GucContext::Userset);
    register_guc!(c"typescript.allow_sys",        c"Requested system-information permission: off|*|comma-list", ALLOW_SYS_GUC,        GucContext::Userset);
    register_guc!(c"typescript.allow_import",     c"Requested import permission: off|*|comma-list",             ALLOW_IMPORT_GUC,     GucContext::Userset);
    register_guc!(c"typescript.allow_pg_execute", c"Requested access to _pg.execute(): off|on",                 ALLOW_PG_EXECUTE_GUC, GucContext::Userset);

    // Superuser max_allow_* caps (off|*|comma-list, default deny).
    register_guc!(c"typescript.max_allow_read",       c"Superuser max read permission cap: off|*|comma-list",               MAX_ALLOW_READ_GUC,       GucContext::Suset);
    register_guc!(c"typescript.max_allow_write",      c"Superuser max write permission cap: off|*|comma-list",              MAX_ALLOW_WRITE_GUC,      GucContext::Suset);
    register_guc!(c"typescript.max_allow_net",        c"Superuser max network permission cap: off|*|comma-list",            MAX_ALLOW_NET_GUC,        GucContext::Suset);
    register_guc!(c"typescript.max_allow_env",        c"Superuser max environment permission cap: off|*|comma-list",        MAX_ALLOW_ENV_GUC,        GucContext::Suset);
    register_guc!(c"typescript.max_allow_run",        c"Superuser max subprocess permission cap: off|*|comma-list",         MAX_ALLOW_RUN_GUC,        GucContext::Suset);
    register_guc!(c"typescript.max_allow_ffi",        c"Superuser max FFI permission cap: off|*|comma-list",                MAX_ALLOW_FFI_GUC,        GucContext::Suset);
    register_guc!(c"typescript.max_allow_sys",        c"Superuser max system-information permission cap: off|*|comma-list", MAX_ALLOW_SYS_GUC,        GucContext::Suset);
    register_guc!(c"typescript.max_allow_import",     c"Superuser max import permission cap: off|*|comma-list",             MAX_ALLOW_IMPORT_GUC,     GucContext::Suset);
    register_guc!(c"typescript.max_allow_pg_execute", c"Superuser max _pg.execute() cap: off|on",                           MAX_ALLOW_PG_EXECUTE_GUC, GucContext::Suset);

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
