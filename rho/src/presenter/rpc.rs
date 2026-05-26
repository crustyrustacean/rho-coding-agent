#![allow(dead_code)] // Staged API — startup/model-resolution methods are wired up in step 6.

//! RPC-mode presentation layer.
//!
//! [`RpcPresenter`] is the RPC-mode counterpart to [`super::ReplPresenter`].
//!
//! ## Design notes
//!
//! * **Startup / diagnostic output** (`eprintln!`) is identical to the REPL
//!   presenter. Stderr is separate from the JSONL stdout channel, so
//!   human-readable diagnostic messages do not interfere with the protocol.
//!
//! * **REPL-only output** (user prompt, assistant reply, slash-command
//!   responses) is silenced — these concepts don't exist in RPC mode; the
//!   protocol carries them as typed events instead.
//!
//! * **Interactive stdin-reading methods** (`provider_consent_prompt`,
//!   `picker_header`, `picker_manual_prompt`) are stubs that always take the
//!   safe/headless path. Full headless handling (auto-approve, emit JSONL
//!   prompts, etc.) lands in step 6.

use std::io::{self, Write};
use std::path::Path;

/// RPC-mode presenter.
///
/// Groups formatted output by concern, mirroring [`super::ReplPresenter`].
/// Methods that write to stderr are equivalent to their REPL counterparts.
/// Methods that would write to stdout or block on stdin are no-ops or stubs.
pub(crate) struct RpcPresenter;

// ── Startup ───────────────────────────────────────────────────────────────────

impl RpcPresenter {
    /// Config loaded with a warning.
    pub fn config_warning(msg: &str) {
        eprintln!("Warning: {msg} — using defaults");
    }

    /// Provider type is not OpenAI-compatible.
    pub fn provider_type_warning(provider_type: &str) {
        eprintln!(
            "warning: provider type \"{provider_type}\" was set, but rho only supports \
             OpenAI-compatible endpoints. Requests may fail."
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

    /// Could not list models; accepting verbatim.
    pub fn model_accepting_verbatim(model: &str, source: &str) {
        eprintln!(
            "warning: could not list models from any provider; \
             accepting model from {source}: {model}"
        );
    }

    /// Model not found in provider's list.
    pub fn model_not_in_list(model: &str) {
        eprintln!("warning: model \"{model}\" not found in provider model list.");
    }

    /// Fuzzy suggestions for a near-miss model name.
    pub fn model_suggestions(suggestions: &str) {
        eprintln!("Did you mean:");
        eprintln!("{suggestions}");
    }

    /// Continuing with the specified model despite warnings.
    pub fn model_continuing(model: &str, source: &str) {
        eprintln!("continuing with model from {source}: {model}");
    }

    /// Model selected from picker.
    pub fn model_picked(model: &str, detail: &str) {
        eprintln!("  using model: {model} ({detail})");
    }

    /// Model ID entered or typed directly.
    pub fn model_entered(model: &str) {
        eprintln!("  using model: {model}");
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

// ── Model picker ──────────────────────────────────────────────────────────────

impl RpcPresenter {
    /// Popular models offered by the interactive picker.
    ///
    /// Must stay in sync with [`super::repl::ReplPresenter::PICKER_MODELS`].
    const PICKER_MODELS: &'static [(&'static str, &'static str, &'static str)] = &[
        ("Claude Sonnet 4", "Anthropic", "anthropic/claude-sonnet-4"),
        ("GPT-4o", "OpenAI", "openai/gpt-4o"),
        ("GLM-5", "z.ai", "z-ai/glm-5"),
    ];

    /// Access the picker models table.
    pub fn picker_models() -> &'static [(&'static str, &'static str, &'static str)] {
        Self::PICKER_MODELS
    }

    /// Headless stub: the interactive picker is not available in RPC mode.
    ///
    /// Full headless model selection (auto-pick first available, or JSONL
    /// prompt) arrives in step 6.
    pub fn picker_header(_provider_names: &[&str]) {
        // No-op in RPC mode.
    }

    /// Headless stub: manual model entry prompt is suppressed in RPC mode.
    pub fn picker_manual_prompt() {
        // No-op in RPC mode.
    }
}

// ── Startup flush helper ──────────────────────────────────────────────────────

impl RpcPresenter {
    /// Flush stderr. Called after groups of startup messages.
    pub fn flush() {
        let _ = io::stderr().flush();
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
    fn picker_header_is_noop() {
        // Must not block on stdin or write to stdout.
        RpcPresenter::picker_header(&["openrouter"]);
    }

    #[test]
    fn picker_manual_prompt_is_noop() {
        // Must not block on stdin or write to stdout.
        RpcPresenter::picker_manual_prompt();
    }

    #[test]
    fn picker_models_returns_nonempty_list() {
        assert!(!RpcPresenter::picker_models().is_empty());
    }

    #[test]
    fn picker_models_matches_repl_presenter() {
        let repl = super::super::repl::ReplPresenter::picker_models();
        let rpc = RpcPresenter::picker_models();
        // Length and first entry must match; keeps the two lists in sync.
        assert_eq!(rpc.len(), repl.len());
        assert_eq!(rpc[0], repl[0]);
    }
}
