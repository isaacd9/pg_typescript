use std::collections::HashSet;

use deno_core::url::Url;
use deno_runtime::deno_permissions::{
    PermissionDescriptorParser, QueryDescriptor, RuntimePermissionDescriptorParser,
};
use pgrx::pg_catalog::pg_proc::PgProc;

use crate::guc::{GucParser, PermissionSetting};
use crate::runtime::RuntimePermissions;

type PermissionValue = PermissionSetting;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PermissionSpec {
    read: PermissionValue,
    write: PermissionValue,
    net: PermissionValue,
    env: PermissionValue,
    run: PermissionValue,
    ffi: PermissionValue,
    sys: PermissionValue,
    import: PermissionValue,
}

pub(crate) fn read_function_config(proc: &PgProc, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    proc.proconfig()
        .unwrap_or_default()
        .into_iter()
        .find_map(|kv| kv.strip_prefix(&prefix).map(|v| v.to_string()))
}

fn resolve_permissions(requested: PermissionSpec, prefix: &str) -> Result<RuntimePermissions, String> {
    let max = read_max_permissions()?;
    enforce_all_caps(&requested, &max, prefix)?;
    Ok(effective_permissions(requested, max))
}

macro_rules! read_spec_from_proc {
    ($proc:expr, $( $field:ident => $guc:expr, $key:literal ),+ $(,)?) => {
        PermissionSpec {
            $( $field: $guc.parse_raw(
                read_function_config($proc, $key),
                concat!("function setting ", $key),
            )?, )+
        }
    };
}

macro_rules! read_spec_from_session {
    ($( $field:ident => $guc:expr, $key:literal ),+ $(,)?) => {
        PermissionSpec {
            $( $field: $guc.parse_setting(concat!("GUC ", $key))?, )+
        }
    };
}

pub(crate) fn read_function_permissions(proc: &PgProc) -> RuntimePermissions {
    read_function_permissions_result(proc).unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
}

fn read_function_permissions_result(proc: &PgProc) -> Result<RuntimePermissions, String> {
    let requested = read_spec_from_proc!(proc,
        read   => crate::ALLOW_READ_GUC,   "typescript.allow_read",
        write  => crate::ALLOW_WRITE_GUC,  "typescript.allow_write",
        net    => crate::ALLOW_NET_GUC,    "typescript.allow_net",
        env    => crate::ALLOW_ENV_GUC,    "typescript.allow_env",
        run    => crate::ALLOW_RUN_GUC,    "typescript.allow_run",
        ffi    => crate::ALLOW_FFI_GUC,    "typescript.allow_ffi",
        sys    => crate::ALLOW_SYS_GUC,    "typescript.allow_sys",
        import => crate::ALLOW_IMPORT_GUC, "typescript.allow_import",
    );
    resolve_permissions(requested, "function setting typescript")
}

pub(crate) fn read_inline_permissions() -> RuntimePermissions {
    read_inline_permissions_result().unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
}

fn read_inline_permissions_result() -> Result<RuntimePermissions, String> {
    let requested = read_spec_from_session!(
        read   => crate::ALLOW_READ_GUC,   "typescript.allow_read",
        write  => crate::ALLOW_WRITE_GUC,  "typescript.allow_write",
        net    => crate::ALLOW_NET_GUC,    "typescript.allow_net",
        env    => crate::ALLOW_ENV_GUC,    "typescript.allow_env",
        run    => crate::ALLOW_RUN_GUC,    "typescript.allow_run",
        ffi    => crate::ALLOW_FFI_GUC,    "typescript.allow_ffi",
        sys    => crate::ALLOW_SYS_GUC,    "typescript.allow_sys",
        import => crate::ALLOW_IMPORT_GUC, "typescript.allow_import",
    );
    resolve_permissions(requested, "GUC typescript")
}

pub(crate) fn read_function_pg_execute(proc: &PgProc) -> bool {
    read_function_pg_execute_result(proc).unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
}

pub(crate) fn import_allowed(url: &str, allow_import: Option<&[String]>) -> Result<bool, String> {
    let Some(allow_import) = allow_import else {
        return Ok(false);
    };

    if allow_import.is_empty() {
        return Ok(true);
    }

    let parser = RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys);
    let url =
        Url::parse(url).map_err(|e| format!("invalid import URL '{url}' in import_map: {e}"))?;
    let requested = parser
        .parse_import_descriptor_from_url(&url)
        .map_err(|e| format!("invalid import URL '{url}' in import_map: {e}"))?;

    for entry in allow_import {
        let allowed = parser.parse_import_descriptor(entry).map_err(|e| {
            format!("invalid import permission value '{entry}' in typescript.allow_import: {e}")
        })?;
        if requested.matches_allow(&allowed) {
            return Ok(true);
        }
    }

    Ok(false)
}

fn read_function_pg_execute_result(proc: &PgProc) -> Result<bool, String> {
    let requested = crate::ALLOW_PG_EXECUTE_GUC.parse_raw(
        read_function_config(proc, "typescript.allow_pg_execute"),
        "function setting typescript.allow_pg_execute",
    )?;
    let max = read_max_pg_execute()?;
    enforce_pg_execute_cap(
        requested,
        max,
        "function setting typescript.allow_pg_execute",
        "GUC typescript.max_allow_pg_execute",
    )?;
    Ok(requested && max)
}

pub(crate) fn read_inline_pg_execute() -> bool {
    read_inline_pg_execute_result().unwrap_or_else(|e| pgrx::error!("pg_typescript: {e}"))
}

fn read_inline_pg_execute_result() -> Result<bool, String> {
    let requested = crate::ALLOW_PG_EXECUTE_GUC.parse_setting("GUC typescript.allow_pg_execute")?;
    let max = read_max_pg_execute()?;
    enforce_pg_execute_cap(
        requested,
        max,
        "GUC typescript.allow_pg_execute",
        "GUC typescript.max_allow_pg_execute",
    )?;
    Ok(requested && max)
}

fn read_max_permissions() -> Result<PermissionSpec, String> {
    Ok(PermissionSpec {
        read: crate::MAX_ALLOW_READ_GUC.parse_setting("GUC typescript.max_allow_read")?,
        write: crate::MAX_ALLOW_WRITE_GUC.parse_setting("GUC typescript.max_allow_write")?,
        net: crate::MAX_ALLOW_NET_GUC.parse_setting("GUC typescript.max_allow_net")?,
        env: crate::MAX_ALLOW_ENV_GUC.parse_setting("GUC typescript.max_allow_env")?,
        run: crate::MAX_ALLOW_RUN_GUC.parse_setting("GUC typescript.max_allow_run")?,
        ffi: crate::MAX_ALLOW_FFI_GUC.parse_setting("GUC typescript.max_allow_ffi")?,
        sys: crate::MAX_ALLOW_SYS_GUC.parse_setting("GUC typescript.max_allow_sys")?,
        import: crate::MAX_ALLOW_IMPORT_GUC.parse_setting("GUC typescript.max_allow_import")?,
    })
}

fn read_max_pg_execute() -> Result<bool, String> {
    crate::MAX_ALLOW_PG_EXECUTE_GUC.parse_setting("GUC typescript.max_allow_pg_execute")
}

fn effective_permissions(requested: PermissionSpec, max: PermissionSpec) -> RuntimePermissions {
    RuntimePermissions {
        allow_read: to_runtime_allowlist(intersect_permission(requested.read, max.read)),
        allow_write: to_runtime_allowlist(intersect_permission(requested.write, max.write)),
        allow_net: to_runtime_allowlist(intersect_permission(requested.net, max.net)),
        allow_env: to_runtime_allowlist(intersect_permission(requested.env, max.env)),
        allow_run: to_runtime_allowlist(intersect_permission(requested.run, max.run)),
        allow_ffi: to_runtime_allowlist(intersect_permission(requested.ffi, max.ffi)),
        allow_sys: to_runtime_allowlist(intersect_permission(requested.sys, max.sys)),
        allow_import: to_runtime_allowlist(intersect_permission(requested.import, max.import)),
    }
}

fn enforce_pg_execute_cap(
    requested: bool,
    max: bool,
    requested_source: &str,
    max_source: &str,
) -> Result<(), String> {
    if requested && !max {
        return Err(format!(
            "{requested_source} cannot be fulfilled by {max_source}: requested 'on' but cap is 'off'"
        ));
    }
    Ok(())
}

fn enforce_permission_cap(
    requested: &PermissionValue,
    max: &PermissionValue,
    requested_source: &str,
    max_source: &str,
) -> Result<(), String> {
    if let Some(detail) = unfulfillable_detail(requested, max) {
        return Err(format!(
            "{requested_source} cannot be fulfilled by {max_source}: {detail}"
        ));
    }
    Ok(())
}

fn enforce_all_caps(
    requested: &PermissionSpec,
    max: &PermissionSpec,
    requested_prefix: &str,
) -> Result<(), String> {
    let fields: &[(&PermissionValue, &PermissionValue, &str)] = &[
        (&requested.read, &max.read, "allow_read"),
        (&requested.write, &max.write, "allow_write"),
        (&requested.net, &max.net, "allow_net"),
        (&requested.env, &max.env, "allow_env"),
        (&requested.run, &max.run, "allow_run"),
        (&requested.ffi, &max.ffi, "allow_ffi"),
        (&requested.sys, &max.sys, "allow_sys"),
        (&requested.import, &max.import, "allow_import"),
    ];
    for (req, cap, name) in fields {
        enforce_permission_cap(
            req,
            cap,
            &format!("{requested_prefix}.{name}"),
            &format!("GUC typescript.max_{name}"),
        )?;
    }
    Ok(())
}

fn unfulfillable_detail(requested: &PermissionValue, max: &PermissionValue) -> Option<String> {
    match requested {
        PermissionValue::Deny => None,
        PermissionValue::AllowAll => match max {
            PermissionValue::AllowAll => None,
            PermissionValue::Deny => Some("requested '*' but cap is 'off'".to_string()),
            PermissionValue::AllowList(max_list) => Some(format!(
                "requested '*' but cap only allows {}",
                format_permission_values(max_list),
            )),
        },
        PermissionValue::AllowList(req_list) => match max {
            PermissionValue::AllowAll => None,
            PermissionValue::Deny => Some(format!(
                "requested {} but cap is 'off'",
                format_permission_values(req_list),
            )),
            PermissionValue::AllowList(max_list) => {
                let cap: HashSet<&str> = max_list.iter().map(String::as_str).collect();
                let mut disallowed = Vec::new();

                for item in req_list {
                    if !cap.contains(item.as_str()) {
                        disallowed.push(item.clone());
                    }
                }

                if disallowed.is_empty() {
                    None
                } else {
                    Some(format!(
                        "requested {} includes disallowed values {}",
                        format_permission_values(req_list),
                        format_permission_values(&disallowed),
                    ))
                }
            }
        },
    }
}

fn format_permission_values(values: &[String]) -> String {
    format!("[{}]", values.join(","))
}

fn intersect_permission(requested: PermissionValue, max: PermissionValue) -> PermissionValue {
    match max {
        PermissionValue::Deny => PermissionValue::Deny,
        PermissionValue::AllowAll => requested,
        PermissionValue::AllowList(max_list) => match requested {
            PermissionValue::Deny => PermissionValue::Deny,
            PermissionValue::AllowAll => PermissionValue::AllowList(max_list),
            PermissionValue::AllowList(req_list) => {
                let cap: HashSet<String> = max_list.into_iter().collect();
                let mut out = Vec::new();
                for item in req_list {
                    if cap.contains(&item) {
                        out.push(item);
                    }
                }
                if out.is_empty() {
                    PermissionValue::Deny
                } else {
                    PermissionValue::AllowList(out)
                }
            }
        },
    }
}

fn to_runtime_allowlist(value: PermissionValue) -> Option<Vec<String>> {
    match value {
        PermissionValue::AllowAll => Some(vec![]),
        PermissionValue::AllowList(values) => Some(values),
        PermissionValue::Deny => None,
    }
}

#[cfg(all(test, not(feature = "pg_test")))]
mod unit_tests {
    use super::*;
    use crate::guc::PermissionParser;

    fn allow_list(values: &[&str]) -> PermissionValue {
        PermissionValue::AllowList(values.iter().map(|v| (*v).to_string()).collect())
    }

    fn parse_setting_for_test(raw: Option<String>) -> PermissionValue {
        PermissionParser::new().parse_raw(raw, "test").unwrap()
    }

    #[test]
    fn import_allowed_denies_when_unset() {
        assert!(!super::import_allowed("https://esm.sh/lodash@4", None)
            .expect("unset allow_import should parse"));
    }

    #[test]
    fn import_allowed_matches_host_allowlist() {
        let allow_import = vec!["esm.sh".to_string()];
        assert!(
            super::import_allowed("https://esm.sh/lodash@4", Some(&allow_import))
                .expect("host allowlist should parse")
        );
        assert!(!super::import_allowed(
            "https://deno.land/std@0.224.0/mod.ts",
            Some(&allow_import)
        )
        .expect("non-matching host should parse"));
    }

    #[test]
    fn intersect_max_deny_overrides_everything() {
        assert_eq!(
            intersect_permission(PermissionValue::AllowAll, PermissionValue::Deny),
            PermissionValue::Deny
        );
        assert_eq!(
            intersect_permission(allow_list(&["PATH"]), PermissionValue::Deny),
            PermissionValue::Deny
        );
    }

    #[test]
    fn intersect_max_allows_all_preserves_requested() {
        assert_eq!(
            intersect_permission(PermissionValue::AllowAll, PermissionValue::AllowAll),
            PermissionValue::AllowAll
        );
        assert_eq!(
            intersect_permission(allow_list(&["PATH", "USER"]), PermissionValue::AllowAll),
            allow_list(&["PATH", "USER"])
        );
    }

    #[test]
    fn intersect_allow_all_with_cap_list_becomes_cap_list() {
        assert_eq!(
            intersect_permission(PermissionValue::AllowAll, allow_list(&["PATH", "HOME"]),),
            allow_list(&["PATH", "HOME"])
        );
    }

    #[test]
    fn intersect_list_with_cap_list_keeps_overlap_in_request_order() {
        assert_eq!(
            intersect_permission(
                allow_list(&["USER", "PATH", "SHELL"]),
                allow_list(&["PATH", "HOME", "USER"]),
            ),
            allow_list(&["USER", "PATH"])
        );
    }

    #[test]
    fn intersect_list_with_no_overlap_becomes_deny() {
        assert_eq!(
            intersect_permission(allow_list(&["USER"]), allow_list(&["PATH"])),
            PermissionValue::Deny
        );
    }

    #[test]
    fn parse_setting_understands_aliases_and_dedupes_lists() {
        assert_eq!(
            parse_setting_for_test(Some("on".to_string())),
            PermissionValue::AllowAll
        );
        assert_eq!(
            parse_setting_for_test(Some("none".to_string())),
            PermissionValue::Deny
        );
        assert_eq!(
            parse_setting_for_test(Some(" PATH , PATH , USER ".to_string())),
            allow_list(&["PATH", "USER"])
        );
    }

    #[test]
    fn effective_permissions_enforces_cap() {
        let requested = PermissionSpec {
            env: allow_list(&["PATH"]),
            net: allow_list(&["internal"]),
            ..Default::default()
        };
        let max = PermissionSpec {
            env: allow_list(&["PATH", "USER"]),
            net: allow_list(&["internal"]),
            ..Default::default()
        };

        let out = effective_permissions(requested, max);
        assert_eq!(out.allow_env, Some(vec!["PATH".to_string()]));
        assert_eq!(out.allow_net, Some(vec!["internal".to_string()]));
        assert_eq!(out.allow_read, None);
    }

    #[test]
    fn unfulfillable_detail_rejects_wildcard_above_list_cap() {
        assert_eq!(
            unfulfillable_detail(&PermissionValue::AllowAll, &allow_list(&["PATH", "HOME"])),
            Some("requested '*' but cap only allows [PATH,HOME]".to_string())
        );
    }

    #[test]
    fn unfulfillable_detail_rejects_partial_overlap() {
        assert_eq!(
            unfulfillable_detail(
                &allow_list(&["USER", "PATH", "SHELL"]),
                &allow_list(&["PATH", "HOME", "USER"])
            ),
            Some("requested [USER,PATH,SHELL] includes disallowed values [SHELL]".to_string())
        );
    }

    #[test]
    fn unfulfillable_detail_accepts_subset() {
        assert_eq!(
            unfulfillable_detail(
                &allow_list(&["USER", "PATH"]),
                &allow_list(&["PATH", "USER"])
            ),
            None
        );
    }
}
