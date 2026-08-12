//! Integration tests for the `Agent` builder (rho-core public API).
//!
//! These exercise [`rho_core::Agent`] / [`rho_core::AgentBuilder`] from the
//! outside, the way an embedder would — the public-API characterization for
//! the embeddable orchestration core.

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
                cancel: rho_core::tool::CancellationToken::new(),
            },
        )
        .await
        .expect("turn should succeed");

    assert_eq!(result.reply, "hello back");
    assert_eq!(result.iterations, 1);
}

// ── Phase 3: public-API characterization ────────────────────────────────────

/// Build an ephemeral agent backed by a `TestProvider` serving `mock-model`,
/// with the given tool registry. Returns the agent and the tempdir (kept alive
/// so the sandbox path stays valid for the test).
fn mock_agent(registry: rho_core::ToolRegistry) -> (rho_core::Agent, tempfile::TempDir) {
    let (dir, sandbox) = tempdir_with_sandbox();
    let provider = rho_test_helpers::TestProvider::new("test", MockChatClient::new(vec![]));
    let mut providers = rho_core::ProviderRegistry::new();
    providers.add(Box::new(provider));
    let agent = Agent::builder()
        .sandbox(sandbox)
        .ephemeral()
        .model("mock")
        .providers(providers)
        .tools(registry)
        .build()
        .expect("agent build should succeed");
    (agent, dir)
}

/// `switch_model` to a model the provider advertises updates the session.
#[tokio::test]
async fn switch_model_switches_to_advertised_model() {
    let (mut agent, _dir) = mock_agent(rho_core::ToolRegistry::new());
    agent
        .switch_model("mock-model")
        .await
        .expect("provider advertises mock-model");
    assert_eq!(agent.session().model(), "mock-model");
}

/// `switch_model` with a bare id no provider advertises → `ModelNotFound`.
#[tokio::test]
async fn switch_model_rejects_unknown_model() {
    let (mut agent, _dir) = mock_agent(rho_core::ToolRegistry::new());
    let err = agent
        .switch_model("no-such-model")
        .await
        .expect_err("unknown model should be rejected");
    assert!(matches!(
        err,
        rho_core::SwitchModelError::ModelNotFound { ref model } if model == "no-such-model"
    ));
}

/// `switch_model` with `provider:model` naming an unconfigured provider →
/// `UnknownProvider`.
#[tokio::test]
async fn switch_model_rejects_unknown_provider() {
    let (mut agent, _dir) = mock_agent(rho_core::ToolRegistry::new());
    let err = agent
        .switch_model("acme:acme-7b")
        .await
        .expect_err("unknown provider should be rejected");
    assert!(matches!(
        err,
        rho_core::SwitchModelError::UnknownProvider { ref provider, ref model }
            if provider == "acme" && model == "acme-7b"
    ));
}

/// A fresh agent's context stats report exactly the system message.
#[test]
fn context_stats_fresh_agent_has_only_system_message() {
    let (agent, _dir) = mock_agent(rho_core::ToolRegistry::new());
    let stats = agent.context_stats();
    // The composed system prompt is the single entry on a fresh session.
    assert_eq!(stats.message_count, 1);
}

/// `list_tools` reflects the registry passed at build time.
#[test]
fn list_tools_reflects_registry() {
    let registry =
        rho_test_helpers::fixed_registry("echo", "echo".into(), rho_core::tool::ToolRisk::Read);
    let (agent, _dir) = mock_agent(registry);
    let tools = agent.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
}

/// The `providers` / `registry` / `registry_mut` accessors expose the live
/// state (used by the RPC handlers and for runtime reload).
#[tokio::test]
async fn accessors_expose_live_state() {
    let registry =
        rho_test_helpers::fixed_registry("first", "x".into(), rho_core::tool::ToolRisk::Read);
    let (mut agent, _dir) = mock_agent(registry);

    // providers: the injected TestProvider.
    assert_eq!(agent.providers().providers().len(), 1);
    assert_eq!(agent.active_provider().name(), "test");

    // registry: the tool registered at build.
    assert_eq!(agent.registry().tool_definitions().len(), 1);

    // registry_mut: register a new tool at runtime, observe it via registry().
    agent
        .registry_mut()
        .register(Box::new(rho_test_helpers::FixedResponseTool {
            name: "second",
            response: "y".into(),
            risk: rho_core::tool::ToolRisk::Read,
        }));
    assert_eq!(agent.registry().tool_definitions().len(), 2);
}
