//! Extension management subcommand handlers.
//!
//! Dispatched from `main.rs` when `rho extension <command>` is invoked.
//! Each handler is intentionally thin — it validates arguments, performs the
//! operation, and prints a human-readable result to stdout.

use crate::cli::ExtensionArgs;

/// Run an extension management subcommand.
///
/// # Errors
///
/// Returns an error if the operation fails (I/O, network, manifest parsing, etc.).
#[expect(
    clippy::unused_async,
    reason = "async will be needed when real implementations land"
)]
pub async fn run(args: ExtensionArgs) -> anyhow::Result<()> {
    use crate::cli::ExtensionCommand;

    match args.command {
        ExtensionCommand::Sync => sync(),
        ExtensionCommand::Install { url } => install(&url),
        ExtensionCommand::Remove { name } => remove(&name),
        ExtensionCommand::List => list(),
    }
}

/// Sync extensions from the manifest file.
fn sync() -> anyhow::Result<()> {
    // TODO: read manifest, download missing extensions, reload
    eprintln!("error: `rho extension sync` is not yet implemented");
    std::process::exit(1)
}

/// Install an extension from a URL.
fn install(url: &str) -> anyhow::Result<()> {
    // TODO: add to manifest, download, reload
    eprintln!("error: `rho extension install` is not yet implemented");
    eprintln!("       url: {url}");
    std::process::exit(1)
}

/// Remove an installed extension.
fn remove(name: &str) -> anyhow::Result<()> {
    // TODO: remove from manifest, delete file, reload
    eprintln!("error: `rho extension remove` is not yet implemented");
    eprintln!("       name: {name}");
    std::process::exit(1)
}

/// List installed extensions.
fn list() -> anyhow::Result<()> {
    // TODO: discover and list installed extensions with their source info
    eprintln!("error: `rho extension list` is not yet implemented");
    std::process::exit(1)
}
