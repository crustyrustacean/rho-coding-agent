//! src/bin/main.rs
//! Binary entry point for rho-coding-agent.

use anyhow::Result;
use clap::Parser;
use rho_core::{AssistantResponse, Conversation, RhoHttpClient};
use std::io::{self, Write};

/// A coding agent powered by local LLMs.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Model identifier to use for chat completion requests.
    #[arg(short, long, default_value = "qwen3-8b")]
    model: String,

    /// System prompt prepended to every conversation.
    #[arg(short, long)]
    system: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let rho_http_client = RhoHttpClient::new();
    let mut conversation = Conversation::new(cli.model, cli.system.as_deref(), vec![]);

    loop {
        print!("User: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if input.trim() == "quit" {
            break;
        }

        let response = conversation.send(input.trim(), &rho_http_client).await?;
        match response {
            AssistantResponse::Message(msg) => println!("Assistant: {msg}"),
            AssistantResponse::ToolCall { name, arguments } => println!(
                "Model wants to call: Tool: {name} with arguments: {arguments}"
            ),
        }
    }

    Ok(())
}
