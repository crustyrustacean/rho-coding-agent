//! Built-in tool implementations for rho.
//!
//! Phase 1b: [`ReadFile`], [`WriteFile`], and [`RunCommand`] with sandbox
//! enforcement and `<context>` framing on file reads.

pub mod files;
pub mod shell;

pub use files::{ReadFile, WriteFile};
pub use shell::RunCommand;

use rho_core::{SandboxRoot, ToolRegistry};

/// Register all built-in tools into a [`ToolRegistry`].
///
/// All file and shell tools are bound to `root` so they cannot operate outside
/// the project sandbox.
pub fn register_all(registry: &mut ToolRegistry, root: SandboxRoot) {
    registry.register(Box::new(ReadFile { root: root.clone() }));
    registry.register(Box::new(WriteFile { root: root.clone() }));
    registry.register(Box::new(RunCommand { root }));
}
