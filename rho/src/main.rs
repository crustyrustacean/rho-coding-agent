//! Binary entry point for rho.

use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use rho_core::{
    AgentConfig, ConfigLoader, LocalChatClient, ModelToolCall, RhoConfig, Session, ToolRegistry,
    ToolRisk,
    approval::ApprovalGate,
    base_prompt, compact_prompt,
    context_files::{ContextScanner, TrustStore, compose_system_prompt},
    find_project_root,
    sandbox::SandboxRoot,
    tool::CancellationToken,
};
use rho_tools::register_all;
use std::path::PathBuf;
use std::{
    fs,
    io::{self, BufRead, Write},
};
use tracing_subscriber::EnvFilter;

// ── REPL approval gate ────────────────────────────────────────────────────────

/// Prints a tool-call preview and reads `y/N` from stdin.
struct ReplApprovalGate;

#[async_trait]
impl ApprovalGate for ReplApprovalGate {
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool {
        let risk_label = match risk {
            ToolRisk::Read => "read",
            ToolRisk::Write => "write",
            ToolRisk::Destructive => "destructive",
        };
        eprintln!();
        eprintln!("  Tool     : {}", call.function.name);
        eprintln!("  Risk     : {risk_label}");
        eprintln!("  Arguments: {}", call.function.arguments);
        eprint!("  Execute? [y/n] ");
        io::stderr().flush().ok();

        let mut line = String::new();
        let ok = io::stdin().lock().read_line(&mut line).is_ok();
        ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
}

// ── CLI ───────────────────────────────────────────────────────────────────────

/// rho — a local coding agent.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Model identifier.
    ///
    /// If omitted (and not set in config), rho queries the server's
    /// `/v1/models` endpoint and uses the first loaded model.
    #[arg(short, long)]
    model: Option<String>,

    /// System prompt (overrides the bundled base prompt and context files).
    #[arg(short, long)]
    system: Option<String>,

    /// Use a compact system prompt suitable for models with small context
    /// windows (e.g. 4K tokens). The full prompt (~2,000 tokens) plus tool
    /// schemas and context files may exceed the context length of smaller
    /// models. This flag swaps the full prompt for a minimal version (~100
    /// tokens) that preserves core identity and safety rules.
    #[arg(long)]
    compact: bool,

    /// Project root / sandbox root (defaults to auto-detected project root).
    ///
    /// When omitted, rho walks up from the current directory looking for
    /// project markers (`.rho/config.toml`, `.git/`, `Cargo.toml`, etc.).
    /// Falls back to the current directory if no marker is found.
    #[arg(long)]
    root: Option<std::path::PathBuf>,

    /// Skip the provider consent warning for external endpoints.
    ///
    /// By default, rho displays a consent prompt before connecting to a
    /// non-local model provider. Use this flag to skip the prompt in
    /// automated workflows where consent has been pre-authorized.
    #[arg(long)]
    accept_external_provider: bool,

    /// Context window token budget.
    ///
    /// Controls how many tokens the sliding window retains before evicting
    /// older turns. Overrides the `[agent] token_budget` config value.
    /// Defaults to 32,768.
    #[arg(long)]
    token_budget: Option<u32>,

    /// Prompt file.
    ///
    /// Reads the file contents and passes them to the agent loop,
    /// then exits (no REPL). Useful for automation and testing.
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Resume a previous session from a JSONL file.
    ///
    /// When specified, rho loads the session from the given path instead of
    /// creating a new one. Use this to continue a conversation that was
    /// interrupted or to inspect a session's history.
    #[arg(long)]
    session: Option<PathBuf>,

    /// Run in ephemeral mode — no session file is written to disk.
    ///
    /// All conversation state lives only in memory and is lost when rho
    /// exits. Useful for one-shot commands, CI pipelines, or when you
    /// don't want `.rho/sessions/` clutter.
    #[arg(long)]
    ephemeral: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    // --- Tracing ---
    let file_appender = tracing_appender::rolling::never("logs", "rho.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,rustls=warn,hyper=warn,reqwest=warn")),
        )
        .init();

    let cli = Cli::parse();

    // --- Sandbox root ---
    let sandbox = match cli.root {
        Some(ref p) => SandboxRoot::new(p).map_err(|e| {
            anyhow::anyhow!("cannot establish sandbox root at `{}`: {e}", p.display())
        })?,
        None => find_project_root()
            .map_err(|e| anyhow::anyhow!("cannot auto-detect project root: {e}"))?,
    };

    // --- Config ---
    let rho_config = ConfigLoader::load(sandbox.path()).unwrap_or_else(|e| {
        eprintln!("Warning: {e} — using defaults");
        RhoConfig::default()
    });

    // --- Provider compatibility check ---
    // rho only speaks the OpenAI Chat Completions wire format.
    // Warn if the configured endpoint looks like a non-OpenAI-compatible API
    // (Anthropic Messages API, Google Gemini, etc.) — these require a proxy
    // that translates to OpenAI format.
    let endpoint = rho_config
        .provider
        .endpoint
        .as_deref()
        .unwrap_or("http://localhost:1234/v1/chat/completions");

    if let Some(ref provider_type) = rho_config.provider.r#type {
        let non_openai = [
            "anthropic",
            "google",
            "gemini",
            "cohere",
            "mistral",
            "together",
            "anyscale",
            "perplexity",
            "bedrock",
            "vertex",
        ];
        if non_openai
            .iter()
            .any(|t| provider_type.eq_ignore_ascii_case(t))
        {
            eprintln!(
                "warning: provider type \"{provider_type}\" was set, but rho only supports \
                 OpenAI-compatible endpoints (the Chat Completions API wire format). \
                 {endpoint}"
            );
        }
    }
    if !endpoint.contains("/chat/completions") && !is_local_endpoint(endpoint) {
        eprintln!(
            "warning: endpoint \"{endpoint}\" does not end with /chat/completions, \
             which is the standard OpenAI-compatible path. rho sends requests in \
             the OpenAI Chat Completions format. If this endpoint uses a different \
             API format (e.g. Anthropic Messages, Google GenerateContent), \
             requests will fail. Use an OpenAI-compatible proxy or verify the endpoint."
        );
    }
    // --- Client (with provider consent check) ---
    // Consent is checked BEFORE the trust workflow so the user can bail out
    // before being prompted about context files.
    check_provider_consent(endpoint, &cli)?;

    // --- Tool registry ---
    let mut registry = ToolRegistry::new();
    register_all(&mut registry, sandbox.clone(), &rho_config);

    // --- Project context files ---
    let system_prompt = load_system_prompt(&sandbox, &cli);

    let client = resolve_api_key(&rho_config).map_or_else(
        || LocalChatClient::with_endpoint_and_egress(endpoint, rho_config.egress.clone()),
        |key| {
            LocalChatClient::with_endpoint_egress_and_key(
                endpoint,
                rho_config.egress.clone(),
                Some(key),
            )
        },
    );

    // --- Session ---
    let model = resolve_model(&rho_config, cli.model.as_ref(), &client).await?;
    let config = AgentConfig::from_config(&rho_config);

    // --- Secret redaction ---
    let redactor = rho_core::Redactor::from_config(
        rho_config.redaction.enabled,
        &rho_config.redaction.custom_patterns,
    );

    let token_budget = rho_core::TokenBudget::new(
        cli.token_budget.unwrap_or(rho_config.agent.token_budget) as usize,
    );

    // --- Session ---
    let mut session = if let Some(path) = &cli.session {
        // Resume an existing session from JSONL
        eprintln!("resuming session from: {}", path.display());
        let mut s =
            Session::open(path).map_err(|e| anyhow::anyhow!("failed to open session: {e}"))?;

        // Detect stale CWD: the session was created in a different directory
        // than the current one. Tools use the current sandbox root, so the
        // model should be told the truth.
        let session_cwd = s.header().cwd.clone();
        let current_cwd = sandbox.path();
        if session_cwd != current_cwd {
            if !session_cwd.as_os_str().is_empty() && !session_cwd.exists() {
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

        // Apply the resolved model, budget, redactor, and tools
        s.set_model(&model);
        s.set_token_budget(token_budget);
        s.set_redactor(redactor);
        s.set_tools(registry.tool_schemas());
        s
    } else if cli.ephemeral {
        // In-memory mode: no disk persistence
        Session::in_memory(
            model,
            Some(&system_prompt),
            registry.tool_schemas(),
            sandbox.path(),
        )
        .with_token_budget(token_budget)
        .with_redactor(redactor)
    } else {
        // Default: persisted session with auto-flush
        Session::new(
            model,
            Some(&system_prompt),
            registry.tool_schemas(),
            sandbox.path(),
        )
        .with_token_budget(token_budget)
        .with_redactor(redactor)
    };

    if let Some(path) = session.save_path() {
        eprintln!("session: {}", path.display());
    }

    // --- Startup budget diagnostics ---
    log_budget_diagnostics(&session);

    let gate = ReplApprovalGate;
    let cancel = CancellationToken::new();

    // pass prompt file directly to the agent loop
    if let Some(path) = &cli.prompt_file {
        let input = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read prompt file `{}`: {e}", path.display()))?;
        eprintln!("using prompt file: {}", path.display());
        match rho_core::run_loop(
            &mut session,
            &input,
            &client,
            &registry,
            &config,
            cancel.clone(),
            &gate,
        )
        .await
        {
            Ok(reply) => println!("Assistant: {reply}"),
            Err(e) => eprintln!("Error: {e}"),
        }

        session.close("prompt file completed");
        return Ok(());
    }

    // --- REPL loop ---

    loop {
        print!("User: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        match input {
            "/quit" | "quit" => {
                session.close("user quit");
                break;
            }
            "/clear" => {
                // Branch back to the system message — same effect as
                // clearing the conversation, but the old tree is preserved
                // on disk so it can be inspected or resumed later.
                let path = session.path_to_root();
                if let Some(root_entry) = path.last() {
                    let root_id = root_entry.id.clone();
                    let _ = session.branch_to(&root_id);
                }
                println!("[conversation cleared]");
                continue;
            }
            "" => continue,
            _ => {}
        }

        match rho_core::run_loop(
            &mut session,
            input,
            &client,
            &registry,
            &config,
            cancel.clone(),
            &gate,
        )
        .await
        {
            Ok(reply) => println!("Assistant: {reply}"),
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    Ok(())
}

// ── Startup helpers ────────────────────────────────────────────────────────────

/// Log token budget diagnostics at startup.
///
/// Displays the budget breakdown so the user knows how much context is
/// available for conversation after system prompt and tool-schema overhead.
/// Warns when overhead consumes more than half the prompt budget.
fn log_budget_diagnostics(session: &rho_core::Session) {
    let budget = session.token_budget();
    let system = session.system_overhead();
    let schema = session.schema_overhead();
    let total_overhead = system + schema;
    let prompt = budget.prompt_budget();
    let available = session.message_budget();

    eprintln!(
        "budget: {}T context, {}T reserve, {}T prompt \
         ({}T system + {}T schema = {}T overhead, {}T for conversation)",
        budget.context_window,
        budget.completion_reserve,
        prompt,
        system,
        schema,
        total_overhead,
        available,
    );

    if total_overhead > prompt / 2 {
        #[allow(clippy::cast_possible_truncation)]
        let pct = (100_usize.saturating_mul(total_overhead) / prompt.max(1)) as u32;
        eprintln!(
            "warning: system overhead is {pct}% of prompt budget — \
             consider --compact or increasing token_budget in .rho/config.toml"
        );
    }
}

/// Resolve the model identifier.
///
/// Priority: config `agent.model` → CLI `--model` → auto-detect via `/v1/models`.
///
/// Returns an error if auto-detection is needed but the server is unreachable
/// or has no models loaded.
async fn resolve_model(
    config: &RhoConfig,
    cli_model: Option<&String>,
    client: &LocalChatClient,
) -> Result<String> {
    // 1. Config takes highest priority.
    if let Some(model) = config.agent.model.as_deref() {
        eprintln!("using model from config: {model}");
        return Ok(model.to_owned());
    }
    // 2. CLI flag.
    if let Some(model) = cli_model {
        eprintln!("using model from --model: {model}");
        return Ok(model.to_owned());
    }
    // 3. Auto-detect from the server.
    // Auto-detection is best-effort: it works reliably for local servers
    // (LM Studio, Ollama) but may fail or return unexpected results for
    // external providers. When using an external provider, always specify
    // the model explicitly with --model or in config.
    eprintln!("no model specified, querying server for loaded models...");
    let configured_endpoint = config.provider.endpoint.as_deref().unwrap_or("");
    let list = client.list_models().await.map_err(|e| {
        // Tailor the error hint to whether the endpoint is local or external.
        let hint = if is_local_endpoint(configured_endpoint) {
            "Load a model in your local server and try again."
        } else {
            "This may indicate the endpoint doesn't support /v1/models, \
                 requires authentication, or uses a non-standard model list. \
                 Specify the model explicitly with --model or in config."
        };
        anyhow::anyhow!("cannot query /v1/models: {e}\n  {hint}")
    })?;
    if list.data.is_empty() {
        let hint = if is_local_endpoint(configured_endpoint) {
            "Load a model in your local server and try again."
        } else {
            "The server returned an empty model list. Specify the model \
             explicitly with --model or in config."
        };
        anyhow::bail!("no models loaded on the server. {hint}");
    }
    let model = &list.data[0].id;
    eprintln!("auto-detected model: {model}");
    Ok(model.clone())
}

/// Load the system prompt from CLI override or project context files.
///
/// Appends the project root as the model's working directory so the model
/// knows its absolute path and that each `run_command` starts a fresh
/// process (i.e. `cd` does not persist between commands).
fn load_system_prompt(sandbox: &SandboxRoot, cli: &Cli) -> String {
    let mut prompt = if let Some(custom) = &cli.system {
        custom.clone()
    } else {
        let prompt_base = if cli.compact {
            compact_prompt()
        } else {
            base_prompt()
        };
        let mut trust_store = TrustStore::load_default();
        let scanner = ContextScanner::new(sandbox);
        let mut stdout = io::stdout();
        let stdin = io::stdin();
        let mut stdin_locked = stdin.lock();
        let context_files = scanner.run(&mut trust_store, &mut stdin_locked, &mut stdout);
        compose_system_prompt(prompt_base, &context_files)
    };

    // Append the working directory so the model knows its absolute path.
    // Each run_command starts a fresh process in this directory —
    // cd / Set-Location does not persist between invocations.
    let root = sandbox.path().display();
    #[allow(clippy::format_push_string)]
    prompt.push_str(&format!(
        "\n\n# Environment\n\n\
         - Working directory (project root): `{root}`\n\
         - All relative file paths and shell commands resolve from this directory.\n\
         - Each `run_command` invocation starts a fresh process in this directory.\n\
           `cd` and `Set-Location` do not persist between commands — include the\n\
           full relative path from the project root in every command."
    ));

    // Append Rust tooling guidance when Rust tools are available.
    prompt.push_str(
        "\n\n# Rust Tooling\n\n\
         - You have access to structured Rust compiler diagnostics via `cargo_check` and `cargo_clippy`.\n\
         - When code fails to compile, use `cargo_check` before attempting manual fixes.\n\
         - Trust machine-applicable suggestions from the compiler — apply them with `cargo_fix` or by\n\
           using the suggested replacement text in `edit_file`.\n\
         - Use `cargo_clippy` for code-quality lints beyond compilation errors.\n\
         - Use `rustc_explain` to look up detailed explanations for error codes (e.g. E0308).\n\
         - Use `cargo_test` to verify fixes — run the relevant tests after each change.\n\
         - Prefer the structured diagnostic tools over `run_command` with raw `cargo check` —\n\
           the tools parse JSON output and surface only actionable workspace diagnostics.",
    );

    prompt
}

/// Display a consent warning and read confirmation for external providers.
///
/// Returns `Ok(())` if the user consents or if the provider is local.
/// Returns `Ok(())` without prompting if `--accept-external-provider` is set.
/// Prints a message and returns `Err` if the user declines.
fn check_provider_consent(endpoint: &str, cli: &Cli) -> Result<()> {
    if is_local_endpoint(endpoint) || cli.accept_external_provider {
        return Ok(());
    }
    eprintln!();
    eprintln!("  ⚠  External provider detected");
    eprintln!("      Endpoint: {endpoint}");
    eprintln!();
    eprintln!("      Your prompts and code will be sent to an external server.");
    eprintln!("      This may expose proprietary code, secrets, or other");
    eprintln!("      sensitive data to the provider and any intermediaries.");
    eprintln!();
    eprint!("      Continue? [y/N] ");
    io::stderr().flush().ok();

    let mut line = String::new();
    let ok = io::stdin().lock().read_line(&mut line).is_ok();
    if ok && matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        eprintln!("  Aborting. Use --accept-external-provider to skip this prompt.");
        Err(anyhow::anyhow!("user declined external provider consent"))
    }
}

// ── Provider detection ─────────────────────────────────────────────────────────

/// Resolve the API key from the provider configuration.
///
/// Reads the environment variable named in `provider.api_key_env` and returns
/// the value. Returns `None` if no env var is configured or the variable is
/// not set.
fn resolve_api_key(rho_config: &RhoConfig) -> Option<String> {
    let env_var = rho_config.provider.api_key_env.as_deref()?;
    let key = std::env::var(env_var).ok()?;
    if key.is_empty() { None } else { Some(key) }
}

/// Determine whether an endpoint URL points to a local address.
///
/// A local endpoint is one whose host is `localhost`, `127.0.0.1`, or `::1`.
/// Any other host is considered external and triggers the consent warning.
///
/// Uses `url::Url` parsing so that crafted hostnames like
/// `api.localhost-fake.evil.com` are correctly classified as external.
fn is_local_endpoint(endpoint: &str) -> bool {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .is_some_and(|h| matches!(h.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoint_localhost() {
        assert!(is_local_endpoint(
            "http://localhost:1234/v1/chat/completions"
        ));
    }

    #[test]
    fn local_endpoint_127_0_0_1() {
        assert!(is_local_endpoint(
            "http://127.0.0.1:1234/v1/chat/completions"
        ));
    }

    #[test]
    fn local_endpoint_ipv6_loopback() {
        assert!(is_local_endpoint("http://[::1]:1234/v1/chat/completions"));
    }

    #[test]
    fn external_endpoint_openai() {
        assert!(!is_local_endpoint(
            "https://api.openai.com/v1/chat/completions"
        ));
    }

    #[test]
    fn external_endpoint_anthropic() {
        assert!(!is_local_endpoint("https://api.anthropic.com/v1/messages"));
    }

    #[test]
    fn local_endpoint_case_insensitive() {
        assert!(is_local_endpoint(
            "http://LocalHost:1234/v1/chat/completions"
        ));
    }

    #[test]
    fn local_endpoint_rejects_localhost_subdomain() {
        // A crafted hostname containing "localhost" as a substring
        // must NOT be classified as local.
        assert!(!is_local_endpoint(
            "https://api.localhost-fake.evil.com/v1/chat/completions"
        ));
    }
}
