# RPC-as-Core: Detailed Step-by-Step Plan

**Based on:** `.plans/rpc-as-core.md`
**Goal:** Extract duplicated agent-turn orchestration from `repl.rs` and `rpc.rs` into shared `App` methods.

## Current State Analysis

### Duplication Identified

1. **Agent turn orchestration** — appears 3×:
   - `repl.rs` normal input (~lines 183–199): clone provider, build composite observer, build `LoopParams`, call `run_loop`, render result
   - `repl.rs` `/paste` command (~lines 143–160): identical logic with different input source
   - `rpc.rs` `handle_prompt` (~lines 264–295): same clone + build + call pattern

2. **Session stats query** — appears 2×:
   - `rpc.rs` `handle_get_session_stats`: reads `session.context_stats()`, renders as JSON
   - `repl.rs` `show_context_stats`: reads `session.context_stats()`, renders as formatted text
   - These differ significantly in rendering (JSON vs formatted text), so extracting just the stats query doesn't save much. **Skip** — rendering is the whole point.

3. **Model switching** — appears 2×:
   - `rpc.rs` `handle_set_model`: `session.set_model(id)` + `ext_loader.set_model_all(id).await` (literal ID)
   - `repl.rs` `switch_model`: `session.set_model(&info.id)` + `ext_loader.set_model_all(&info.id).await` + provider index lookup + fuzzy matching + interactive disambiguation
   - The REPL version has significant additional logic (fuzzy match, provider index, interactive prompts). The shared core is just 2 lines. **Extract** as `App::set_model()`.

4. **Compaction** — appears 1×:
   - `rpc.rs` `handle_compact`: builds strategy + threshold, calls `session.compact_older_than()`
   - The REPL has no `/compact` command currently.
   - **Extract** as `App::compact()` for future use and to keep RPC handler thin.

### Key Observations from Code Review

- `CompositeObserver<'a>` borrows `Vec<&'a dyn AgentObserver>` — lifetime is tied to the caller's stack frame. `run_turn()` must accept a pre-built composite, not build one internally (lifetimes would be complex with `&self.ext_observers`).
- `ApprovalGate` is a trait object (`&dyn ApprovalGate`). The REPL uses `ReplApprovalGate` (reads stdin), RPC uses `RpcApprovalGate` (reads JSONL). Both implement `ApprovalGate`. `run_turn()` takes `&dyn ApprovalGate`.
- `AgentObserver` is a trait object (`&dyn AgentObserver`). Already handled via `CompositeObserver`.
- `LoopParams` takes `client: &dyn rho_ai::ChatService`. The client is created per-turn via `active_provider().clone_boxed_service()`. This must remain inside `run_turn()`.
- `test_app()` in `rpc.rs` is `fn test_app(...) -> App` — constructs an `App` directly. It will be needed by new `app.rs` tests. Make it `pub(crate)`.

---

## Step 1: Define `TurnResult` — RED phase

**File:** `rho/src/app.rs`

### What to add

```rust
/// The outcome of a single agent turn.
#[derive(Debug, Clone)]
pub(crate) enum TurnResult {
    /// Agent produced a text reply.
    Reply(String),
    /// Agent loop encountered an error.
    Error(String),
}
```

### What to add (stub)

```rust
impl App {
    /// Run one agent turn. NOT YET IMPLEMENTED.
    pub(crate) async fn run_turn(
        &mut self,
        message: &str,
        gate: &dyn ApprovalGate,
        observer: &dyn AgentObserver,
    ) -> TurnResult {
        TurnResult::Error("not implemented".to_owned())
    }
}
```

### New imports needed in `app.rs`

```rust
use rho_core::{AgentObserver, ApprovalGate};
```

### Tests to add (in `app.rs` `#[cfg(test)] mod tests`)

- `run_turn_returns_error_when_not_implemented` — calls `run_turn()`, asserts `matches!(result, TurnResult::Error(_))` and the error contains "not implemented"

### Verification

- `cargo check` — compiles with new types and stub
- `cargo test -p rho -- run_turn_returns_error` — RED: test passes (stub returns error)

---

## Step 2: Implement `App::run_turn()` — GREEN phase

**File:** `rho/src/app.rs`

### What to change

Replace the `run_turn` stub with the real implementation:

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

### Additional imports needed

```rust
use rho_core::LoopParams;
```

### Tests to add

Use `test_app()` (copied/made shared — see Step 8) or inline a minimal test app builder.

- `run_turn_text_only_returns_reply` — mock client returns text events; assert `TurnResult::Reply("hello")`
- `run_turn_tool_call_returns_reply` — mock returns tool call then text; assert `TurnResult::Reply` with the final text
- `run_turn_error_returns_error_string` — mock returns HTTP 403 error; assert `TurnResult::Error` containing "403"
- `run_turn_max_iterations_returns_error` — set `config.max_iterations = 1` with infinite-tool-call mock; assert `TurnResult::Error` containing "maximum iterations"
- `run_turn_session_persists_messages` — send "hello", assert session has system + user + assistant messages in `path_messages()`
- `run_turn_multiple_turns_accumulate` — send "hello" then "world", assert 5 messages in session

### Test infrastructure

To avoid duplicating `test_app()`, we have two options:
- **Option A (simpler):** Duplicate a minimal `test_app()` in `app.rs` tests (it's ~20 lines).
- **Option B (cleaner):** Extract `test_app()` from `rpc.rs` to a shared test helpers module and make it `pub(crate)`. Defer this to Step 8.

**Recommendation:** Use Option A for now (inline in app.rs tests), refactor in Step 8.

### Verification

- `cargo check` — compiles
- `cargo test -p rho -- run_turn` — all 7 tests pass (1 stub test + 6 new tests)

---

## Step 3: Add `App::set_model()` — shared model switching

**File:** `rho/src/app.rs`

### What to add

```rust
/// Switch the active model. Updates session and all extension observers.
pub(crate) async fn set_model(&mut self, model_id: &str) {
    self.session.set_model(model_id);
    self.ext_loader.set_model_all(model_id).await;
}
```

### Tests to add

- `set_model_updates_session_model` — call `set_model("x")`, assert `session.model() == "x"`
- `set_model_idempotent` — call twice, assert final model is the second one

### Verification

- `cargo check` — compiles
- `cargo test -p rho -- set_model` — tests pass

---

## Step 4: Add `App::compact()` — shared compaction

**File:** `rho/src/app.rs`

### What to add

```rust
/// Trigger context compaction on the active session.
///
/// Uses mechanical compaction with a threshold of one-quarter of the message
/// budget.
pub(crate) async fn compact(&mut self) -> Result<(), rho_core::RhoError> {
    let strategy = rho_core::MechanicalCompactionStrategy::new();
    let threshold = self.session.message_budget() / 4;
    self.session.compact_older_than(threshold, &strategy).await
}
```

### Additional imports needed

```rust
use rho_core::MechanicalCompactionStrategy;
```

### Tests to add

- `compact_on_empty_session_succeeds` — fresh session, assert `Ok(())`
- `compact_after_turn_succeeds` — one turn via `run_turn`, then compact, assert `Ok(())`

### Verification

- `cargo check` — compiles
- `cargo test -p rho -- compact` — tests pass

---

## Step 5: Refactor `rpc.rs` to use `App::run_turn()`, `App::set_model()`, and `App::compact()`

**File:** `rho/src/rpc.rs`

### Changes to `handle_prompt`

Replace the inline orchestration (~30 lines) with:

```rust
async fn handle_prompt(app: &mut App, cmd: &Value, out: &Out, inp: &In) {
    let message = match cmd["message"].as_str() {
        Some(m) if !m.is_empty() => m.to_owned(),
        _ => {
            write_event(out, json!({
                "type": "response",
                "success": false,
                "error": "prompt requires a non-empty \"message\" field",
            }));
            return;
        }
    };

    write_event(out, json!({"type": "agent_start"}));

    let observer = RpcObserver { out: Arc::clone(out) };
    let composite = CompositeObserver::new({
        let mut obs: Vec<&dyn AgentObserver> = vec![&observer];
        for ext_obs in &app.ext_observers {
            obs.push(ext_obs);
        }
        obs
    });
    let gate = RpcApprovalGate {
        out: Arc::clone(out),
        input: Arc::clone(inp),
    };

    match app.run_turn(&message, &gate, &composite).await {
        TurnResult::Reply(reply) => write_event(out, json!({"type": "agent_end", "reply": reply})),
        TurnResult::Error(e) => write_event(out, json!({"type": "agent_error", "error": e})),
    }
}
```

Key change: The `client`, `compaction_client`, `params`, and `run_loop` call are all replaced by `app.run_turn()`.

### Changes to `handle_set_model`

Replace:
```rust
app.session.set_model(id);
app.ext_loader.set_model_all(id).await;
```
With:
```rust
app.set_model(id).await;
```

### Changes to `handle_compact`

Replace:
```rust
let strategy = MechanicalCompactionStrategy::new();
let threshold = app.session.message_budget() / 4;
match app.session.compact_older_than(threshold, &strategy).await {
```
With:
```rust
match app.compact().await {
```

### Removals from `rpc.rs` imports

- Remove `MechanicalCompactionStrategy` from the `use rho_core::{...}` import (no longer used in rpc.rs)

### Additions to `rpc.rs` imports

- Add `use crate::app::TurnResult;`

### Tests

**No new tests.** All 43 existing RPC integration tests must continue to pass unchanged. This is the safety net — the tests exercise the full RPC loop end-to-end, so any behavioral change will be caught.

### Verification

- `cargo check` — compiles
- `cargo clippy -- -D warnings` — no new lints
- `cargo test -p rho` — all 43+ RPC tests pass

---

## Step 6: Refactor `repl.rs` to use `App::run_turn()`

**File:** `rho/src/repl.rs`

### Changes to normal input path (~lines 183–199)

Replace the inline orchestration with:

```rust
let composite = build_composite(&repl_observer, &app.ext_observers);
match app.run_turn(input, &gate, &composite).await {
    Ok(reply) => P::assistant_reply(&reply),
    Err(e) => P::error(&e.to_string()),
}
```

Wait — `run_turn` returns `TurnResult`, not `Result`. The mapping is:

```rust
let composite = build_composite(&repl_observer, &app.ext_observers);
match app.run_turn(input, &gate, &composite).await {
    TurnResult::Reply(reply) => P::assistant_reply(&reply),
    TurnResult::Error(e) => P::error(&e),
}
```

### Changes to `/paste` command (~lines 143–160)

Same replacement:

```rust
let composite = build_composite(&repl_observer, &app.ext_observers);
match app.run_turn(pasted.trim(), &gate, &composite).await {
    TurnResult::Reply(reply) => P::assistant_reply(&reply),
    TurnResult::Error(e) => P::error(&e),
}
```

### Removals from `repl.rs`

- Remove the `client`, `compaction_client`, and `params` variable declarations from both code paths
- Remove `use rho_core::LoopParams;` if it was imported (check — currently not directly imported, the path is `rho_core::LoopParams`)

### Tests

**No new tests.** The REPL doesn't have integration tests (it reads from real stdin). Verification is by:
1. `cargo check` — confirms compilation
2. `cargo clippy -- -D warnings` — lint compliance
3. `cargo test` — all existing tests pass (including RPC tests that exercise the shared `run_turn`)

### Verification

- `cargo check` — compiles
- `cargo clippy -- -D warnings` — no new lints
- `cargo test` — all tests pass

---

## Step 7: Extract shared test infrastructure (optional cleanup)

**File:** Either `rho/tests/common/mod.rs` (new integration test module) or keep inline.

### What to extract

Move the `test_app()` helper from `rpc.rs` tests to a location accessible to both `app.rs` tests and `rpc.rs` tests.

**Recommendation:** Since both `app.rs` and `rpc.rs` are in the same crate (`rho`), the simplest approach is:
1. Make `test_app()` in `rpc.rs` `pub(crate)`
2. Import it in `app.rs` tests via `use crate::rpc::tests::test_app`

But wait — `rpc::tests` is a private module. Better approach:
1. Add a `pub(crate)` test helper module at `rho/src/test_helpers.rs` (gated by `#[cfg(test)]`)
2. Move `test_app()` there
3. Import from both `app.rs` tests and `rpc.rs` tests

### Actually — let's simplify

Since `app.rs` tests already have an inline `test_app()` from Step 2, and the RPC tests work fine with their own copy, this step is **purely cosmetic**. Defer unless the duplication bothers us.

**Decision:** Skip this step. The duplication is ~20 lines in test code, gated by `#[cfg(test)]`, and the two `test_app()` helpers may diverge slightly (RPC tests need `Mode::Rpc`, app tests may want `Mode::Repl`). Not worth the complexity.

---

## Verification Checklist (after all steps)

1. `cargo fmt --all -- --check` — formatting clean
2. `cargo clippy --all-targets -- -D warnings` — no lints
3. `cargo test` — all tests pass
4. `cargo xtask ci` — full CI pipeline

---

## What We Are NOT Doing

| Idea | Why not |
|---|---|
| `Frontend` trait with `read_input` / `render_reply` | The REPL and RPC input models are fundamentally different (line-based string matching vs JSON dispatch). `run_turn` captures 90% of the value. |
| `Command` enum unifying slash commands and RPC commands | The REPL commands are presentation-heavy (formatted tables, interactive picker) while RPC commands are data-only. |
| Event channel / broadcast for TUI | Premature — there's no TUI yet. The observer trait already supports adding a TUI observer. |
| Merging `ReplPresenter` and `RpcPresenter` | They already share method names. The divergence is intentional. |
| Moving `run_turn` into `rho-core` | It depends on `App` fields (`active_provider`, `ext_observers`, `config`). Keeping it in the binary crate is the right layer. |
| Extracting `SessionStats` struct | `show_context_stats` (REPL) and `handle_get_session_stats` (RPC) differ in what they render and how they compute derived values. The shared read is `session.context_stats()` which is already a method. |
| Extracting `build_composite()` to `App` | The observer composition is inherently frontend-specific (different observer types, different lifetimes). Keep it in each frontend. |

---

## Order of Execution

```
Step 1: RED   — TurnResult + run_turn stub + failing test          (~10 min)
Step 2: GREEN — Implement run_turn + 6 passing tests                (~30 min)
Step 3:       — App::set_model + 2 tests                            (~10 min)
Step 4:       — App::compact + 2 tests                              (~10 min)
Step 5:       — Refactor rpc.rs to use shared methods                (~15 min)
Step 6:       — Refactor repl.rs to use shared methods              (~10 min)
Step 7:       — SKIP (shared test infra not worth the complexity)
Final:       — cargo xtask ci                                       (~5 min)
```

**Total estimated effort: ~90 minutes**
