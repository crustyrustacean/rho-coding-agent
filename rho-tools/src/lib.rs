//! Built-in tool implementations for rho.
//!
//! ## File tools (Phase 2)
//!
//! [`ReadFile`], [`WriteFile`], [`ListDir`], [`EditFile`] — sandbox-enforced
//! file operations with `<context>` framing, `.gitignore`-aware listing, and
//! exact-match editing.
//!
//! ## Shell tools (Phase 2)
//!
//! [`RunCommand`], [`PowerShellExecutor`], [`CommandDenylist`] — shell execution
//! behind the [`ShellExecutor`] trait with command denylist enforcement.
//!
//! ## Rust tools (Phase 3)
//!
//! [`CargoCheck`] — structured compiler diagnostics via
//! `cargo check --message-format=json`, with NDJSON parsing and dependency
//! noise filtering.

pub mod files;
pub mod rust;
pub mod shell;

pub use files::{EditFile, ListDir, ReadFile, WriteFile};
pub use rust::{CargoCheck, CargoClippy, RustcExplain};
pub use shell::{CommandDenylist, PowerShellExecutor, RunCommand};

use rho_core::{SandboxRoot, ToolRegistry};

/// Register all built-in tools into a [`ToolRegistry`].
///
/// All file and shell tools are bound to `root` so they cannot operate outside
/// the project sandbox. The default shell executor is [`PowerShellExecutor`]
/// with a [`CommandDenylist`] built from the default PowerShell list plus any
/// config-supplied additions.
pub fn register_all(registry: &mut ToolRegistry, root: SandboxRoot, config: &rho_core::RhoConfig) {
    registry.register(Box::new(ReadFile { root: root.clone() }));
    registry.register(Box::new(WriteFile { root: root.clone() }));
    registry.register(Box::new(ListDir { root: root.clone() }));
    registry.register(Box::new(EditFile { root: root.clone() }));

    // Rust tooling (Phase 3)
    let check_executor = Box::new(PowerShellExecutor::new());
    registry.register(Box::new(CargoCheck {
        root: root.clone(),
        executor: check_executor,
    }));
    let clippy_executor = Box::new(PowerShellExecutor::new());
    registry.register(Box::new(CargoClippy {
        root: root.clone(),
        executor: clippy_executor,
    }));
    let explain_executor = Box::new(PowerShellExecutor::new());
    registry.register(Box::new(RustcExplain {
        root: root.clone(),
        executor: explain_executor,
    }));

    let executor = Box::new(PowerShellExecutor::new());
    let denylist = CommandDenylist::from_config(config);
    registry.register(Box::new(RunCommand {
        root,
        executor,
        denylist,
    }));
}
