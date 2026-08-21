//! Built-in tool implementations for rho.
//!
//! ## File tools (Phase 2)
//!
//! [`ReadFile`], [`WriteFile`], [`ListDir`], [`EditFile`] — sandbox-enforced
//! file operations with `<context>` framing, `.gitignore`-aware listing, and
//! hashline-anchored editing.
//!
//! ## Shell tools (Phase 2)
//!
//! [`RunCommand`], [`PowerShellExecutor`], [`CommandDenylist`] — shell execution
//! behind the `ShellExecutor` trait with command denylist enforcement.
//!
//! ## Rust tools (Phase 3)
//!
//! [`CargoCheck`] — structured compiler diagnostics via
//! `cargo check --message-format=json`, with NDJSON parsing and dependency
//! noise filtering.
//!
//! [`RustdocTool`] — stdlib documentation lookup from locally installed rustdoc.
//!
//! ## Hashline editing (Phase 3.10)
//!
//! [`compute_line_hash`] — content-addressed line editing with hash-anchored
//! references for reliable file edits.

pub mod crates_io;
pub mod edit;
pub mod error;
pub mod file_ops;
pub mod files;
pub mod hashline;
pub mod memory;
pub mod rust;
pub mod search;
pub mod session_summary;
pub mod shell;

pub use session_summary::SessionSummary;

use rho_memory::Memory;
pub type SessionPathHolder = Arc<Mutex<Option<PathBuf>>>;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use crate::edit::EditFile;
pub use crate::error::{ToolError, ToolResult};
pub use crates_io::CratesIoLookup;
pub use file_ops::{BatchRead, ListDir, ReadFile, WriteFile};
pub use hashline::compute_line_hash;
pub use memory::MemoryTool;
pub use rust::{CargoCheck, CargoClippy, CargoFix, CargoTest, RustcExplain, RustdocTool};
pub use search::{FindFiles, SearchFiles};
pub use shell::{CommandDenylist, PowerShellExecutor, RunCommand};

use rho_core::{SandboxRoot, ToolRegistry};

/// Register all built-in tools into a [`ToolRegistry`].
///
/// All file and shell tools are bound to `root` so they cannot operate outside
/// the project sandbox. The default shell executor is [`PowerShellExecutor`]
/// with a [`CommandDenylist`] built from the default PowerShell list plus any
/// config-supplied additions.
///
/// # Errors
///
/// Returns an error if no PowerShell (`pwsh` or `powershell`) is found on
/// `PATH`. Shell-based tools require PowerShell for command execution.
pub fn register_all(
    registry: &mut ToolRegistry,
    root: SandboxRoot,
    config: &rho_core::RhoConfig,
    memory: Option<Arc<Memory>>,
) -> ToolResult<SessionPathHolder> {
    registry.register(Box::new(ReadFile { root: root.clone() }));
    registry.register(Box::new(BatchRead { root: root.clone() }));
    registry.register(Box::new(WriteFile { root: root.clone() }));
    registry.register(Box::new(ListDir { root: root.clone() }));
    registry.register(Box::new(EditFile { root: root.clone() }));

    let make_executor =
        || -> ToolResult<Box<PowerShellExecutor>> { PowerShellExecutor::new().map(Box::new) };

    // Rust tooling (Phase 3)
    let check_executor = make_executor()?;
    registry.register(Box::new(CargoCheck {
        root: root.clone(),
        executor: check_executor,
    }));
    let clippy_executor = make_executor()?;
    registry.register(Box::new(CargoClippy {
        root: root.clone(),
        executor: clippy_executor,
    }));
    let explain_executor = make_executor()?;
    registry.register(Box::new(RustcExplain {
        root: root.clone(),
        executor: explain_executor,
    }));
    let test_executor = make_executor()?;
    registry.register(Box::new(CargoTest {
        root: root.clone(),
        executor: test_executor,
    }));
    let fix_executor = make_executor()?;
    registry.register(Box::new(CargoFix {
        root: root.clone(),
        executor: fix_executor,
    }));

    // Rustdoc lookup (Phase 3.5)
    registry.register(Box::new(RustdocTool::new()));

    // crates.io lookup (Phase 3.6)
    registry.register(Box::new(CratesIoLookup::new()));

    // Read-tier tools (engine Phase 0): structured search/find so the model
    // does not shell out for code lookups.
    registry.register(Box::new(SearchFiles::new(root.clone())));
    registry.register(Box::new(FindFiles::new(root.clone())));

    let executor = make_executor()?;
    let denylist = CommandDenylist::from_config(config);
    registry.register(Box::new(RunCommand {
        root,
        executor,
        denylist,
    }));

    // Memory tools (knowledge base)
    if let Some(mem) = memory {
        registry.register(Box::new(MemoryTool::new(mem)));
    }

    // Session summary tool (context recovery)
    let (session_summary, path_holder) = SessionSummary::new();
    registry.register(Box::new(session_summary));
    Ok(path_holder)
}
