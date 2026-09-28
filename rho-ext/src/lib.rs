//! rho-ext — TypeScript extension runtime for rho.
//!
//! Provides [`ExtensionRuntime`] which owns a V8 isolate on a dedicated thread
//! and allows calling extension tools, hooks, and commands via async channels.
//!
//! Extensions declare their capabilities via `export default { ... }` in their
//! main TypeScript module. The manifest is parsed into [`LoadedExtension`]
//! during spawn and is available via [`ExtensionRuntime::manifest`].
//!
//! # Concurrency model
//!
//! **One extension per isolate, loaded and invoked serially.** Each
//! [`ExtensionRuntime`] owns a V8 isolate on a dedicated OS thread. Isolates
//! are not invoked concurrently with one another: a single extension's calls
//! are serialised through its own request channel, and the loader spawns
//! runtimes one at a time.
//!
//! This is a deliberate constraint, not an implementation detail. V8 isolate
//! creation and teardown are expensive and are the part of this crate that has
//! proved least forgiving under concurrency — see issue #83, where running
//! this crate's test suite in parallel reliably aborts the test binary.
//!
//! Two consequences for anyone extending this crate:
//!
//! - **Build isolates on a thread that has entered a tokio runtime.**
//!   `deno_core` aborts the process if V8 posts a delayed task for an isolate
//!   registered without a tokio handle (see `runtime/setup.rs` upstream). The
//!   pattern used throughout `runtime.rs` is `thread::spawn` + a
//!   `tokio::runtime::Runtime` constructed *before* `JsRuntime::new`.
//! - **Don't create a `JsRuntime` inline on a bare thread.** Several older
//!   tests in `host.rs` do this and predate that requirement; new code should
//!   follow `runtime.rs`, which is the correct model.

pub mod async_dispatcher;
pub mod deno_observer;
pub mod deno_tool;
pub mod discover;
pub mod error;
pub mod host;
pub mod loader;
pub mod manifest;
pub mod module_loader;
pub mod ops;
pub mod runtime;
pub mod transpile;

pub use deno_observer::DenoObserver;
pub use deno_tool::DenoTool;
pub use discover::DiscoveredExtension;
pub use error::ExtensionError;
pub use host::HostState;
pub use loader::{ExtensionLoader, ReloadReport};
pub use manifest::{
    LoadedCommand, LoadedExtension, LoadedHooks, LoadedTool, ManifestError, ToolRisk,
};
pub use module_loader::RhoModuleLoader;
pub use runtime::ExtensionRuntime;
