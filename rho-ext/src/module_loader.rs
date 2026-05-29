//! Custom [`ModuleLoader`] for rho extensions.
//!
//! [`RhoModuleLoader`] resolves and loads ES module imports within an
//! extension's directory. It enforces path sandboxing (no imports outside the
//! extension root) and transpiles `.ts`/`.tsx`/`.mts`/`.cts` files to JavaScript.
//!
//! # Security
//!
//! Only `file://` URLs whose canonical paths fall within the extension's root
//! directory are accepted. Imports outside that boundary produce a load error.
//!
//! # Threading
//!
//! `RhoModuleLoader` is `!Send` (it uses `Rc` through `ModuleLoader`). This is
//! fine — it lives on the extension thread alongside `JsRuntime`.

use std::path::PathBuf;

use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleResolveResponse,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind, resolve_import,
};
use deno_error::JsErrorBox;
use url::Url;

use crate::transpile::transpile;

// ── RhoModuleLoader ──────────────────────────────────────────────────────────

/// A module loader that resolves imports relative to an extension's root
/// directory, transpiles TypeScript, and enforces path sandboxing.
pub struct RhoModuleLoader {
    /// Root directory for path sandboxing.
    root_dir: PathBuf,
}

impl RhoModuleLoader {
    /// Create a new loader scoped to `root_dir`.
    ///
    /// Only modules whose canonical file path starts with `root_dir` (after
    /// symlink resolution) will be loaded.
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }
}

impl ModuleLoader for RhoModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> ModuleResolveResponse {
        resolve_import(specifier, referrer).map_err(JsErrorBox::from_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        // Convert URL → file path
        let path = match specifier_to_path(module_specifier) {
            Ok(p) => p,
            Err(e) => {
                return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                    "Cannot resolve module path '{module_specifier}': {e}"
                ))));
            }
        };

        // Sandbox: canonicalize and check the path is within root_dir
        let canonical_path = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                    "Cannot access module path '{}': {e}",
                    path.display()
                ))));
            }
        };

        let canonical_root = match self.root_dir.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                    "Invalid extension root directory '{}': {e}",
                    self.root_dir.display()
                ))));
            }
        };

        if !canonical_path.starts_with(&canonical_root) {
            return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                "Import '{module_specifier}' is outside the extension directory"
            ))));
        }

        // Read source from disk
        let source = match std::fs::read_to_string(&canonical_path) {
            Ok(s) => s,
            Err(e) => {
                return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                    "Failed to read module '{module_specifier}': {e}"
                ))));
            }
        };

        // Transpile TypeScript variants; pass through .js as-is
        let js_code = match canonical_path.extension().and_then(|e| e.to_str()) {
            Some("ts" | "tsx" | "mts" | "cts") => match transpile(module_specifier, &source) {
                Ok(js) => js,
                Err(e) => {
                    return ModuleLoadResponse::Sync(Err(JsErrorBox::generic(format!(
                        "Failed to transpile '{module_specifier}': {e}"
                    ))));
                }
            },
            _ => source,
        };

        let code = ModuleSourceCode::String(js_code.into());
        ModuleLoadResponse::Sync(Ok(ModuleSource::new(
            ModuleType::JavaScript,
            code,
            module_specifier,
            None,
        )))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Convert a `file://` URL to a `PathBuf`.
///
/// Uses `Url::to_file_path` which handles percent-decoding and platform
/// differences. Returns `Err` for non-`file://` URLs.
fn specifier_to_path(specifier: &Url) -> Result<PathBuf, String> {
    if specifier.scheme() != "file" {
        return Err(format!("unsupported scheme '{}'", specifier.scheme()));
    }
    specifier
        .to_file_path()
        .map_err(|()| format!("invalid file URL: {specifier}"))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative_import() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.ts");
        std::fs::write(&main_path, "").unwrap();

        let main_url = Url::from_file_path(&main_path).unwrap();
        let loader = RhoModuleLoader::new(dir.path().to_path_buf());

        let result = loader.resolve("./helper.ts", main_url.as_str(), ResolutionKind::Import);

        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert!(resolved.as_str().ends_with("helper.ts"));
    }

    #[test]
    fn load_transpiles_typescript() {
        let dir = tempfile::tempdir().unwrap();
        let helper_path = dir.path().join("helper.ts");
        std::fs::write(
            &helper_path,
            r"export function add(a: number, b: number): number { return a + b; }",
        )
        .unwrap();

        let helper_url = Url::from_file_path(&helper_path).unwrap();
        let loader = RhoModuleLoader::new(dir.path().to_path_buf());

        let result = loader.load(
            &helper_url,
            None,
            ModuleLoadOptions {
                is_dynamic_import: false,
                is_synchronous: false,
                requested_module_type: deno_core::RequestedModuleType::None,
            },
        );

        match result {
            ModuleLoadResponse::Sync(Ok(source)) => {
                // Should be JavaScript (transpiled)
                // Read the code out of the source
                assert!(matches!(source.module_type, ModuleType::JavaScript));
            }
            ModuleLoadResponse::Sync(Err(e)) => {
                panic!("load should succeed, got error: {e}");
            }
            ModuleLoadResponse::Async(_) => {
                panic!("expected Sync response, got Async");
            }
        }
    }

    #[test]
    fn load_rejects_path_outside_root() {
        let root_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();

        // Create a file outside root
        let outside_path = outside_dir.path().join("outside.ts");
        std::fs::write(&outside_path, r"export const x = 1;").unwrap();

        let outside_url = Url::from_file_path(&outside_path).unwrap();
        let loader = RhoModuleLoader::new(root_dir.path().to_path_buf());

        let result = loader.load(
            &outside_url,
            None,
            ModuleLoadOptions {
                is_dynamic_import: false,
                is_synchronous: false,
                requested_module_type: deno_core::RequestedModuleType::None,
            },
        );

        match result {
            ModuleLoadResponse::Sync(Err(e)) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("outside"),
                    "error should mention 'outside': {msg}"
                );
            }
            _ => panic!("load should fail for paths outside root"),
        }
    }

    #[test]
    fn load_passes_through_javascript() {
        let dir = tempfile::tempdir().unwrap();
        let js_path = dir.path().join("pure.js");
        std::fs::write(&js_path, r"export const x = 42;").unwrap();

        let js_url = Url::from_file_path(&js_path).unwrap();
        let loader = RhoModuleLoader::new(dir.path().to_path_buf());

        let result = loader.load(
            &js_url,
            None,
            ModuleLoadOptions {
                is_dynamic_import: false,
                is_synchronous: false,
                requested_module_type: deno_core::RequestedModuleType::None,
            },
        );

        assert!(
            matches!(result, ModuleLoadResponse::Sync(Ok(_))),
            "loading a .js file should succeed without transpilation"
        );
    }

    #[test]
    fn specifier_to_path_file_url() {
        let url = Url::parse("file:///tmp/test.ts").unwrap();
        let path = specifier_to_path(&url).unwrap();
        assert!(path.to_string_lossy().contains("test.ts"));
    }

    #[test]
    fn specifier_to_path_rejects_http() {
        let url = Url::parse("https://evil.com/steal.ts").unwrap();
        let result = specifier_to_path(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported scheme"));
    }
}
