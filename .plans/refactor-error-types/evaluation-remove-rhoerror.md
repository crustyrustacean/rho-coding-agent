# Evaluation: Can we remove `RhoError` entirely?

**Date:** 2026-05-18
**Status:** Assessment

## TL;DR

**Yes, but it requires changing 5 public trait signatures and all their implementors.** The main benefit is eliminating the `Unexpected(anyhow::Error)` catch-all. The cost is a large mechanical refactor touching trait definitions, 2 external crates (`rho-tools`, `rho-test-helpers`), and ~50 call sites. It does not unlock new functionality — it's purely a hygiene improvement.

## What `RhoError` is today

`RhoError` is a thin boundary enum with 7 variants:

```rust
enum RhoError {
    Client(ClientError),           // #[error(transparent)]
    Agent(AgentError),             // #[error(transparent)]
    Session(SessionError),         // #[error(transparent)]
    Sandbox(SandboxError),         // #[error(transparent)]
    ToolNotFound(String),          // standalone
    RetryBudgetExhausted(u32, Box<RhoError>),  // recursive
    Unexpected(#[from] anyhow::Error),         // ← the catch-all
}
```

It serves as the error type for `crate::error::Result<T>` = `Result<T, RhoError>`.

## Where `RhoError` is used as a return type

### Trait methods (5 traits)

| Trait | Method | Returns | Implementors |
|-------|--------|---------|-------------|
| `ChatClient` | `chat()` | `Result<ModelResponse>` | `LocalChatClient`, `MockChatClient` |
| `ChatClient` | `chat_stream()` | `Result<ModelResponseStream>` | `LocalChatClient`, `MockChatClient` |
| `Tool` | `execute()` | `Result<ToolOutcome>` | 5 tools in `rho-tools` |
| `ToolRegistry` | `execute()` | `Result<ToolResult>` | (method on struct) |
| `ShellExecutor` | `execute()` | `Result<ShellOutput>` | `TokioShellExecutor` |

### Struct methods (in `rho-core`)

| Struct | Methods | Count |
|--------|---------|-------|
| `Sandbox` | `new`, `validate`, `validate_for_write`, `assert_within` | 4 |
| `Session` | `open`, `flush`, `branch_to`, `compact_older_than` | 4+ |
| `Session` (persist) | `open_session`, `flush_session` | 2 |
| `FilePath` | `new_validated`, `new_for_write` | 2 |
| `ToolRegistry` | `execute` | 1 |
| Free fns | `find_project_root`, `canonicalize_for_write`, `build_tool_calls` | 3 |

### Agent loop

- `TransitionError::{Retryable, Fatal}` both wrap `RhoError`
- `run_agent_loop` returns `Result<String>` = `Result<String, RhoError>`
- `run_with_retry` returns `Result<AssistantResponse>` = `Result<AssistantResponse, RhoError>`

### External crates

- `rho-tools/src/error.rs`: `From<ToolError> for RhoError` (bridging impl)
- `rho-test-helpers/src/lib.rs`: `MockChatClient` and `FailingTool` implement traits returning `Result<T, RhoError>`
- `rho-core/tests/integration_tests.rs`: 15+ pattern matches on `RhoError` variants

## What would need to change

### Option A: Replace with `anyhow::Error` everywhere

Unlikely what we want — loses typed error matching.

### Option B: Per-trait error types

Each trait defines its own error:

```rust
// In ChatClient trait
enum ChatClientError {
    Http(reqwest::Error),
    Json(serde_json::Error),
    RateLimited,
    // ...
}

// In Tool trait
enum ToolExecError { ... }

// In ShellExecutor trait
enum ShellExecError { ... }
```

The agent loop would then need its own aggregate type or use `anyhow::Error`.

**Estimated scope:**
- Change 5 trait `Result<T>` → `Result<T, DomainError>`
- Update all implementors (2 ChatClient impls, 5 Tool impls, 1 ShellExecutor impl)
- Update `ToolRegistry::execute` to translate tool errors
- Update `Session` methods to return `SessionError` instead of `RhoError`
- Update `Sandbox` methods to return `SandboxError` instead of `RhoError`
- Update agent loop `TransitionError` to work with new types
- Update `From` impls in external crates
- Update ~50 test assertions
- **~200–300 lines changed across ~15 files**

### Option C: Keep `RhoError` but remove `Unexpected` variant

This is the middle ground. Remove the `#[from] anyhow::Error` variant, forcing all catch-all sites to pick a specific variant. The remaining 6 variants are all legitimate cross-domain concerns.

**Estimated scope:**
- Remove `Unexpected(#[from] anyhow::Error)` variant
- Fix 3 remaining sites that still use `Unexpected`:
  1. `rho-tools/src/error.rs:99` — `From<ToolError>` bridge → change to `RhoError::Tool(...)` or similar
  2. `rho-core/src/sandbox.rs:58` — `From<SandboxError>` bridge → already has `RhoError::Sandbox` variant, just not using it
  3. `rho-test-helpers/src/lib.rs:272` — `FailingTool` → needs a new `ToolError` variant or `anyhow` via a different mechanism
- Update `RetryBudgetExhausted` to box the domain error directly
- **~30 lines changed across 5 files**

## Analysis by variant

| Variant | Owner domain | Could it move? | Notes |
|---------|-------------|----------------|-------|
| `Client(ClientError)` | client | ✅ Already has its own type | Transparent wrapper |
| `Agent(AgentError)` | agent | ✅ Already has its own type | Transparent wrapper |
| `Session(SessionError)` | session | ✅ Already has its own type | Transparent wrapper |
| `Sandbox(SandboxError)` | sandbox | ✅ Already has its own type | Transparent wrapper |
| `ToolNotFound(String)` | tool registry | ⚠️ Cross-cutting | Agent loop + ToolRegistry both emit this |
| `RetryBudgetExhausted` | agent loop | ⚠️ Cross-cutting | Wraps another RhoError recursively |
| `Unexpected(anyhow)` | *catch-all* | 🎯 Target for removal | 3 remaining sites |

## Recommendation

**Option C (remove `Unexpected` variant only)** gives 80% of the benefit at 10% of the cost:

1. The `Unexpected(anyhow)` catch-all is the only "bad" part of `RhoError`. It swallows structured errors into an opaque `anyhow::Error`.
2. All 4 domain types already have their own variants in `RhoError`.
3. The `ToolNotFound` and `RetryBudgetExhausted` variants are legitimate cross-domain concerns that belong at the top level.
4. Removing the catch-all forces any new error to go through a domain type first, which is the whole point of the refactor.

**Option B (per-trait error types)** is the "correct" architecture but is a much larger change with no functional benefit. It would be appropriate as a follow-up if the codebase grows more trait implementors.

## Steps for Option C

1. Fix `SandboxError → RhoError` conversion to use `RhoError::Sandbox` instead of `RhoError::Unexpected`
2. Fix `ToolError → RhoError` conversion in `rho-tools` — add `ToolError` variant to `RhoError` (or use a different bridging strategy)
3. Fix `FailingTool` in test-helpers — either use an existing variant or add a `TestFailure` variant
4. Remove `Unexpected(#[from] anyhow::Error)` variant from `RhoError`
5. Update `RetryBudgetExhausted` to box a domain error instead of `RhoError`
6. Run CI

**Estimated time:** 30–60 minutes.
