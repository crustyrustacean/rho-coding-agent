//! Binary entry point for rho.

use anyhow::Result;
use async_trait::async_trait;
use clap::Parser;
use rho_core::{
    AgentConfig, Conversation, LocalChatClient, ModelToolCall, ToolRegistry, ToolRisk,
    approval::ApprovalGate,
    base_prompt,
    context_files::{ContextScanner, TrustStore, compose_system_prompt},
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
    #[arg(short, long, default_value = "qwen3-8b")]
    model: String,

    /// System prompt (overrides the bundled base prompt and context files).
    #[arg(short, long)]
    system: Option<String>,

    /// Project root / sandbox root (defaults to the current directory).
    #[arg(long, default_value = ".")]
    root: std::path::PathBuf,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // --- Sandbox root ---
    let sandbox = SandboxRoot::new(&cli.root).map_err(|e| {
        anyhow::anyhow!(
            "cannot establish sandbox root at `{}`: {e}",
            cli.root.display()
        )
    })?;

    // --- Tool registry ---
    let mut registry = ToolRegistry::new();
    register_all(&mut registry, sandbox.clone());

    // --- Project context files ---
    let system_prompt = if let Some(custom) = &cli.system {
        custom.clone()
    } else {
        let cwd_sandbox = SandboxRoot::new(&cli.root)
            .unwrap_or_else(|_| SandboxRoot::new(".").expect("current dir must exist"));
        let mut trust_store = TrustStore::load_default();
        let scanner = ContextScanner::new(&cwd_sandbox);
        let mut stdout = io::stdout();
        let stdin = io::stdin();
        let mut stdin_locked = stdin.lock();
        let context_files = scanner.run(&mut trust_store, &mut stdin_locked, &mut stdout);
        compose_system_prompt(base_prompt(), &context_files)
    };

    // --- Conversation ---
    let client = LocalChatClient::new();
    let config = AgentConfig::default();
    let mut conversation =
        Conversation::new(cli.model, Some(&system_prompt), registry.tool_schemas());

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
