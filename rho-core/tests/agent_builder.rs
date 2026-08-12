//! Integration tests for the `Agent` builder (rho-core public API).
//!
//! These exercise [`rho_core::Agent`] / [`rho_core::AgentBuilder`] from the
//! outside, the way an embedder would. They are the Phase 1 acceptance tests:
//! additive `rho-core` only — the `rho` binary is untouched.

use rho_core::config::RhoConfig;
use rho_core::{Agent, NopObserver};
use rho_test_helpers::{AutoApproveGate, MockChatClient, tempdir_with_sandbox};

/// A freshly built ephemeral agent carries the configured model, holds no
/// persisted session, and registers no tools unless told to.
#[tokio::test]
async fn builder_produces_ephemeral_agent() {
    let (_dir, sandbox) = tempdir_with_sandbox();
    let agent = Agent::builder()
        .config(RhoConfig::default())
        .sandbox(sandbox)
        .ephemeral()
        .model("test-model")
        .build()
        .expect("build should succeed");

    assert_eq!(agent.session().model(), "test-model");
    assert!(
        agent.session().save_path().is_none(),
        "ephemeral session must be in-memory"
    );
    assert!(
        agent.list_tools().is_empty(),
        "no tools registered by default"
    );
}

/// `run_turn` against a mock provider returns the model's reply and records
/// one iteration. This is the embedder "hello world" — no CLI, no config file,
/// no extensions, no tracing subscriber.
#[tokio::test]
async fn run_turn_returns_mock_reply() {
    let (_dir, sandbox) = tempdir_with_sandbox();
    let client = MockChatClient::new(vec![rho_test_helpers::text_events("hello back")]);
    let provider = rho_test_helpers::TestProvider::new("test", client);
    let mut registry = rho_core::ProviderRegistry::new();
    registry.add(Box::new(provider));

    let mut agent = Agent::builder()
        .sandbox(sandbox)
        .ephemeral()
        .model("mock")
        .providers(registry)
        .build()
        .expect("build should succeed");

    let result = agent
        .run_turn(
            "hi",
            &rho_core::TurnInputs {
                observer: &NopObserver,
                gate: &AutoApproveGate,
                steering: None,
            },
        )
        .await
        .expect("turn should succeed");

    assert_eq!(result.reply, "hello back");
    assert_eq!(result.iterations, 1);
}
