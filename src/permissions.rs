use std::collections::HashSet;

use pgrx::pg_catalog::pg_proc::PgProc;

use crate::runtime::RuntimePermissions;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum PermissionValue {
    #[default]
    Deny,
    AllowAll,
    AllowList(Vec<String>),
}

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

fn read_function_config(proc: &PgProc, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    proc.proconfig()
        .unwrap_or_default()
        .into_iter()
        .find_map(|kv| kv.strip_prefix(&prefix).map(|v| v.to_string()))
}

pub(crate) fn read_function_permissions(proc: &PgProc) -> RuntimePermissions {
    let requested = PermissionSpec {
        read: parse_permission_setting(
            read_function_config(proc, "typescript.allow_read"),
            "function setting typescript.allow_read",
        ),
        write: parse_permission_setting(
            read_function_config(proc, "typescript.allow_write"),
            "function setting typescript.allow_write",
        ),
        net: parse_permission_setting(
            read_function_config(proc, "typescript.allow_net"),
            "function setting typescript.allow_net",
        ),
        env: parse_permission_setting(
            read_function_config(proc, "typescript.allow_env"),
            "function setting typescript.allow_env",
        ),
        run: parse_permission_setting(
            read_function_config(proc, "typescript.allow_run"),
            "function setting typescript.allow_run",
        ),
        ffi: parse_permission_setting(
            read_function_config(proc, "typescript.allow_ffi"),
            "function setting typescript.allow_ffi",
        ),
        sys: parse_permission_setting(
            read_function_config(proc, "typescript.allow_sys"),
            "function setting typescript.allow_sys",
        ),
        import: parse_permission_setting(
            read_function_config(proc, "typescript.allow_import"),
            "function setting typescript.allow_import",
        ),
    };

    effective_permissions(requested, read_max_permissions())
}

pub(crate) fn read_inline_permissions() -> RuntimePermissions {
    let requested = PermissionSpec {
        read: parse_permission_setting(
            guc_value(crate::ALLOW_READ_GUC.get()),
            "GUC typescript.allow_read",
        ),
        write: parse_permission_setting(
            guc_value(crate::ALLOW_WRITE_GUC.get()),
            "GUC typescript.allow_write",
        ),
        net: parse_permission_setting(
            guc_value(crate::ALLOW_NET_GUC.get()),
            "GUC typescript.allow_net",
        ),
        env: parse_permission_setting(
            guc_value(crate::ALLOW_ENV_GUC.get()),
            "GUC typescript.allow_env",
        ),
        run: parse_permission_setting(
            guc_value(crate::ALLOW_RUN_GUC.get()),
            "GUC typescript.allow_run",
        ),
        ffi: parse_permission_setting(
            guc_value(crate::ALLOW_FFI_GUC.get()),
            "GUC typescript.allow_ffi",
        ),
        sys: parse_permission_setting(
            guc_value(crate::ALLOW_SYS_GUC.get()),
            "GUC typescript.allow_sys",
        ),
        import: parse_permission_setting(
            guc_value(crate::ALLOW_IMPORT_GUC.get()),
            "GUC typescript.allow_import",
        ),
    };

    effective_permissions(requested, read_max_permissions())
}

fn read_max_permissions() -> PermissionSpec {
    PermissionSpec {
        read: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_READ_GUC.get()),
            "GUC typescript.max_allow_read",
        ),
        write: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_WRITE_GUC.get()),
            "GUC typescript.max_allow_write",
        ),
        net: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_NET_GUC.get()),
            "GUC typescript.max_allow_net",
        ),
        env: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_ENV_GUC.get()),
            "GUC typescript.max_allow_env",
        ),
        run: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_RUN_GUC.get()),
            "GUC typescript.max_allow_run",
        ),
        ffi: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_FFI_GUC.get()),
            "GUC typescript.max_allow_ffi",
        ),
        sys: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_SYS_GUC.get()),
            "GUC typescript.max_allow_sys",
        ),
        import: parse_permission_setting(
            guc_value(crate::MAX_ALLOW_IMPORT_GUC.get()),
            "GUC typescript.max_allow_import",
        ),
    }
}

fn guc_value(value: Option<std::ffi::CString>) -> Option<String> {
    value
        .and_then(|cstr| cstr.to_str().ok().map(|s| s.to_string()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_permission_setting(raw: Option<String>, source: &str) -> PermissionValue {
    let Some(value) = raw else {
        return PermissionValue::Deny;
    };

    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "off" | "none" | "deny" | "false" | "0" => PermissionValue::Deny,
        "*" | "all" | "on" | "true" | "1" => PermissionValue::AllowAll,
        _ => PermissionValue::AllowList(parse_permission_list(&value, source)),
    }
}

fn parse_permission_list(value: &str, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for raw in value.split(',') {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        if seen.insert(item.to_string()) {
            out.push(item.to_string());
        }
    }

    if out.is_empty() {
        pgrx::error!("pg_typescript: invalid empty permission list in {source}");
    }

    out
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
                let mut seen = HashSet::new();
                for item in req_list {
                    if cap.contains(&item) && seen.insert(item.clone()) {
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

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn allow_list(values: &[&str]) -> PermissionValue {
        PermissionValue::AllowList(values.iter().map(|v| (*v).to_string()).collect())
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
            parse_permission_setting(Some("on".to_string()), "test"),
            PermissionValue::AllowAll
        );
        assert_eq!(
            parse_permission_setting(Some("none".to_string()), "test"),
            PermissionValue::Deny
        );
        assert_eq!(
            parse_permission_setting(Some(" PATH , PATH , USER ".to_string()), "test"),
            allow_list(&["PATH", "USER"])
        );
    }

    #[test]
    fn effective_permissions_enforces_cap() {
        let requested = PermissionSpec {
            env: PermissionValue::AllowAll,
            net: allow_list(&["example.com", "internal"]),
            ..Default::default()
        };
        let max = PermissionSpec {
            env: allow_list(&["PATH"]),
            net: allow_list(&["internal"]),
            ..Default::default()
        };

        let out = effective_permissions(requested, max);
        assert_eq!(out.allow_env, Some(vec!["PATH".to_string()]));
        assert_eq!(out.allow_net, Some(vec!["internal".to_string()]));
        assert_eq!(out.allow_read, None);
    }
}
