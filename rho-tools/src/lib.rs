//! Built-in tool implementations for rho.
//!
//! Phase 2: [`ReadFile`], [`WriteFile`], [`RunCommand`], [`PowerShellExecutor`],
//! and [`CommandDenylist`] with sandbox enforcement, `<context>` framing on file
//! reads, shell execution abstracted behind the [`ShellExecutor`] trait, and
//! command denylist enforcement.

pub mod files;
pub mod shell;

pub use files::{ReadFile, WriteFile};
pub use shell::{CommandDenylist, PowerShellExecutor, RunCommand};

use rho_core::{SandboxRoot, ToolRegistry};

/// Register all built-in tools into a [`ToolRegistry`].
///
/// All file and shell tools are bound to `root` so they cannot operate outside
/// the project sandbox. The default shell executor is [`PowerShellExecutor`]
/// with the default [`CommandDenylist`].
pub fn register_all(registry: &mut ToolRegistry, root: SandboxRoot) {
    registry.register(Box::new(ReadFile { root: root.clone() }));
    registry.register(Box::new(WriteFile { root: root.clone() }));
    let executor = Box::new(PowerShellExecutor::new());
    let denylist = CommandDenylist::default_powershell();
    registry.register(Box::new(RunCommand {
        root,
        executor,
        denylist,
    }));
}
