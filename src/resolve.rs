use std::borrow::Cow;
use std::collections::HashMap;

use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
    error::ModuleLoaderError,
};
use deno_error::JsErrorBox;

/// Module loader that resolves bare npm specifiers to esm.sh and fetches them.
pub struct EsmModuleLoader {
    /// Optional version pins: package name → version string.
    pub pins: HashMap<String, String>,
}

impl EsmModuleLoader {
    pub fn new() -> Self {
        Self { pins: HashMap::new() }
    }

    /// Resolve a bare specifier (e.g. `"zod"` or `"lodash/chunk"`) to a full
    /// esm.sh URL, honouring any version pins.
    pub fn resolve_to_url(&self, specifier: &str) -> String {
        if specifier.starts_with("http://") || specifier.starts_with("https://") {
            return specifier.to_string();
        }
        let base = specifier.split('/').next().unwrap_or(specifier);
        match self.pins.get(base) {
            Some(ver) => format!("https://esm.sh/{}@{}", specifier, ver),
            None => format!("https://esm.sh/{}", specifier),
        }
    }
}

impl ModuleLoader for EsmModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        if specifier.starts_with("http://") || specifier.starts_with("https://") {
            return ModuleSpecifier::parse(specifier)
                .map_err(|e| JsErrorBox::from_err(e));
        }
        if specifier.starts_with("./") || specifier.starts_with("../") {
            let base = ModuleSpecifier::parse(referrer)
                .unwrap_or_else(|_| ModuleSpecifier::parse("file:///").unwrap());
            return base.join(specifier).map_err(|e| JsErrorBox::from_err(e));
        }
        // Bare specifier → esm.sh.
        let url = self.resolve_to_url(specifier);
        ModuleSpecifier::parse(&url).map_err(|e| JsErrorBox::from_err(e))
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.as_str().to_string();

        ModuleLoadResponse::Sync(fetch_module(&url).map(|code| {
            ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(code.into()),
                module_specifier,
                None,
            )
        }))
    }

    fn get_source_map(&self, _specifier: &str) -> Option<Cow<'_, [u8]>> {
        None
    }
}

/// Synchronously fetch a URL's body as a string.
///
/// Uses `ureq` (pure Rust, no tokio dependency) so blocking inside a tokio
/// `block_on` context does not deadlock.
fn fetch_module(url: &str) -> Result<String, ModuleLoaderError> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| JsErrorBox::generic(format!("Failed to fetch {url}: {e}")))?;

    response
        .into_string()
        .map_err(|e| JsErrorBox::generic(format!("Failed to read response from {url}: {e}")))
}
