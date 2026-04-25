//! src/bin/main.rs
//! Binary entry point for rho-coding-agent.

use anyhow::Result;
use rho_core::{ChatMessage, ChatRequest, RhoHttpClient, Role};

#[tokio::main]
async fn main() -> Result<()> {
    let rho_http_client = RhoHttpClient::new();

    let chat_request = ChatRequest {
        model: "qwen2.5-coder-14b".to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: "Write a `hello_world` program in Rust.".to_string(),
        }],
    };

    let chat_response = rho_http_client.chat(&chat_request).await?;

    println!("{}", chat_response.choices[0].message.content);

    Ok(())
}
