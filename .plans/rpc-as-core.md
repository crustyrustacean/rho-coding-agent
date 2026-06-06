# Plan: RPC as Core — Extract Shared Agent Orchestration

## Goal

Eliminate the duplication between `repl.rs` and `rpc.rs` by extracting the shared
agent-turn orchestration into a new `frontend.rs` module. Both the REPL and RPC
modes will call the same function to drive an agent turn, differing only in
how they render results and handle approval.

## Current Duplication (Tally)

The following code block appears **three times** (twice in `repl.rs` for normal
input and `/paste`, once in `rpc.rs`):

```rust
let client = app.active_provider().clone_boxed_service();
let compaction_client = if app.config.compaction_mode == "llm" {
    Some(std::sync::Arc::from(app.active_provider().clone_boxed_service()))
} else {
    None
};
let composite = CompositeObserver::new(/* ... */);
let params = rho_core::LoopParams {
    client: client.as_ref(),
    registry: &app.registry,
    config: &app.config,
    cancel: app.cancel.clone(),
    gate: &gate,
    observer: &composite,
    compaction_client,
};
match rho_core::run_loop(&mut app.session, message, &params).await {
    Ok(reply) => /* render */,
    Err(e) => /* render error */,
}
```

Additionally, `handle_get_session_stats` (RPC) and `show_context_stats` (REPL)
read the same data from `session.context_stats()` and differ only in rendering.

And `handle_set_model` (RPC) and `switch_model` (REPL) share the core logic of
`session.set_model()` + `ext_loader.set_model_all()`, differing in how the model
is resolved (literal ID vs fuzzy match) and rendered.

## Architecture

```
                       rho_core::run_loop()
                              │
                    ┌─────────▼──────────┐
                    │   App::run_turn()   │  ← NEW in app.rs
                    │                     │
                    │  - clone provider   │
                    │  - build composite  │
                    │  - build LoopParams │
                    │  - call run_loop()  │
                    │  - return TurnResult│
                    └─────────┬──────────┘
                              │
              ┌───────────────┼───────────────┐
              │               │               │
     ┌────────▼───────┐ ┌────▼──────────┐ ┌───▼──────────┐
     │   ReplObserver  │ │  RpcObserver  │ │ TuiObserver  │
     │  (render text) │ │ (write JSONL) │ │ (future)     │
     └────────────────┘ └───────────────┘ └──────────────┘
              │               │
     ┌────────▼───────┐ ┌────▼──────────┐
     │ ReplApproval   │ │ RpcApproval   │
     │ Gate (stdin y/N)│ │ Gate (JSONL)  │
     └────────────────┘ └───────────────┘
```

## Step-by-step Plan (TDD)

### Step 1: Define `TurnResult` and `App::run_turn()` — RED phase

**File:** `rho/src/app.rs`

Add a `TurnResult` enum and a `run_turn()` method on `App`:

```rust
/// The outcome of a single agent turn.
pub(crate) enum TurnResult {
    /// Agent produced a text reply.
    Reply(String),
    /// Agent loop encountered an error.
    Error(String),
}
```

Add `run_turn` as a private method that currently just returns
`TurnResult::Error("not implemented")`. This is the RED test.

**Tests (in `rho/src/app.rs` `#[cfg(test)] mod tests`):**
- `run_turn_returns_error_when_not_implemented` — calls `run_turn()`, asserts
  it returns `TurnResult::Error`

**Rationale:** Write the test first so the compiler sees `TurnResult` and
`run_turn` in the same compilation unit. `run_turn` can be `pub(crate)` since
both `repl.rs` and `rpc.rs` are in the same crate.

### Step 2: Implement `App::run_turn()` — GREEN phase

**File:** `rho/src/app.rs`

Implement `run_turn` to match the duplicated logic:

```rust
pub(crate) async fn run_turn(
    &mut self,
    message: &str,
    gate: &dyn ApprovalGate,
    observer: &dyn AgentObserver,
) -> TurnResult {
    let client = self.active_provider().clone_boxed_service();
    let compaction_client = if self.config.compaction_mode == "llm" {
        Some(std::sync::Arc::from(self.active_provider().clone_boxed_service()))
    } else {
        None
    };
    let params = rho_core::LoopParams {
        client: client.as_ref(),
        registry: &self.registry,
        config: &self.config,
        cancel: self.cancel.clone(),
        gate,
        observer,
        compaction_client,
    };
    match rho_core::run_loop(&mut self.session, message, &params).await {
        Ok(reply) => TurnResult::Reply(reply),
        Err(e) => TurnResult::Error(e.to_string()),
    }
}
```

**Note:** The caller is responsible for building the composite observer (which
includes extension observers). This keeps `run_turn` focused on the orchestration
concern and lets each frontend compose observers however it wants.

**Tests:**
- `run_turn_text_only_returns_reply` — mock client returns text; assert
  `TurnResult::Reply("hello")`
- `run_turn_tool_call_returns_reply` — mock returns tool call then text; assert
  `TurnResult::Reply` with the final text
- `run_turn_error_returns_error_string` — mock returns HTTP 403; assert
  `TurnResult::Error` containing "403"
- `run_turn_max_iterations_returns_error` — set `max_iterations = 1` with
  infinite-tool-call mock; assert `TurnResult::Error` containing "maximum
  iterations"
- `run_turn_session_persists_messages` — send "hello", assert session has
  system + user + assistant messages in `path_messages()`
- `run_turn_multiple_turns_accumulate` — send "hello" then "world", assert
  5 messages in session

**Test infrastructure:** Use the same `test_app()` pattern from `rpc.rs` tests
(Make it `pub(crate)` or extract it to a shared test helpers module in
`rho/`). The mock client setup (`MockChatClient`, `FixedResponseTool`,
`text_events`, etc.) from `rho-test-helpers` is already shared.

### Step 3: Add `App::get_session_stats()` — shared stats query

**File:** `rho/src/app.rs`

```rust
/// Token budget and context-window usage statistics.
#[derive(Debug, Clone)]
pub(crate) struct SessionStats {
    pub context_window: usize,
    pub completion_reserve: usize,
    pub estimated_used: usize,
    pub estimated_remaining: usize,
    pub utilization_percent: u8,
    pub message_count: usize,
    pub entry_count: usize,
    pub path_entry_count: usize,
    pub compacted_entry_count: usize,
    pub compaction_tokens: usize,
    pub role_tokens: RoleTokenStats,
    pub resolution_tokens: ResolutionTokenStats,
}

#[derive(Debug, Clone)]
pub(crate) struct RoleTokenStats { pub system: usize, pub user: usize, pub assistant: usize, pub tool: usize }

#[derive(Debug, Clone)]
pub(crate) struct ResolutionTokenStats { pub full: usize, pub outlined: usize, pub summarized: usize, pub pinned: usize }

impl App {
    pub(crate) fn get_session_stats(&self) -> SessionStats {
        let stats = self.session.context_stats();
        SessionStats { /* ... */ }
    }
}
```

**Tests:**
- `get_session_stats_returns_structured_data` — call on a fresh app, assert
  `context_window > 0`, `utilization_percent < 100`, etc.
- `get_session_stats_reflects_usage` — after a `run_turn`, assert
  `estimated_used > 0` and `message_count > 0`

### Step 4: Add `App::set_model()` — shared model switching

**File:** `rho/src/app.rs`

```rust
impl App {
    /// Switch the active model. Updates session and all extension observers.
    pub(crate) async fn set_model(&mut self, model_id: &str) {
        self.session.set_model(model_id);
        self.ext_loader.set_model_all(model_id).await;
    }
}
```

This is a thin wrapper but it encapsulates the two-step update that both
frontends must perform.

**Tests:**
- `set_model_updates_session_model` — call `set_model("x")`, assert
  `session.model() == "x"`
- `set_model_updates_active_model` — call `set_model("x")`, assert
  `get_state()` returns "x"

### Step 5: Add `App::compact()` — shared compaction

**File:** `rho/src/app.rs`

```rust
pub(crate) async fn compact(&mut self) -> Result<(), rho_core::RhoError> {
    let strategy = rho_core::MechanicalCompactionStrategy::new();
    let threshold = self.session.message_budget() / 4;
    self.session.compact_older_than(threshold, &strategy).await
}
```

**Tests:**
- `compact_on_empty_session_succeeds` — fresh session, assert `Ok(())`
- `compact_after_turn_succeeds` — one turn, then compact, assert `Ok(())`

### Step 6: Refactor `rpc.rs` to use `App::run_turn()`

**File:** `rho/src/rpc.rs`

Replace `handle_prompt`'s inline orchestration with `app.run_turn()`:

```rust
async fn handle_prompt(app: &mut App, cmd: &Value, out: &Out, inp: &In) {
    let message = /* ... validate ... */;
    write_event(out, json!({"type": "agent_start"}));

    let observer = RpcObserver { out: Arc::clone(out) };
    let composite = CompositeObserver::new(/* ... */);
    let gate = RpcApprovalGate { out: Arc::clone(out), input: Arc::clone(inp) };

    match app.run_turn(&message, &gate, &composite).await {
        TurnResult::Reply(reply) => write_event(out, json!({"type": "agent_end", "reply": reply})),
        TurnResult::Error(e) => write_event(out, json!({"type": "agent_error", "error": e})),
    }
}
```

Replace `handle_get_session_stats` with `App::get_session_stats()`.
Replace `handle_set_model` with `App::set_model()`.
Replace `handle_compact` with `App::compact()`.

**Tests:** All existing RPC integration tests must continue to pass unchanged.
This is the safety net — 43 tests validate that the refactored RPC produces
identical output.

### Step 7: Refactor `repl.rs` to use `App::run_turn()`

**File:** `rho/src/repl.rs`

Replace the two duplicated orchestration blocks (normal input and `/paste`)
with `app.run_turn()`:

```rust
// Normal input path (and /paste — same call)
match app.run_turn(input, &gate, &composite).await {
    TurnResult::Reply(reply) => P::assistant_reply(&reply),
    TurnResult::Error(e) => P::error(&e),
}
```

**Tests:** The REPL doesn't have integration tests (it reads from real stdin),
but the refactoring is verified by:
1. `cargo check` — confirms compilation
2. `cargo clippy` — confirms lint compliance
3. `cargo test` — confirms all existing tests pass (including the 43 RPC tests
   that now exercise the shared `run_turn`)

### Step 8: Extract shared test infrastructure

**File:** `rho/tests/common/mod.rs` (new integration test module)

Move the `test_app()` helper from `rpc.rs` into a shared location accessible
to both RPC tests and any future REPL integration tests:

```rust
// rho/tests/common/mod.rs
pub fn test_app(client: MockChatClient, registry: ToolRegistry) -> App { ... }
pub fn echo_registry() -> ToolRegistry { ... }
pub fn destructive_registry() -> ToolRegistry { ... }
```

Update `rpc.rs` tests to import from the shared location. This is optional
if making `test_app` `pub(crate)` is simpler (it's in the same crate).

**Tests:** All existing tests must pass after the extraction.

## What We Are NOT Doing (and why)

| Idea | Why not |
|---|---|
| `Frontend` trait with `read_input` / `render_reply` | The REPL and RPC input models are fundamentally different (line-based string matching vs JSON dispatch). Abstracting them would be over-engineering. `run_turn` captures 90% of the value. |
| `Command` enum unifying slash commands and RPC commands | The REPL commands are presentation-heavy (formatted tables, interactive picker) while RPC commands are data-only. Unifying them would force both into a lowest-common-denominator abstraction. |
| Event channel / broadcast for TUI | Premature — there's no TUI yet. The observer trait already supports adding a TUI observer when needed. Channels can be added later without changing the `run_turn` interface. |
| Merging `ReplPresenter` and `RpcPresenter` | They already share method names for startup diagnostics. The divergence is intentional (stdout vs stderr, interactive vs no-op). A merger would add complexity without eliminating duplication. |
| Moving `run_turn` into `rho-core` | It depends on `App` fields (`active_provider`, `ext_observers`, `config`). Moving it would require exposing those internals. Keeping it in the binary crate is the right layer. |

## Verification

After each step:
1. `cargo check` — compilation
2. `cargo clippy -- -D warnings` — lint compliance
3. `cargo test` — all tests pass

After all steps:
4. `cargo xtask ci` — full CI pipeline

## Order of operations

```
Step 1: RED   — TurnResult + run_turn stub + failing test
Step 2: GREEN — Implement run_turn + passing tests
Step 3:       — App::get_session_stats + tests
Step 4:       — App::set_model + tests
Step 5:       — App::compact + tests
Step 6:       — Refactor rpc.rs to use shared methods (existing tests validate)
Step 7:       — Refactor repl.rs to use shared methods (compile + lint + existing tests)
Step 8:       — Extract shared test helpers (optional)
```
