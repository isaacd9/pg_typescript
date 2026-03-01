use pgrx::prelude::*;

pgrx::pg_module_magic!(name, version);

mod convert;
mod fetch;
mod loader;
mod plhandler;
mod runtime;

// Register the GUC for per-function import maps.
#[pg_guard]
pub unsafe extern "C-unwind" fn _PG_init() {
    use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};

    static IMPORT_MAP_GUC: GucSetting<Option<std::ffi::CString>> =
        GucSetting::<Option<std::ffi::CString>>::new(None);

    GucRegistry::define_string_guc(
        c"typescript.import_map",
        c"Deno-style import map JSON for pg_typescript functions, e.g. {\"imports\":{\"lodash\":\"https://esm.sh/lodash@4.17.23\"}}",
        c"",
        &IMPORT_MAP_GUC,
        GucContext::Userset,
        GucFlags::default(),
    );
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
