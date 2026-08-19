# Embedding `rho-core`

> A runnable version of the example below lives at `rho-core/examples/agent_hello.rs` (`cargo run -p rho-core --example agent_hello`).

`rho-core` is an embeddable library: `Agent` (built via `AgentBuilder`) owns the long-lived agent state and exposes a programmatic API, so every consumer — the `rho` binary's RPC loop, a Makepad or web frontend, headless scripts, benches — is a thin wrapper. Construction is decoupled from CLI flags, config files, and stdio.

```rust
use rho_core::{Agent, AgentBuilder, TurnInputs};

let mut agent = Agent::builder()
    .config(cfg)              // or .load_config_at(sandbox), or pure setters
    .ephemeral()              // in-memory session (also: persisted / resume / continue_last)
    .model("gpt-4o")
    .tools(tool_registry)     // host registers tools/extensions/memory
    .build()?;                // -> Result<Agent, AgentBuildError>

// Headless / embedder default (auto-approve gate, no-op observer, no steering):
let result = agent.run("list the source files").await?;   // -> AgentResult

// Frontend path: inject a UI observer, an approval gate, steering, and a
// per-turn cancel token via TurnInputs.
let result = agent.run_turn(prompt, &TurnInputs {
    observer: &my_ui,
    gate: &my_approval_gate,
    steering: Some(&my_steering_queue),
    cancel: cancel_token,
}).await?;
```

**Separation principle.** The kernel owns *"I am a configured agent; run a prompt."* Host-only concerns stay *out* of `rho-core` and are composed on top by the host (`App` in the `rho` binary): the TypeScript extension runtime (`rho-ext`), the tracing subscriber, interactive consent prompts, and startup banners. Per-turn, per-connection inputs (observer / approval gate / steering / cancel token) are supplied via `TurnInputs` because they are tied to a UI connection and may change between turns (e.g. on extension reload). `rho-core` therefore has no dependency on `rho-ext` or `tracing-appender`.

**Consent / presenter seam.** External-provider consent is enforced inside the builder: if external providers are configured and consent was not granted (`.accept_external_consent()`), `build()` returns `AgentBuildError::ExternalConsentRequired`. The kernel emits diagnostics through `tracing` and returns structured errors; the host formats human-facing banners (the `rho` binary uses `RpcPresenter` to stderr, separate from the JSON-RPC stdout channel).

**Abort / reset.** The cancel token in `TurnInputs` is per-turn by design — the RPC layer swaps a fresh token in before each turn so a prior `abort` doesn't poison the next. Headless `run()` clones the agent's own token.
