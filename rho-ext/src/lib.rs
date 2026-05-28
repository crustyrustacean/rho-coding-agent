//! rho-ext — TypeScript extension runtime for rho.
//!
//! Provides [`ExtensionRuntime`] which owns a V8 isolate on a dedicated thread
//! and allows calling extension tools, hooks, and commands via async channels.
//!
//! Extensions declare their capabilities via `export default { ... }` in their
//! main TypeScript module. The manifest is parsed into [`LoadedExtension`]
//! during spawn and is available via [`ExtensionRuntime::manifest`].

pub mod error;
pub mod host;
pub mod manifest;
pub mod module_loader;
pub mod runtime;
pub mod spike;
pub mod transpile;

pub use error::ExtensionError;
pub use host::HostState;
pub use manifest::{
    LoadedCommand, LoadedExtension, LoadedHooks, LoadedTool, ManifestError, ToolRisk,
};
pub use module_loader::RhoModuleLoader;
pub use runtime::ExtensionRuntime;
