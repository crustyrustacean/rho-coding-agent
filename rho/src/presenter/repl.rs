//! REPL-mode presentation layer.
//!
//! All terminal output for the REPL mode goes through [`ReplPresenter`].
//! No business logic here — only formatting and I/O. This keeps the
//! call sites clean and makes it straightforward to swap in an RPC
//! presenter later.

use std::io::{self, Write};
use std::path::Path;

/// REPL-mode terminal presenter.
///
/// Groups formatted output by concern (startup, REPL interaction,
/// slash commands, approval, paste mode). Every `println!`/`eprintln!`
/// that would appear in a REPL session goes through here.
pub(crate) struct ReplPresenter;

// ── Startup ──────────────────────────────────────────────────────────────────

impl ReplPresenter {
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

    /// Token budget summary printed at startup.
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

// ── Model resolution ─────────────────────────────────────────────────────────

impl ReplPresenter {
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

// ── Provider consent ─────────────────────────────────────────────────────────

impl ReplPresenter {
    /// Print the external provider consent warning and prompt.
    pub fn provider_consent_prompt(has_local: bool, external_names: &[&str]) {
        eprintln!();
        if !has_local {
            eprintln!("  ⚠  No local model server detected");
        }
        eprintln!("  ⚠  External provider(s) configured:");
        for name in external_names {
            eprintln!("      - {name}");
        }
        eprintln!();
        eprintln!("      Your prompts and code will be sent to external servers.");
        eprintln!("      This may expose proprietary code, secrets, or other");
        eprintln!("      sensitive data to the providers and any intermediaries.");
        eprintln!();
        eprint!("      Continue? [y/N] ");
        let _ = io::stderr().flush();
    }

    /// User declined consent.
    pub fn provider_consent_aborted() {
        eprintln!("  Aborting. Use --accept-external-provider to skip this prompt.");
    }
}

// ── Model picker ─────────────────────────────────────────────────────────────

impl ReplPresenter {
    /// Popular models offered by the interactive picker.
    const PICKER_MODELS: &'static [(&'static str, &'static str, &'static str)] = &[
        ("Claude Sonnet 4", "Anthropic", "anthropic/claude-sonnet-4"),
        ("GPT-4o", "OpenAI", "openai/gpt-4o"),
        ("GLM-5", "z.ai", "z-ai/glm-5"),
    ];

    /// Access the picker models table for numeric choice matching.
    pub fn picker_models() -> &'static [(&'static str, &'static str, &'static str)] {
        Self::PICKER_MODELS
    }

    /// Print the model picker menu.
    pub fn picker_header(provider_names: &[&str]) {
        eprintln!();
        eprintln!(
            "  Could not list models from: {}",
            provider_names.join(", ")
        );
        eprintln!("  Select a model to use:");
        eprintln!();
        for (i, (display_name, family, _id)) in Self::PICKER_MODELS.iter().enumerate() {
            eprintln!(
                "    [{idx}] {name:<22} ({family})",
                idx = i + 1,
                name = display_name,
                family = family
            );
        }
        eprintln!("    [0] Enter model ID manually");
        eprintln!();
        eprint!("  Choice: ");
        let _ = io::stderr().flush();
    }

    /// Prompt for manual model ID entry.
    pub fn picker_manual_prompt() {
        eprint!("  Model ID: ");
        let _ = io::stderr().flush();
    }
}

// ── REPL interaction ─────────────────────────────────────────────────────────

impl ReplPresenter {
    /// Print the user prompt marker.
    pub fn user_prompt() {
        print!("User: ");
        let _ = io::stdout().flush();
    }

    /// Print the assistant's reply.
    pub fn assistant_reply(text: &str) {
        println!("\nAssistant: {text}");
    }

    /// Print an error message.
    pub fn error(msg: &str) {
        println!("\nError: {msg}");
    }

    /// Confirm conversation was cleared.
    pub fn conversation_cleared() {
        println!("[conversation cleared]");
    }

    /// File read error during /paste.
    pub fn paste_read_error(path: &str, msg: &str) {
        println!("Error: cannot read `{path}`: {msg}");
    }
}

// ── Slash commands ───────────────────────────────────────────────────────────

impl ReplPresenter {
    /// No models available.
    pub fn no_models() {
        println!("No models available from any provider.");
    }

    /// Model list header for a provider.
    pub fn models_provider_header(provider: &str) {
        println!();
        println!("  [{provider}]");
    }

    /// Single model in the model list.
    pub fn models_entry(model: &str) {
        println!("    {model}");
    }

    /// Model list summary.
    pub fn models_summary(model_count: usize, provider_count: usize) {
        println!();
        println!("  {model_count} model(s) across {provider_count} provider(s).");
    }

    /// Successfully switched model.
    pub fn model_switched(model: &str, provider: &str) {
        println!("Switched to {model} (provider: {provider})");
    }

    /// Model not found on a specific provider.
    pub fn model_not_found_on_provider(model: &str, provider: &str) {
        println!("Model \"{model}\" not found on provider \"{provider}\".");
    }

    /// Could not query provider.
    pub fn provider_query_error(provider: &str, error: &str) {
        println!("Could not query provider \"{provider}\": {error}");
    }

    /// Provider not found.
    pub fn provider_not_found(provider: &str) {
        println!("Provider \"{provider}\" not found.");
    }

    /// No model matching query.
    pub fn no_model_match(query: &str) {
        println!("No model matching \"{query}\".");
    }

    /// Ambiguous model match — show candidates.
    pub fn ambiguous_match(matches: &[(String, String)]) {
        println!(
            "Ambiguous match ({} results). Use provider/model syntax:",
            matches.len()
        );
        for (provider, model) in matches {
            println!("  {provider}/{model}");
        }
    }

    /// No previous sessions.
    pub fn no_sessions() {
        println!("No previous sessions for this project.");
    }

    /// Session list header.
    pub fn sessions_header() {
        println!("  Recent sessions:");
    }

    /// Single session entry in the list.
    pub fn sessions_entry(
        index: usize,
        datetime: &str,
        size_kb: u64,
        entries: usize,
        is_latest: bool,
    ) {
        let latest = if is_latest { "  \u{2190} latest" } else { "" };
        println!("    [{index}] {datetime}  {size_kb:>5} KB  {entries:>3} entries{latest}");
    }

    /// Truncation notice for long session lists.
    pub fn sessions_truncated(remaining: usize) {
        println!("    ... and {remaining} more");
    }

    /// Resume hint.
    pub fn sessions_resume_hint() {
        println!();
        println!("  Resume with: rho -c");
    }

    /// Detailed context window status (`/status` command).
    #[allow(clippy::uninlined_format_args)]
    #[allow(clippy::too_many_arguments)]
    pub fn context_status(
        context_window: usize,
        completion_reserve: usize,
        prompt_budget: usize,
        system_overhead: usize,
        schema_overhead: usize,
        conv_tokens: usize,
        estimated_used: usize,
        estimated_remaining: usize,
        utilization_pct: u8,
        message_count: usize,
        path_entry_count: usize,
        entry_count: usize,
        msg_budget: usize,
        model: &str,
        session_path: Option<&Path>,
    ) {
        println!("  Context Window Status");
        println!("  ─────────────────────");
        println!("  Context window:     {:>8} tokens", context_window);
        println!("  Completion reserve: {:>8} tokens", completion_reserve);
        println!("  Prompt budget:      {:>8} tokens", prompt_budget);
        println!();
        println!("  System prompt:      {:>8} tokens", system_overhead);
        println!("  Tool schemas:       {:>8} tokens", schema_overhead);
        println!(
            "  Conversation:       {:>8} tokens (estimated)",
            conv_tokens
        );
        println!("  ─────────────────────");
        println!("  Estimated used:     {:>8} tokens", estimated_used);
        println!("  Estimated remaining:{:>8} tokens", estimated_remaining);
        println!("  Utilization:        {:>8}%", utilization_pct);
        println!();
        println!("  Messages (fitted):  {:>8}", message_count);
        println!("  Path entries:       {:>8}", path_entry_count);
        println!("  Total entries:      {:>8}", entry_count);
        println!("  Message budget:     {:>8} tokens", msg_budget);
        println!();
        println!("  Model: {model}");
        if let Some(path) = session_path {
            println!("  Session: {}", path.display());
        }
    }

    /// Compact context usage bar after each turn.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn context_bar(
        pct: u8,
        used_k: f64,
        window_k: f64,
        remaining_k: f64,
        message_count: usize,
    ) {
        let filled = (pct as usize * 20 / 100).min(20);
        let empty = 20 - filled;
        let bar: String = "\u{2588}".repeat(filled) + &"\u{2591}".repeat(empty);

        let (color, reset) = if pct < 60 {
            ("\x1b[32m", "\x1b[0m")
        } else if pct < 80 {
            ("\x1b[33m", "\x1b[0m")
        } else {
            ("\x1b[31m", "\x1b[0m")
        };

        println!(
            "{color}[{bar}]{reset} {used_k:.1}k/{window_k:.0}k tokens ({pct}%) │ \
             {remaining_k:.1}k remaining │ {message_count} messages",
        );
    }
}

// ── Paste mode ───────────────────────────────────────────────────────────────

impl ReplPresenter {
    /// Instructions at the start of paste mode.
    pub fn paste_header() {
        println!("  Entering paste mode. Paste your text, then:");
        println!("    • Press Enter twice (empty line) to finish");
        println!("    • Type --- on its own line to finish");
        println!("    • Press Ctrl-D / Ctrl-Z to finish");
    }

    /// Prompt for the first line of paste input.
    pub fn paste_prompt() {
        print!("  paste> ");
        let _ = io::stdout().flush();
    }

    /// Continuation prompt for additional paste lines.
    pub fn paste_continuation() {
        print!("  ...   ");
        let _ = io::stdout().flush();
    }

    /// Paste complete — show line count.
    pub fn paste_complete(lines: usize) {
        println!("  [paste: {lines} line(s)]");
    }
}

// ── Approval gate ────────────────────────────────────────────────────────────

impl ReplPresenter {
    /// Print the tool-call approval prompt.
    pub fn approval_prompt(tool: &str, risk: &str, arguments: &str) {
        println!();
        println!("  Tool     : {tool}");
        println!("  Risk     : {risk}");
        println!("  Arguments: {arguments}");
        print!("  Execute? [y/n] ");
        let _ = io::stdout().flush();
    }
}
