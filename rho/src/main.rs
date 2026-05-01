//! Binary entry point for rho.

use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use rho_core::{
    AgentConfig, ConfigLoader, Conversation, LocalChatClient, ModelToolCall, RhoConfig,
    ToolRegistry, ToolRisk,
    approval::ApprovalGate,
    base_prompt, compact_prompt,
    context_files::{ContextScanner, TrustStore, compose_system_prompt},
    find_project_root,
    sandbox::SandboxRoot,
    tool::CancellationToken,
};
use rho_tools::register_all;
use std::io::{self, BufRead, Write};

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
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
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

    // --- Tool registry ---
    let mut registry = ToolRegistry::new();
    register_all(&mut registry, sandbox.clone(), Some(&rho_config));

    // --- Project context files ---
    let system_prompt = load_system_prompt(&sandbox, &cli);

    // --- Client (with provider consent check) ---
    let endpoint = rho_config
        .provider
        .endpoint
        .as_deref()
        .unwrap_or("http://localhost:1234/v1/chat/completions");

    check_provider_consent(endpoint, &cli)?;

    let client = LocalChatClient::with_endpoint_and_egress(endpoint, rho_config.egress.clone());

    // --- Conversation ---
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

    let mut conversation = Conversation::new(model, Some(&system_prompt), registry.tool_schemas())
        .with_token_budget(token_budget)
        .with_redactor(redactor);

    // --- REPL loop ---
    let gate = ReplApprovalGate;

    loop {
        print!("User: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        match input {
            "/quit" | "quit" => break,
            "/clear" => {
                conversation.clear();
                println!("[conversation cleared]");
                continue;
            }
            "" => continue,
            _ => {}
        }

        let cancel = CancellationToken::new();
        match rho_core::run_loop(
            &mut conversation,
            input,
            &client,
            &registry,
            &config,
            cancel,
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
    eprintln!("no model specified, querying server for loaded models...");
    let list = client
        .list_models()
        .await
        .map_err(|e| anyhow::anyhow!("cannot query /v1/models: {e}"))?;
    if list.data.is_empty() {
        anyhow::bail!("no models loaded on the server. Load a model in LM Studio and try again.");
    }
    let model = &list.data[0].id;
    eprintln!("auto-detected model: {model}");
    Ok(model.clone())
}

/// Load the system prompt from CLI override or project context files.
fn load_system_prompt(sandbox: &SandboxRoot, cli: &Cli) -> String {
    if let Some(custom) = &cli.system {
        return custom.clone();
    }
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

/// Determine whether an endpoint URL points to a local address.
///
/// A local endpoint is one whose host is `localhost`, `127.0.0.1`, or `::1`.
/// Any other host is considered external and triggers the consent warning.
///
/// Uses simple string matching rather than full URL parsing to avoid pulling
/// in the `url` or `reqwest` crates at the binary level.
fn is_local_endpoint(endpoint: &str) -> bool {
    // Check for localhost, 127.0.0.1, or [::1] in the authority portion.
    let lower = endpoint.to_lowercase();
    lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("[::1]")
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
}
