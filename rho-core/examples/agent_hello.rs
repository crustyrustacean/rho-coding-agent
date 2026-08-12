//! Embedder "hello world".
//!
//! Builds a [`rho_core::Agent`] entirely from code — no CLI, no config file,
//! no tracing subscriber, no extensions — and runs one prompt against a mock
//! provider. This is the acceptance proof that the kernel is embeddable: every
//! consumer (the `rho` binary, a Makepad or web frontend, headless scripts,
//! benches) is a thin wrapper around this same API.
//!
//! Run with: `cargo run -p rho-core --example agent_hello`
//!
//! To talk to a real model instead, drop the mock [`TestProvider`] and
//! configure providers/models via `AgentBuilder::config` (a `RhoConfig`,
//! e.g. loaded from `~/.rho/config.toml`) or the `endpoint` / `api_key_env` /
//! `model` setters. See `ARCHITECTURE.md` → "Embedding `rho-core`".

use rho_core::Agent;
use rho_test_helpers::{MockChatClient, TestProvider, text_events};

#[tokio::main]
async fn main() {
    // A mock provider that streams a single text reply (no network).
    let client = MockChatClient::new(vec![text_events("Hello from rho-core!")]);
    let mut providers = rho_core::ProviderRegistry::new();
    providers.add(Box::new(TestProvider::new("demo", client)));

    // Pure programmatic construction: in-memory session, no disk I/O.
    let mut agent = Agent::builder()
        .ephemeral()
        .model("demo-model")
        .providers(providers)
        .build()
        .expect("agent build should succeed");

    // Headless default: a no-op observer, an auto-approve gate, no steering.
    // A frontend would instead call `agent.run_turn(prompt, &TurnInputs { .. })`
    // to inject its own observer, approval gate, steering source, and a per-turn
    // cancel token (see ARCHITECTURE.md → "Embedding `rho-core`").
    let result = agent.run("Say hello.").await.expect("turn should succeed");

    println!("reply:      {}", result.reply);
    println!("iterations: {}", result.iterations);
    println!("finish:     {:?}", result.finish_reason);
}
