# Refactor: Domain-Specific Error Types

**Status:** 📋 Planned  
**Branch:** `refactor/error-types`  
**Created:** 2026-05-17

## Motivation

The current centralized `RhoError` enum in `rho-core/src/error.rs` has design issues that compound as the codebase grows:

### 1. The `Unexpected` catch-all is a structural dead-end

In `rho-tools/src/shell.rs`, every internal error — permission denied, bad working directory, process spawn failure, timeout, encoding error — is flattened into:

```rust
Err(rho_core::RhoError::Unexpected(anyhow::anyhow!("message")))
```

This strips all structural information. Callers cannot match on specific tool errors. A `ReadFile` I/O error and a `RunCommand` syntax error look identical at the `RhoError` level. The `anyhow::Error` inside `Unexpected` is a string, not a structured type.

### 2. Violates the dependency rule

Per `ARCHITECTURE.md`: *"a crate may only depend on crates below it in the stack."* `rho-tools` depends on `rho-core`. When `rho-tools` needs a new error variant, someone has to edit `rho-core/src/error.rs` — a crate that logically has nothing to do with tool errors. The `Tool` trait's return type (`ToolOutcome`) already provides a natural decoupling boundary that the current code ignores.

### 3. Domain concerns are mixed

`RhoError` currently combines errors from different domains:

| Domain | Current variant(s) |
|--------|-------------------|
| HTTP/API | `Http`, `HttpError`, `RetryBudgetExhausted` |
| Agent loop | `MaxIterationsExceeded`, `Cancelled`, `ProtocolViolation` |
| Tool invocation | `Tool invocation | `ToolNotFound` |
| Session | `EntryNotFound` |
| Serialization | `Json` |
| Everything else | `Unexpected(anyhow)` |

These concerns have different recovery semantics, different contextual data needs, and different consumers. Mixing them in one enum makes matches unwieldy and makes it harder to reason about error handling in any single domain.

### 4. `HighlightError` is already separate — inconsistently

`rho-highlight` already has its own `HighlightError` enum with three variants, completely independent of `RhoError`. This proves the model works. The inconsistency is that `rho-tools` didn't follow the same pattern.

### When a shared error type *does* make sense

There are valid reasons for shared errors — `is_retryable()` on `RhoError` is genuinely useful. The agent loop needs a single place to decide "should I retry this?" regardless of error origin. This plan preserves that capability via a **thin boundary enum** (or trait) after carving out the domain specifics.

## Target Architecture

```
rho-core/
  src/
    error.rs               → REMOVED after migration (or thin boundary enum)
    client/
      mod.rs
      error.rs             → ClientError (Http, Json, HttpError, RetryBudgetExhausted)
    agent/
      mod.rs
      error.rs             → AgentError (MaxIterationsExceeded, Cancelled, ProtocolViolation)
    session/
      mod.rs
      error.rs             → SessionError (EntryNotFound, PersistenceError)
    sandbox.rs             → stays, but errors are local (SandboxError)
    tool.rs                → ToolError / ToolNotFound stays here (registry concern)

rho-tools/
  src/
    error.rs               → NEW: ToolError (FileSystem, CommandFailed, SandboxViolation, Cancelled)

rho-highlight/
  src/
    error.rs               → stays: HighlightError (already independent)
```

### Retry logic preserved via a trait

```rust
/// Implemented by error types that can be classified as retryable or permanent.
pub trait Retryable {
    fn is_retryable(&self) -> bool;
}
```

Implemented for `ClientError`, with a blanket impl that returns `false` for other domain errors. The agent loop uses `Retryable` instead of matching on `RhoError` directly.

### Optional thin boundary enum

If the binary (`rho`) or shared infrastructure needs a unified type to pass around, a minimal boundary enum can exist:

```rust
/// A unified error type for the top-level agent loop.
/// Each variant wraps a domain-specific error.
pub enum AgentError {
    Client(client::error::ClientError),
    Agent(agent::error::AgentError),
    Session(session::error::SessionError),
    ToolNotFound(String),
    Sandbox(SandboxError),
}
```

With `From` impls for each domain type. This is *optional* — the binary can also work with trait objects if preferred.

### Tool errors stay in `ToolResult`

The `Tool::execute` signature returns `Result<ToolOutcome>` where `ToolOutcome` contains a `ToolResult`. Tool-level errors (non-zero exit, I/O failure) should be returned as `ToolResult::error(...)` inside a successful `ToolOutcome::Immediate`, not as an `Err` variant — per the existing convention in the `Tool` trait docs. The new `ToolError` enum is used for true failures (tool precondition violations, cancellation, etc.).

## Implementation Phases

### Phase 1: `rho-tools` gets its own error type

**Goal:** Stop using `RhoError::Unexpected(anyhow!())` in `rho-tools`.

Steps:
1. Add `thiserror` to `rho-tools/Cargo.toml` (currently only has `anyhow`)
2. Create `rho-tools/src/error.rs` with a `ToolError` enum:
   ```rust
   #[derive(Debug, Error)]
   pub enum ToolError {
       #[error("missing required argument `{name}`")]
       MissingArgument { name: String },

       #[error("I/O error on `{path}`: {source}")]
       FileSystem { path: PathBuf, source: io::Error },

       #[error("command `{command}` failed (exit code: {exit_code}): {stderr}")]
       CommandFailed { command: String, exit_code: i32, stderr: String },

       #[error("path `{path}` is outside the working directory")]
       WorkingDirectoryEscape { path: PathBuf },

       #[error("tool cancelled")]
       Cancelled,
   }
   ```
3. Update `shell.rs` — replace every `RhoError::Unexpected(anyhow!(...))` with the appropriate `ToolError` variant
4. Update `files.rs` — same treatment for I/O and path errors
5. Update `rust/` modules — same treatment for parse errors, cargo failures, etc.
6. Remove `use rho_core::{Result, ...}` and replace with the tool's own `Result<ToolOutcome, ToolError>` in `execute` signatures (or keep using `rho_core::Result` by adding a `From<ToolError>` impl — see Phase 4)
7. Verify `cargo test --package rho-tools` passes

### Phase 2: `rho-core` per-module errors

**Goal:** Carve `RhoError` into focused domain enums.

Steps:
1. Create `rho-core/src/client/error.rs`:
   ```rust
   #[derive(Debug, Error)]
   pub enum ClientError {
       #[error("HTTP request failed: {0}")]
       Http(#[from] reqwest::Error),

       #[error("model API returned HTTP {status}: {message}")]
       HttpError { status: u16, message: String },

       #[error("retry budget exhausted after {0} attempts: {1}")]
       RetryBudgetExhausted(u32, Box<ClientError, Box<ClientError>),
   }
   ```
2. Create `rho-core/src/agent/error.rs`:
   ```rust
   #[derive(Debug, Error)]
   pub enum AgentError {
       #[error("agent loop exceeded maximum iterations ({0})")]
       MaxIterationsExceeded(u32),

       #[error("cancelled")]
       Cancelled,

       #[error("protocol violation: {0}")]
       ProtocolViolation(String),
   }
   ```
3. Create `rho-core/src/session/error.rs`:
   ```rust
   #[derive(Debug, Error)]
   pub enum SessionError {
       #[error("entry not found: {0}")]
       EntryNotFound(String),

       #[error("persistence error: {0}")]
       Persistence(String),
   }
   ```
4. Add `SandboxError` inline in `sandbox.rs`
5. Move `ToolNotFound` into `tool.rs` as a `ToolRegistryError`
6. `Json` errors (serde_json::Error)` — keep as a standalone type or absorb into whichever domain uses it
7. Add `pub mod error` or `pub mod client` etc. to `lib.rs` re-exports
8. Write `From` impls to convert each domain error into the old `RhoError` (keeping existing callers working during migration)

### Phase 3: `rho-highlight` audit

**Goal:** Ensure `HighlightError`HighlightError` is consistent and doesn't need changes.

Steps:
1. Review `HighlightError` variants — do they cover all error paths?
2. Ensure no references to `RhoError` in `rho-highlight`
3. If missing variants found, add them
4. Verify `cargo test --package rho-highlight` passes

### Phase 4: Remove old `RhoError` (or reduce to thin boundary)

**Goal:** Decide the fate of the central enum.

Two options:

**Option A: Remove entirely** — if `From` impls on each domain error + `Retryable` trait cover all use cases, delete `RhoError` and update all callers.

**Option B: Thin boundary enum** — keep a minimal `AgentError` in `rho-core/src/error.rs` that wraps domain errors:

```rust
#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Client(client::error::ClientError),

    #[error(transparent)]
    Agent(agent::error::AgentError),

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)] Session(session::error::SessionError),

    #[error(transparent)]
    Sandbox(SandboxError),
}
```

**Decision criteria:** If the binary has more than 3 places that match on `RhoError` variants from different domains simultaneously, keep Option B. Otherwise, Option A.

Steps:
1. Implement `Retryable` trait for `ClientError`
2. Update `agent.rs` to use `Retryable` instead of `RhoError::is_retryable()`
3. Implement chosen option (A or B)
4. Update all imports

### Phase 5: Wire the binary and test helpers

**Goal:** Fix all references in `rho`, `rho-bench`, and `rho-test-helpers`.

Steps:
1. Update `rho/src/main.rs` — fix any `RhoError` pattern matches
2. Update `rho-bench/src/` — fix any `RhoError` references
3. Update `rho-test-helpers/src/lib.rs` — ensure `MockChatClient` and helpers use correct error types
4. Verify everything compiles: `cargo build --workspace`

### Phase 6: CI and documentation

Steps:
1. Run `cargo xtask ci` — fix any failures
2. Run `cargo clippy --workspace --workspace -- -D warnings`
3. Update `ARCHITECTURE.md`:
   - Remove mention of `RhoError` from the Safety Layers table if it's gone
   - Update key-types table to show per-module error types
   - Update `error.rs` description to reflect the thin boundary or removal
   - Update `rho-tools` crate description to mention `ToolError`
   - Update `rho-highlight` crate description to mention `HighlightError`
4. Update `AGENTS.md` if any project conventions changed
5. Update `.plans/roadmap.md` if needed
6. Build docs: `cargo doc --workspace --no-deps --open`
7. Commit

## Rollback Plan

```bash
git checkout trunk
git branch -D refactor/error-types
```

Each phase compiles independently (the `From` impls in Phase 2 keep old callers working), so a partial rollback is also safe.

## Key Risks

| Risk | Mitigation |
|------|-----------|
| Large diff makes review hard | Phased implementation with compilable checkpoints |
| Missing error variant in new enums | Thorough audit of all `match` arms before deleting old enum |
| `is_retryable()` logic duplicated | `Retryable` trait keeps it centralized |
| Binary gets messy with 5+ error types | Thin boundary enum (Option B) keeps top-level code clean |
