//! src/bin/main.rs
//! Binary entry point for rho-coding-agent.

use anyhow::Result;
use rho_core::{Conversation, RhoHttpClient};
use std::io::{self, Write};

/// Model identifier sent with each chat completion request.
const MODEL: &str = "qwen3-8b";
/// System prompt prepended to every conversation.
const SYSTEM_PROMPT: Option<&str> =
    Some("You are an expert in the Rust programming language and its associated ecosystem.");

#[tokio::main]
async fn main() -> Result<()> {
    let rho_http_client = RhoHttpClient::new();
    let mut conversation = Conversation::new(MODEL.to_string(), SYSTEM_PROMPT);

    loop {
        print!("User: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if input.trim() == "quit" {
            break;
        }

        let response = conversation.send(input.trim(), &rho_http_client).await?;
        print!("Agent: ");
        print!("{response}");
        println!();
    }

    Ok(())
}
