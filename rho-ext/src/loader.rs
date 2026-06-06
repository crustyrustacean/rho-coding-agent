//! Extension loader — manages the full extension lifecycle.
//!
//! [`ExtensionLoader`] orchestrates discovery, filtering, spawning, tool
//! registration, and hot reload. It owns the [`ExtensionRuntime`] instances
//! and tracks file mtimes for change detection.
//!
//! # Lifecycle
//!
//! ```text
//! ExtensionLoader::new(config, std::path::PathBuf::from("."))
//!   ├─ load_all(dirs)           → discover + filter + spawn
//! │   ├─ discover(dirs)
//! │   ├─ deduplicate()
//! │   ├─ filter_by_config()
//! │   └─ spawn_from_file() × N
//! │
//! ├─ register_tools(registry)  → DenoTool wrappers → ToolRegistry
//! ├─ fire_on_load()            → call onLoad hooks
//! │
//! └─ reload(dirs)              → mtime comparison → selective respawn
//!     ├─ discover(dirs)        → fresh scan
//!     ├─ diff                  → new, changed, removed
//!     ├─ shutdown removed      → clean up old runtimes
//!     ├─ spawn new/changed     → create new runtimes
//!     ├─ unregister old tools  → remove stale tools from registry
//!     └─ register new tools    → add fresh tools to registry
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use rho_core::config::ExtensionConfig;
use rho_core::newtypes::ToolName;
use rho_core::tool::{Tool, ToolRegistry};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::deno_observer::DenoObserver;
use crate::deno_tool::DenoTool;
use crate::discover::{self, DiscoveredExtension};
use crate::error::ExtensionError;
use crate::runtime::ExtensionRuntime;

// ── Loaded extension state ────────────────────────────────────────────────────

/// A running extension, tracking its entry file's mtime for hot reload.
struct LoadedState {
    /// The shared runtime handle (also held by DenoTool/DenoObserver).
    runtime: Arc<Mutex<ExtensionRuntime>>,
    /// The extension's manifest (cached to avoid locking the runtime).
    manifest: crate::manifest::LoadedExtension,
    /// The entry file's modification time at load time.
    mtime: SystemTime,
    /// Tool names registered from this extension.
    tool_names: Vec<String>,
}

// ── Reload result ─────────────────────────────────────────────────────────────

/// The outcome of a reload operation.
#[derive(Clone, Debug, Default)]
pub struct ReloadReport {
    /// Extensions that were newly loaded (didn't exist before).
    pub added: Vec<String>,
    /// Extensions whose entry files changed and were reloaded.
    pub reloaded: Vec<String>,
    /// Extensions that were removed (no longer discovered).
    pub removed: Vec<String>,
    /// Extensions that failed to load.
    pub failed: Vec<(String, String)>,
}

impl ReloadReport {
    /// Returns `true` if any changes were made.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.reloaded.is_empty() || !self.removed.is_empty()
    }
}

// ── ExtensionLoader ───────────────────────────────────────────────────────────

/// Manages the full lifecycle of rho extensions.
///
/// Owns running [`ExtensionRuntime`] instances, tracks mtimes for hot reload,
/// and provides methods to register/unregister tools in a [`ToolRegistry`].
///
/// # Thread safety
///
/// The loader itself is `Send + Sync` — all runtime access goes through
/// `Arc<Mutex<ExtensionRuntime>>`.
pub struct ExtensionLoader {
    /// Extension configuration (enabled/disabled, permissions).
    config: ExtensionConfig,
    /// The project sandbox root.
    ///
    /// Passed to `ExtensionRuntime::spawn_from_file_with_perms` so that
    /// extensions have the project root as their `cwd` for file access.
    project_root: PathBuf,
    /// Currently loaded extensions, keyed by name.
    loaded: HashMap<String, LoadedState>,
}

impl ExtensionLoader {
    /// Create a new, empty extension loader.
    ///
    /// The `project_root` is stored so that all spawned extensions
    /// have the project root as their working directory for file access.
    pub fn new(config: ExtensionConfig, project_root: PathBuf) -> Self {
        Self {
            config,
            project_root,
            loaded: HashMap::new(),
        }
    }

    /// Replace the stored extension configuration.
    ///
    /// This should be called before reloading extensions when config files have been
    /// edited during the session, so that newly-enabled extensions are
    /// picked up by the filter.
    pub fn set_config(&mut self, config: ExtensionConfig) {
        self.config = config;
    }

    /// Discover and load all extensions from the given directories.
    ///
    /// This is the initial load — call [`ExtensionLoader::register_tools`]
    /// afterwards to wire tools into the registry.
    ///
    /// # Errors
    ///
    /// Individual extension load failures are logged and skipped. The method
    /// returns `Ok(())` as long as the discovery scan succeeds.
    pub fn load_all(&mut self, dirs: &[PathBuf]) -> Result<(), ExtensionError> {
        let extensions = discover_and_filter(dirs, &self.config)?;
        self.spawn_extensions(extensions);
        Ok(())
    }

    /// Register all loaded extension tools into the given registry.
    ///
    /// Call this after `load_all` or `reload`. Each tool is wrapped in a
    /// [`DenoTool`] before registration.
    pub fn register_tools(&self, registry: &mut ToolRegistry) {
        for (name, state) in &self.loaded {
            for tool_meta in &state.manifest.tools {
                let deno_tool = DenoTool::new(tool_meta.clone(), state.runtime.clone());
                let tool_name = deno_tool.name();
                registry.register(Box::new(deno_tool));
                debug!(
                    extension = %name,
                    tool = %tool_name,
                    "registered extension tool"
                );
            }
        }
    }

    /// Fire `onLoad` hooks on all loaded extensions that declare one.
    ///
    /// This is async because hook execution goes through the V8 runtime.
    pub async fn fire_on_load(&self) {
        for (name, state) in &self.loaded {
            let rt = state.runtime.lock().await;
            if rt.manifest().hooks.on_load.is_none() {
                continue;
            }
            if let Err(e) = rt.call_hook("onLoad", "").await {
                warn!(extension = %name, error = %e, "onLoad hook failed");
            } else {
                debug!(extension = %name, "onLoad hook fired");
            }
        }
    }

    /// Build observers for all loaded extensions.
    ///
    /// Returns one [`DenoObserver`] per loaded extension. Observers should
    /// be composed into a single observer for the agent loop.
    pub fn build_observers(&self) -> Vec<DenoObserver> {
        self.loaded
            .values()
            .map(|state| DenoObserver::new(state.runtime.clone()))
            .collect()
    }

    /// Hot-reload extensions.
    ///
    /// Compares currently loaded extensions against a fresh discovery scan.
    /// Extensions whose entry file mtime changed are shut down and respawned.
    /// New extensions are loaded; removed extensions are shut down.
    ///
    /// Updates the `registry` in place:
    /// - Unregisters tools from removed/changed extensions
    /// - Registers tools from new/changed extensions
    ///
    /// # Errors
    ///
    /// Individual extension load failures are logged and recorded in the
    /// [`ReloadReport::failed`] list. The method returns `Ok(report)` even
    /// when some extensions fail.
    pub async fn reload(
        &mut self,
        dirs: &[PathBuf],
        registry: &mut ToolRegistry,
    ) -> Result<ReloadReport, ExtensionError> {
        let fresh = discover_and_filter(dirs, &self.config)?;
        let mut report = ReloadReport::default();

        // Build a map of fresh extensions for quick lookup.
        let fresh_map: HashMap<String, DiscoveredExtension> =
            fresh.into_iter().map(|e| (e.name.clone(), e)).collect();

        // ── Phase 1: Remove extensions that are no longer discovered ──────
        let current_names: Vec<String> = self.loaded.keys().cloned().collect();
        for name in &current_names {
            if !fresh_map.contains_key(name) {
                self.unload_extension(name, registry);
                report.removed.push(name.clone());
            }
        }

        // ── Phase 2: Check fresh extensions for new or changed ────────────
        for (name, disc) in &fresh_map {
            let current_mtime = mtime_of(&disc.entry_path);

            if let Some(state) = self.loaded.get(name) {
                // Already loaded — check mtime.
                if current_mtime > state.mtime {
                    info!(extension = %name, "file changed, reloading");
                    self.unload_extension(name, registry);
                    match self.spawn_one(name, disc, current_mtime) {
                        Ok(()) => {
                            self.register_extension_tools(name, registry);
                            report.reloaded.push(name.clone());
                        }
                        Err(e) => {
                            warn!(extension = %name, error = %e, "reload failed");
                            report.failed.push((name.clone(), e.to_string()));
                        }
                    }
                }
                // else: unchanged, skip.
            } else {
                // New extension.
                info!(extension = %name, "new extension discovered");
                match self.spawn_one(name, disc, current_mtime) {
                    Ok(()) => {
                        self.register_extension_tools(name, registry);
                        report.added.push(name.clone());
                    }
                    Err(e) => {
                        warn!(extension = %name, error = %e, "load failed");
                        report.failed.push((name.clone(), e.to_string()));
                    }
                }
            }
        }

        // ── Phase 3: Fire onLoad on new/reloaded extensions ──────────────
        for name in report.added.iter().chain(report.reloaded.iter()) {
            if let Some(state) = self.loaded.get(name) {
                let rt = state.runtime.lock().await;
                if rt.manifest().hooks.on_load.is_some()
                    && let Err(e) = rt.call_hook("onLoad", "").await
                {
                    warn!(extension = %name, error = %e, "onLoad hook failed after reload");
                }
            }
        }

        if report.has_changes() {
            info!(
                added = report.added.len(),
                reloaded = report.reloaded.len(),
                removed = report.removed.len(),
                failed = report.failed.len(),
                "extension reload complete"
            );
        } else {
            debug!("no extensions changed on reload");
        }

        Ok(report)
    }

    /// Shut down all loaded extensions.
    pub async fn shutdown_all(&mut self) {
        for (name, state) in self.loaded.drain() {
            let mut rt = state.runtime.lock().await;
            if let Err(e) = rt.shutdown() {
                warn!(extension = %name, error = %e, "shutdown failed");
            }
        }
    }

    /// Number of currently loaded extensions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.loaded.len()
    }

    /// Whether any extensions are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.loaded.is_empty()
    }

    /// Return the names of all currently loaded extensions.
    #[must_use]
    pub fn loaded_names(&self) -> Vec<String> {
        self.loaded.keys().cloned().collect()
    }

    /// Return the names of tools contributed by each loaded extension.
    ///
    /// Returns a list of `(extension_name, tool_names)` pairs in stable
    /// order (sorted by extension name). Used to inject extension awareness
    /// into the system prompt.
    #[must_use]
    pub fn extension_tools(&self) -> Vec<(String, Vec<String>)> {
        let mut pairs: Vec<(String, Vec<String>)> = self
            .loaded
            .iter()
            .map(|(name, state)| (name.clone(), state.tool_names.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    /// Update the model name in all loaded extension runtimes.
    ///
    /// This allows extensions to see the current model via `rho.getModel()`
    /// after the user switches models with `/model`.
    pub async fn set_model_all(&self, model: &str) {
        for (name, state) in &self.loaded {
            let rt = state.runtime.lock().await;
            rt.set_model(model);
            debug!(extension = %name, model = %model, "updated model");
        }
    }

    /// Get a reference to the shared runtime for an extension.
    #[allow(dead_code)]
    fn get_runtime(&self, name: &str) -> Option<Arc<Mutex<ExtensionRuntime>>> {
        self.loaded.get(name).map(|s| s.runtime.clone())
    }

    /// Unload a single extension: shut down the runtime and unregister tools.
    fn unload_extension(&mut self, name: &str, registry: &mut ToolRegistry) {
        if let Some(state) = self.loaded.remove(name) {
            // Unregister tools.
            for tool_name in &state.tool_names {
                registry.unregister(&ToolName::from(tool_name.as_str()));
                debug!(extension = %name, tool = %tool_name, "unregistered tool");
            }
            // Shut down the runtime.
            // Note: we can't await here (sync context), so we drop the
            // Arc<Mutex<>> and let the runtime clean up when all references
            // are gone (ExtensionRuntime::drop closes the channel).
            drop(state);
            info!(extension = %name, "unloaded");
        }
    }

    /// Register tools for a single extension into the registry.
    fn register_extension_tools(&self, name: &str, registry: &mut ToolRegistry) {
        let Some(state) = self.loaded.get(name) else {
            return;
        };

        for tool_meta in &state.manifest.tools {
            let deno_tool = DenoTool::new(tool_meta.clone(), state.runtime.clone());
            let tool_name = deno_tool.name();
            registry.register(Box::new(deno_tool));
            debug!(
                extension = %name,
                tool = %tool_name,
                "registered extension tool"
            );
        }
    }

    /// Spawn a single extension and add it to the loaded map.
    fn spawn_one(
        &mut self,
        name: &str,
        disc: &DiscoveredExtension,
        mtime: SystemTime,
    ) -> Result<(), ExtensionError> {
        let perms = self.config.permissions_for(name);
        let rt = ExtensionRuntime::spawn_from_file_with_perms(
            &disc.entry_path,
            &disc.root_dir,
            &self.project_root,
            &perms,
            "",
        )?;

        let manifest = rt.manifest().clone();
        let tool_names: Vec<String> = manifest.tools.iter().map(|t| t.name.clone()).collect();

        let shared = Arc::new(Mutex::new(rt));

        self.loaded.insert(
            name.to_string(),
            LoadedState {
                runtime: shared,
                manifest,
                mtime,
                tool_names,
            },
        );

        Ok(())
    }

    /// Spawn multiple discovered extensions, logging failures.
    fn spawn_extensions(&mut self, extensions: Vec<DiscoveredExtension>) {
        for disc in extensions {
            let name = disc.name.clone();
            let mtime = mtime_of(&disc.entry_path);
            let perms = self.config.permissions_for(&name);

            match ExtensionRuntime::spawn_from_file_with_perms(
                &disc.entry_path,
                &disc.root_dir,
                &self.project_root,
                &perms,
                "", // model name — will be set when wired into rho
            ) {
                Ok(rt) => {
                    let manifest = rt.manifest().clone();
                    let tool_names: Vec<String> =
                        manifest.tools.iter().map(|t| t.name.clone()).collect();

                    info!(
                        extension = %name,
                        tools = tool_names.len(),
                        "loaded extension"
                    );

                    self.loaded.insert(
                        name,
                        LoadedState {
                            runtime: Arc::new(Mutex::new(rt)),
                            manifest,
                            mtime,
                            tool_names,
                        },
                    );
                }
                Err(e) => {
                    warn!(extension = %name, error = %e, "failed to load extension");
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Discover, deduplicate, and filter extensions.
fn discover_and_filter(
    dirs: &[PathBuf],
    config: &ExtensionConfig,
) -> Result<Vec<DiscoveredExtension>, ExtensionError> {
    let discovered = discover::discover(dirs)
        .map_err(|e| ExtensionError::ModuleLoad(format!("extension directory scan failed: {e}")))?;
    let deduped = discover::deduplicate(discovered);
    let filtered = discover::filter_by_config(&deduped, config);
    Ok(filtered)
}

/// Get the modification time of a file, defaulting to epoch zero on error.
fn mtime_of(path: &std::path::Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal TS extension source.
    const EXT_SOURCE: &str = r#"
        export default {
            name: "test-ext",
            tools: [{
                name: "ping",
                description: "Ping",
                risk: "read" as const,
                parameters: {},
                execute: async () => "pong",
            }],
        };
    "#;

    /// Create a temp dir with an extension.
    fn make_ext_dir(name: &str, source: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let ext_dir = dir.path().join(name);
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("mod.ts"), source).unwrap();
        dir
    }

    /// Config that allows everything.
    fn permissive_config() -> ExtensionConfig {
        ExtensionConfig::default()
    }

    // =========================================================================
    // load_all + register_tools
    // =========================================================================

    #[test]
    fn load_all_discovers_and_loads_extensions() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        assert_eq!(loader.len(), 1);
        assert!(loader.get_runtime("hello").is_some());
    }

    #[test]
    fn load_all_registers_tools() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        let mut registry = ToolRegistry::new();
        loader.register_tools(&mut registry);

        assert!(registry.get_by_name(&ToolName::from("ping")).is_some());
    }

    #[test]
    fn load_all_skips_disabled_extensions() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let config = ExtensionConfig {
            disabled: vec!["hello".into()],
            ..Default::default()
        };
        let mut loader = ExtensionLoader::new(config, std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        assert!(loader.is_empty());
    }

    #[test]
    fn load_all_empty_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();
        assert!(loader.is_empty());
    }

    // =========================================================================
    // Tool execution through loader
    // =========================================================================

    #[tokio::test]
    async fn tool_execution_works_after_load() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        let rt = loader.get_runtime("hello").unwrap();
        let rt = rt.lock().await;
        let result = rt.call_tool("ping", "").await.unwrap();
        assert_eq!(result, "pong");
    }

    // =========================================================================
    // Reload
    // =========================================================================

    #[tokio::test]
    async fn reload_detects_new_extension() {
        let dir = tempfile::tempdir().unwrap();
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();
        assert!(loader.is_empty());

        // Add a new extension.
        let ext_dir = dir.path().join("new_ext");
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(ext_dir.join("mod.ts"), EXT_SOURCE).unwrap();

        let mut registry = ToolRegistry::new();
        let report = loader
            .reload(&[dir.path().to_path_buf()], &mut registry)
            .await
            .unwrap();

        assert_eq!(report.added, vec!["new_ext"]);
        assert!(loader.get_runtime("new_ext").is_some());
        assert!(registry.get_by_name(&ToolName::from("ping")).is_some());
    }

    #[tokio::test]
    async fn reload_detects_removed_extension() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        let mut registry = ToolRegistry::new();
        loader.register_tools(&mut registry);
        assert!(registry.get_by_name(&ToolName::from("ping")).is_some());

        // Remove the extension directory.
        std::fs::remove_dir_all(dir.path().join("hello")).unwrap();

        let report = loader
            .reload(&[dir.path().to_path_buf()], &mut registry)
            .await
            .unwrap();

        assert_eq!(report.removed, vec!["hello"]);
        assert!(loader.is_empty());
        assert!(registry.get_by_name(&ToolName::from("ping")).is_none());
    }

    #[tokio::test]
    async fn reload_detects_changed_extension() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        let mut registry = ToolRegistry::new();
        loader.register_tools(&mut registry);

        // Modify the extension file (bump mtime).
        std::thread::sleep(std::time::Duration::from_millis(10));
        let changed_source = r#"
            export default {
                name: "hello",
                tools: [{
                    name: "ping2",
                    description: "Ping2",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "pong2",
                }],
            };
        "#;
        std::fs::write(dir.path().join("hello/mod.ts"), changed_source).unwrap();

        let report = loader
            .reload(&[dir.path().to_path_buf()], &mut registry)
            .await
            .unwrap();

        assert_eq!(report.reloaded, vec!["hello"]);
        // Old tool should be gone, new one should exist.
        assert!(registry.get_by_name(&ToolName::from("ping")).is_none());
        assert!(registry.get_by_name(&ToolName::from("ping2")).is_some());
    }

    #[tokio::test]
    async fn reload_no_changes_is_noop() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();

        let mut registry = ToolRegistry::new();
        let report = loader
            .reload(&[dir.path().to_path_buf()], &mut registry)
            .await
            .unwrap();

        assert!(!report.has_changes());
        assert_eq!(loader.len(), 1);
    }

    // =========================================================================
    // Observers
    // =========================================================================

    #[test]
    fn build_observers_returns_one_per_extension() {
        let dir1 = make_ext_dir("ext1", EXT_SOURCE);
        let dir2 = make_ext_dir(
            "ext2",
            r#"
            export default {
                name: "ext2",
                tools: [{
                    name: "echo",
                    description: "Echo",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "echo",
                }],
            };
        "#,
        );
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        // We need a single scan dir for both extensions
        let combined = tempfile::tempdir().unwrap();
        let e1 = combined.path().join("ext1");
        let e2 = combined.path().join("ext2");
        std::fs::create_dir_all(&e1).unwrap();
        std::fs::create_dir_all(&e2).unwrap();
        std::fs::write(e1.join("mod.ts"), EXT_SOURCE).unwrap();
        std::fs::write(
            e2.join("mod.ts"),
            r#"
            export default {
                name: "ext2",
                tools: [{
                    name: "echo",
                    description: "Echo",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => "echo",
                }],
            };
        "#,
        )
        .unwrap();
        drop(dir1);
        drop(dir2);

        loader.load_all(&[combined.path().to_path_buf()]).unwrap();
        let observers = loader.build_observers();
        assert_eq!(observers.len(), 2);
    }

    // =========================================================================
    // onLoad
    // =========================================================================

    #[tokio::test]
    async fn fire_on_load_calls_hook() {
        let dir = make_ext_dir(
            "hooked",
            r#"
            let loaded = false;
            export default {
                name: "hooked",
                tools: [{
                    name: "check",
                    description: "Check",
                    risk: "read" as const,
                    parameters: {},
                    execute: async () => String(loaded),
                }],
                hooks: {
                    onLoad: async () => { loaded = true; },
                },
            };
        "#,
        );

        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();
        loader.fire_on_load().await;

        let rt = loader.get_runtime("hooked").unwrap();
        let rt = rt.lock().await;
        let result = rt.call_tool("check", "").await.unwrap();
        assert_eq!(result, "true");
    }

    // =========================================================================
    // shutdown_all
    // =========================================================================

    #[tokio::test]
    async fn shutdown_all_cleans_up() {
        let dir = make_ext_dir("hello", EXT_SOURCE);
        let mut loader = ExtensionLoader::new(permissive_config(), std::path::PathBuf::from("."));
        loader.load_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(loader.len(), 1);

        loader.shutdown_all().await;
        assert!(loader.is_empty());
    }
}
