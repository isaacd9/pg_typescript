use pgrx::prelude::*;

pgrx::pg_module_magic!(name, version);

mod convert;
mod plhandler;
mod resolve;
mod runtime;

// Register the GUC for version pinning on shared-library load.
#[pg_guard]
pub unsafe extern "C-unwind" fn _PG_init() {
    use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};

    // `GucSetting` for strings must be `Option<CString>`.
    static IMPORTS_GUC: GucSetting<Option<std::ffi::CString>> =
        GucSetting::<Option<std::ffi::CString>>::new(None);

    GucRegistry::define_string_guc(
        c"typescript.imports",
        c"JSON object mapping package names to pinned versions, e.g. {\"zod\": \"3.22.0\"}",
        c"",
        &IMPORTS_GUC,
        GucContext::Userset,
        GucFlags::default(),
    );
}

// SQL that runs last (after pgrx-generated stubs) to register the PL.
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
