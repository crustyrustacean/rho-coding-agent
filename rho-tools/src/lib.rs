//! Built-in tool implementations for rho.
//!
//! Phase 2: [`ReadFile`], [`WriteFile`], [`RunCommand`], and [`PowerShellExecutor`]
//! with sandbox enforcement, `<context>` framing on file reads, and shell
//! execution abstracted behind the [`ShellExecutor`] trait.

pub mod files;
pub mod shell;

pub use files::{ReadFile, WriteFile};
pub use shell::{PowerShellExecutor, RunCommand};

use rho_core::{SandboxRoot, ToolRegistry};

/// Register all built-in tools into a [`ToolRegistry`].
///
/// All file and shell tools are bound to `root` so they cannot operate outside
/// the project sandbox. The default shell executor is [`PowerShellExecutor`].
pub fn register_all(registry: &mut ToolRegistry, root: SandboxRoot) {
    registry.register(Box::new(ReadFile { root: root.clone() }));
    registry.register(Box::new(WriteFile { root: root.clone() }));
    let executor = Box::new(PowerShellExecutor::new());
    registry.register(Box::new(RunCommand { root, executor }));
}
