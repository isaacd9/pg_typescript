use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::ops::Deref;

use pgrx::guc::GucSetting;
use serde_json::Value;

pub(crate) struct StringGuc {
    inner: GucSetting<Option<CString>>,
}

impl StringGuc {
    pub(crate) const fn new() -> Self {
        Self {
            inner: GucSetting::<Option<CString>>::new(None),
        }
    }

    fn get(&self) -> Option<CString> {
        self.inner.get()
    }
}

impl Deref for StringGuc {
    type Target = GucSetting<Option<CString>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub(crate) trait GucParser {
    type Output;

    fn inner(&self) -> &StringGuc;
    fn default_value(&self) -> Self::Output;
    fn parse_nonempty(&self, raw: &str, source: &str) -> Result<Self::Output, String>;

    fn parse_setting(&self, source: &str) -> Result<Self::Output, String> {
        self.parse_cstring(self.inner().get(), source)
    }

    fn parse_cstring(&self, raw: Option<CString>, source: &str) -> Result<Self::Output, String> {
        let raw = raw.and_then(|cstr| cstr.to_str().ok().map(|s| s.to_string()));
        self.parse_raw(raw, source)
    }

    fn parse_raw(&self, raw: Option<String>, source: &str) -> Result<Self::Output, String> {
        match normalize_raw(raw) {
            Some(value) => self.parse_nonempty(&value, source),
            None => Ok(self.default_value()),
        }
    }
}

fn normalize_raw(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub(crate) struct BoolGucParser {
    inner: StringGuc,
}

impl BoolGucParser {
    pub(crate) const fn new() -> Self {
        Self {
            inner: StringGuc::new(),
        }
    }
}

impl GucParser for BoolGucParser {
    type Output = bool;

    fn inner(&self) -> &StringGuc {
        &self.inner
    }

    fn default_value(&self) -> Self::Output {
        false
    }

    fn parse_nonempty(&self, value: &str, source: &str) -> Result<Self::Output, String> {
        let normalized = value.to_ascii_lowercase();
        match normalized.as_str() {
            "off" | "none" | "deny" | "false" | "0" => Ok(false),
            "*" | "all" | "allow" | "on" | "true" | "1" => Ok(true),
            _ => Err(format!(
                "invalid boolean setting '{value}' in {source}; expected on/off"
            )),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum PermissionSetting {
    #[default]
    Deny,
    AllowAll,
    AllowList(Vec<String>),
}

pub(crate) struct PermissionParser {
    inner: StringGuc,
}

impl PermissionParser {
    pub(crate) const fn new() -> Self {
        Self {
            inner: StringGuc::new(),
        }
    }
}

impl GucParser for PermissionParser {
    type Output = PermissionSetting;

    fn inner(&self) -> &StringGuc {
        &self.inner
    }

    fn default_value(&self) -> Self::Output {
        PermissionSetting::Deny
    }

    fn parse_nonempty(&self, value: &str, source: &str) -> Result<Self::Output, String> {
        let normalized = value.to_ascii_lowercase();
        match normalized.as_str() {
            "off" | "none" | "deny" | "false" | "0" => Ok(PermissionSetting::Deny),
            "*" | "all" | "on" | "true" | "1" => Ok(PermissionSetting::AllowAll),
            _ => parse_permission_list(value, source).map(PermissionSetting::AllowList),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ImportUrlCap {
    Deny,
    #[default]
    AllowAll,
    AllowList(Vec<String>),
}

pub(crate) struct MaxImportsParser {
    inner: StringGuc,
}

impl MaxImportsParser {
    pub(crate) const fn new() -> Self {
        Self {
            inner: StringGuc::new(),
        }
    }
}

impl GucParser for MaxImportsParser {
    type Output = ImportUrlCap;

    fn inner(&self) -> &StringGuc {
        &self.inner
    }

    fn default_value(&self) -> Self::Output {
        ImportUrlCap::AllowAll
    }

    fn parse_nonempty(&self, value: &str, source: &str) -> Result<Self::Output, String> {
        let normalized = value.to_ascii_lowercase();
        match normalized.as_str() {
            "off" | "none" | "deny" | "false" | "0" => Ok(ImportUrlCap::Deny),
            "*" | "all" | "on" | "true" | "1" => Ok(ImportUrlCap::AllowAll),
            _ => parse_import_prefix_list(value, source).map(ImportUrlCap::AllowList),
        }
    }
}

pub(crate) fn import_url_allowed(url: &str, cap: &ImportUrlCap) -> Result<bool, String> {
    match cap {
        ImportUrlCap::AllowAll => Ok(true),
        ImportUrlCap::Deny => Ok(false),
        ImportUrlCap::AllowList(prefixes) => {
            let normalized = normalize_http_url(url, "import URL")?;
            Ok(prefixes.iter().any(|prefix| normalized.starts_with(prefix)))
        }
    }
}

pub(crate) struct ImportMapParser {
    inner: StringGuc,
}

impl ImportMapParser {
    pub(crate) const fn new() -> Self {
        Self {
            inner: StringGuc::new(),
        }
    }
}

impl GucParser for ImportMapParser {
    type Output = HashMap<String, String>;

    fn inner(&self) -> &StringGuc {
        &self.inner
    }

    fn default_value(&self) -> Self::Output {
        HashMap::new()
    }

    fn parse_nonempty(&self, raw: &str, _source: &str) -> Result<Self::Output, String> {
        parse_import_map_json(raw)
    }
}

fn parse_permission_list(value: &str, source: &str) -> Result<Vec<String>, String> {
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
        return Err(format!("invalid empty permission list in {source}"));
    }

    Ok(out)
}

fn parse_import_prefix_list(value: &str, source: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for raw in value.split(',') {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        let normalized = normalize_http_url(item, source)?;
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }

    if out.is_empty() {
        return Err(format!("invalid empty max_imports list in {source}"));
    }

    Ok(out)
}

fn parse_import_map_json(json: &str) -> Result<HashMap<String, String>, String> {
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

fn normalize_http_url(url: &str, source: &str) -> Result<String, String> {
    use deno_core::ModuleSpecifier;
    let parsed =
        ModuleSpecifier::parse(url).map_err(|e| format!("invalid URL '{url}' in {source}: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        scheme => Err(format!(
            "invalid URL '{url}' in {source}: unsupported scheme '{scheme}' (only http/https allowed)"
        )),
    }
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
