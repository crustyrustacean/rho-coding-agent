//! Presentation layer.
//!
//! All formatted output goes to stderr, which is separate from the JSONL
//! stdout channel used by the RPC protocol. This keeps diagnostics visible
//! to humans without interfering with machine-readable events.

use std::path::Path;

/// Presentation layer for headless (RPC) mode.
///
/// All output goes to stderr so it doesn't interfere with JSONL stdout.
pub(crate) struct RpcPresenter;

impl RpcPresenter {
    /// Config loaded with a warning.
    pub fn config_warning(msg: &str) {
        eprintln!("Warning: {msg} — using defaults");
    }

    /// Session resumed from a specific path.
    pub fn session_resumed(path: &Path) {
        eprintln!("resuming session: {}", path.display());
    }

    /// New session created.
    pub fn session_created(path: &Path) {
        eprintln!("session: {}", path.display());
    }

    /// Hint about previous sessions.
    pub fn previous_sessions_hint(count: usize) {
        eprintln!(
            "  ({count} previous session(s) for this project — use rho -c to resume the latest)"
        );
    }

    /// Token budget summary emitted at startup.
    pub fn budget_summary(
        context_window: usize,
        completion_reserve: usize,
        prompt: usize,
        system: usize,
        schema: usize,
        overhead: usize,
        available: usize,
    ) {
        eprintln!(
            "budget: {context_window}T context, {completion_reserve}T reserve, {prompt}T prompt \
             ({system}T system + {schema}T schema = {overhead}T overhead, {available}T for conversation)"
        );
    }

    /// System overhead is dangerously high.
    pub fn budget_overhead_warning(pct: u32) {
        eprintln!(
            "warning: system overhead is {pct}% of prompt budget — \
             consider --compact or increasing token_budget in .rho/config.toml"
        );
    }

    /// Extensions loaded successfully.
    pub fn extensions_loaded(count: usize) {
        if count > 0 {
            eprintln!("extensions: {count} loaded");
        }
    }

    /// Extension load error.
    pub fn extension_load_error(msg: &str) {
        eprintln!("warning: extension load error: {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Smoke tests: verify methods compile and don't panic.
    // Output goes to stderr (safe alongside JSONL stdout in tests).

    #[test]
    fn startup_methods_do_not_panic() {
        RpcPresenter::config_warning("bad config");
        RpcPresenter::budget_overhead_warning(75);
    }

    #[test]
    fn extension_methods_do_not_panic() {
        RpcPresenter::extensions_loaded(3);
        RpcPresenter::extension_load_error("bad extension");
    }
}
