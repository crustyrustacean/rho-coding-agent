//! Presentation layer.
//!
//! All formatted output goes to stderr, which is separate from the JSONL
//! stdout channel used by the RPC protocol. This keeps diagnostics visible
//! to humans without interfering with machine-readable events.

use std::path::Path;

/// Presentation layer for headless (RPC) mode.
///
/// All output goes to stderr so it doesn't interfere with JSONL stdout.
/// Interactive stdin-reading methods are stubs or errors.
pub(crate) struct RpcPresenter;

// ── Startup ───────────────────────────────────────────────────────────────────

impl RpcPresenter {
    /// Config loaded with a warning.
    pub fn config_warning(msg: &str) {
        eprintln!("Warning: {msg} — using defaults");
    }

    /// Provider type is not supported by either OpenAI-shaped transport.
    pub fn provider_type_warning(provider_type: &str) {
        eprintln!(
            "warning: provider type \"{provider_type}\" was set, but rho only supports \
             OpenAI Responses or Chat Completions endpoints. Requests may fail."
        );
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

    /// Session's working directory has changed.
    pub fn stale_cwd_warning(session_cwd: &Path, current_cwd: &Path, exists: bool) {
        if !session_cwd.as_os_str().is_empty() && !exists {
            eprintln!(
                "warning: session's working directory no longer exists\n  \
                 session: {}\n  current: {}\n  continuing with current directory",
                session_cwd.display(),
                current_cwd.display()
            );
        } else {
            eprintln!(
                "warning: session was created in a different directory\n  \
                 session: {}\n  current: {}\n  continuing with current directory",
                session_cwd.display(),
                current_cwd.display()
            );
        }
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

// ── Model resolution ──────────────────────────────────────────────────────────

impl RpcPresenter {
    /// Model auto-detected from provider.
    pub fn model_auto_detected(model: &str, provider: &str) {
        eprintln!("auto-detected model: {model} (from provider: {provider})");
    }

    /// Model explicitly specified and validated.
    pub fn model_from_source(model: &str, source: &str, provider: &str) {
        eprintln!("using model from {source}: {model} (provider: {provider})");
    }

    /// Catalog enrichment details for a resolved model.
    pub fn model_catalog_info(context_window: u64, max_tokens: u64, thinking: bool) {
        eprintln!(
            "catalog: context_window={context_window}, max_output={max_tokens}, thinking={thinking}"
        );
    }
}

// ── Provider consent ──────────────────────────────────────────────────────────

impl RpcPresenter {
    /// Headless stub: external provider consent is auto-declined.
    ///
    /// RPC callers must pass `--accept-external-provider` (or `--endpoint`)
    /// to use external providers. Full JSONL-based consent handling arrives
    /// in step 6.
    pub fn provider_consent_prompt(_has_local: bool, _external_names: &[&str]) {
        // No-op: headless policy is applied by the caller before reaching this.
    }

    /// User declined consent.
    pub fn provider_consent_aborted() {
        eprintln!("  Aborting. Use --accept-external-provider to skip this prompt.");
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
        RpcPresenter::provider_type_warning("anthropic");
        RpcPresenter::budget_overhead_warning(75);
    }

    #[test]
    fn provider_consent_prompt_is_noop() {
        // Must not block on stdin or write to stdout.
        RpcPresenter::provider_consent_prompt(true, &["openrouter"]);
    }

    #[test]
    fn extension_methods_do_not_panic() {
        RpcPresenter::extensions_loaded(3);
        RpcPresenter::extension_load_error("bad extension");
    }
}
