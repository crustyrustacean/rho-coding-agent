//! rho-ext — TypeScript extension runtime for rho.
//!
//! Provides [`ExtensionRuntime`] which owns a V8 isolate on a dedicated thread
//! and allows calling exported TypeScript functions via async channels.
//!
//! The spike in [`spike`] validates the threading model and `deno_core` API.
//! The production runtime is in [`runtime`].

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
