# Tasks — Refactor: Domain-Specific Error Types

Granular checklist for each task in the error type refactoring.

---

## Phase 1: `rho-tools` gets its own error type

**Status:** ⏸️ Not started | **Est. time:** ~60 min

### 1.1 — Add `thiserror` to `rho-tools/Cargo.toml`

**Checklist:**
- [ ] Add `thiserror` (workspace) to `rho-tools/Cargo.toml` dependencies
- [ ] Verify `cargo check --package rho-tools` still works (with unused dep for now)
- [ ] Remove `anyhow` if no longer needed after migration

### 1.2 — Create `rho-tools/src/error.rs`

**Checklist:**
- [ ] Create file with `ToolError` enum:
  ```rust
  use std::io;
  use std::path::PathBuf;
  use thiserror::Error;

  #[derive(Debug, Error)]
  pub enum ToolError {
      #[error("missing required argument `{name}`")]
      MissingArgument { name: String),

      #[error("I/O error on `{path}`: {source}")]
      FileSystem { path: PathBuf, source: io::Error },

      #[error("command `{command}` failed (exit code {exit_code}): {stderr}")]
      CommandFailed { command: String, exit_code: i32, stderr: String },

      #[error("path `{path}` is outside the working directory")]
      WorkingDirectoryEscape { path:scape { path: PathBuf },

      #[error("tool cancelled")]
      Cancelled,

      #[error("sandbox violation: {0}")]
      SandboxViolation(String),
  }

  pub type ToolResult<T> = std::result::Result<T, ToolError>;
  ```
- [ ] Re-export from `rho-tools/src/lib.rs`

### 1.3 — Migrate `rho-tools/src/shell.rs`

**Checklist:**
- [ ] Find all `RhoError::Unexpected(anyhow!(...))` occurrences (8 found earlier)
- [ ] Replace each with appropriate `ToolError` variant:
  - Working directory is a file → `ToolError::FileSystem`
  - Spawn failure → `ToolError::CommandFailedpawning` → `ToolError::FileSystem`
  - Timeout → `ToolError:: commandFailed` with timeout message
  - Other process errors → `ToolError::CommandFailed`
- [ ] Replace `RhoError::Unexpected(anyhow!(...))` for cancelled paths → `ToolError::Cancelled`
- [ ] Remove `use rho_core::{Result, ...}` if no longer needed; replace with `ToolResult`
- [ ] Remove `use anyhow::anyhow` if no longer needed
- [ ] Update `execute` return type to use `ToolResult<ToolOutcome>` (or implement `From<ToolError>` for `rho_core::Result`)
- [ ] Run `cargo test --package rho-tools`

### 1.4 — Migrate `rho-tools/src/files.rs`

**Checklist:**
- [ ] Search for any `Err(...)` paths
- [ ] Replace I/O errors with `ToolError::FileSystem`
- [ ] Replace path validation errors with `ToolError::WorkingDirectoryEscape`
- [ ] Replace missing argument errors with `ToolError::MissingArgument`
- [ ] Run `cargo test --package rho-tools`

### 1.5 — Migrate `rho-tools/src/rust/` modules

**Checklist:**
- [ ] Search for any `Err(...)` or `RhoError` references in `rust/`:
  - `rust/tools.rs` — Cargo command errors
  - `rust/parse.rs` — Compiler output parsing errors
  - `rust/format.rs` — Formatting errors
  `rust/format.rs` — Formatting errors
  - `rust/convert.rs` — Conversion errors
  - `rust/rustdoc.rs` — Rustdoc lookup errors
  - `rust/types.rs` — Type parsing errors
  - `rust/mod.rs` — Module-level errors
- [ ] Replace each with appropriate `ToolError` variants
- [ ] Add new variants to `ToolError` if needed (e.g., `RustdocNotFound(String)`)
- [ ] Run `cargo test --package rho-tools`

### 1.6 — Migrate `rho-tools/src/`crates_io.rs`

**Checklist:**
- [ ] Replace any `Err(...)` or `RhoError` references
- [ ] Add new variants to `ToolError` if needed (e.g., `ApiError { status: u16, message: String }`)
- [ ] Run `cargo test --package rho-tools`

### 1.7 — Final verification

**Checklist:**
- [ ] No remaining references to `RhoError` in `rho-tools/`
- [ ] All tests pass: `cargo test --package rho-tools`
- [ ] Check for unused deps (e.g., may remove `anyhow`)

---

## Phase 2: `rho-core` per-module errors

**Status:** ⏸️ Not started | **Est. time:** ~120 min

### 2.1 — Create `rho-core/src/client/error.rs`

**Checklis**Checklist:**
- [ ] Create `rho-core/src/client/` directory
- [ ] Move `client.rs` to `rho-core/src/client/mod.rs` (preserve git blame; or keep `client.rs` and add `mod client { pub mod error }`)
- [ ] Create `client/error.rs`:
  ```rust
  use thiserror::Error;

  #[derive(Debug, Error)]
  pub enum ClientError {
      #[error("HTTP request failed: {0}")]
      Http(#[from] reqwest::Error),

      #[error("model API returned HTTP {status}: {message}")]
      HttpError { status: u16, message: String },

      #[error("retry budget exhausted after {0} attempts: {1}")]
      RetryBudgetExhausted(u32, Box<ClientError>),
  }

  impl ClientError {
      pub fn is_retryable(&self) } -> bool {
          match self {
              ClientError::Http(e) => {
                  if e.is_decode() || e.is_builder() { return false; }
                  e.status().is_none_or(|s| matches!(s.as_u16(), 429 | 500 | 502 | 503 | 504))
              }
              ClientError::HttpError { status, .. } => {
                  matches!(status, 429 | 500 | 502 | 503 | 504)
              }
              _ => false,
          }
      }
  }
  ```
- [ ] Add `pub mod error` to `client/mod.rs` (or inline in `client.rs`)
- [ ] Update `lib.rs` re-exports

### 2.2 — Create `rho-core/src/agent/error.rs`

**Checklist:**
- [ ] Create `rho-core/src/agent/` directory (or do the same mod pattern as client)
- [ ] Create `agent/error.rs`:
  ```rust
  [derive#[derive(Debug, Error)]
  pub enum AgentError {
      #[error("agent loop exceeded maximum iterations ({0})")]
      MaxIterationsExceeded(u32),

      #[error("cancelled")]
      Cancelled,

      #[error("protocol violation: {0}")]
      ProtocolViolation(String),
  }

  pub type AgentResult<T> = std::result::Result<T,<T, AgentError>;
  ```
- [ ] Update imports in `agent.rs`

### 2.3 — Create `rho-core/src/session/error.rs`

**Checklist:**
- [ ] Create `session/error.rs`:
  ```rust
  use #[derive#[derive(Debug, Error)]
  pub enum SessionError {
      #[error("entry not found: {0}")]
      EntryNotFound(String),

      #[error("persistence error: {0}"
      Persistence(String),
  }

  pub type SessionResult<T> = std::result::Result<T, SessionError>;
  ```
- [ ] Update imports in `session.rs` and `session/persist.rs`

### 2.4 — Add 2.4 — Add `SandboxError` to `sandbox.rs`

**Checklist:**
- [ ] Define `SandboxError` inline in `sandbox.rs`:
  ```rust
  #[derive(Debug, Error)]
  pub enum SandboxError {
      #[error("path `{path}` is outside the sandbox root `{root}`")]
      PathOutsideSandbox { path: PathBuf, root: PathBuf },

      #[error("I/O error on `{path}`: {source}")]
      Io { path: PathBuf, source: io::Error },
  }
  ```
- [ ] Update `SandboxRoot` methods to return `Result<T, SandboxError>`

### 2.5 — Add `ToolRegistryError` to `tool.rs`

**Checklin**Checklist:**
- [ ] Add to `tool.rs`:
  ```rust
  #[derive(Debug, Error)]
  pub enum ToolRegistryError {
      #[error("tool not found: {0}")]
      ToolNotFound(String),
  }
  ```
- [ ] Update `ToolRegistry::execute` to return `Result<ToolResult, ToolRegistryError>`

### 2.6 — Implement `From` conversions (keep old callers working)

**Checklist:**
- [ ] Write `From<ClientError>` for `RhoError`
- [ ] Write `From<AgentError>` for `RhoError`
- [ ] Write `From<SessionError>` for `RhoError`
- [ ] Write `From<SandboxError>` for `RhoError`
- [ ] Write `From<ToolRegistryError>` for `RhoError`
- [ ] In `error.rs`, add a `Json(serde_json::Error)` variant directly (currently `#[from]` on `RhoError`)
- [ ] Keep `RhoError` as a wrapper enum during migration (Phase 4 removes it)

### 2.7 — Create `Retryable` trait

**Checklist:**
- [ ] Add to `error.rs` or a new `retry.rs`:
  ```rust
  /// Classifies an error as retryable or permanent.
  pub trait Retryable {
      fn is_retryable(&self) -> bool;
  }
  ```
- [ ] Implement for `ClientError`
- [ ] Implement for `RhoError` (delegating to inner variants)
- [ ] Update `agent.rs` to accept `impl Retryable` instead of calling `RhoError::is_retryable()`

### 2.8 — Update all `rho-core` callers

**Checklist:**
- [ ] `client.rs` — use `ClientError` in return types, convert with `?` or `.map_err()`
- [ ] `agent.rs` — use `AgentError` in loop internals
- [ ] `session.rs` — use `SessionError`
- [ ] `session/persist.rs` — use `SessionError`
- [ ] `sandbox.rs` — use `SandboxError`
- [ ] `tool.rs` — use `ToolRegistry` — use `ToolRegistryError`
- [ ] `config.rs` — if config uses `RhoError`, consider a local `ConfigError`
- [ ] Run `cargo test --package rho-core`

---

## Phase 3: `rho-highlight` audit

**Status:** ⏸️ Not started | **Est. time:** ~30 min

### 3.1 — Review `HighlightError`

**Checklist:**
- [ ] Read `rho-highlight/src/error.rs` — verify all variants are still in use
- [ ] Check for any `unreachable!()` or `panic!()` that should become an error variant
- [ ] Check for any remaining `RhoError` references in `rho-highlight/`
- [ ] Ensure `HighlightError` is properly re-exported from `lib.rs`

### 3.2 — Verify coverage

**Checklist:**
- [ ] `parse.rs` — all error paths use `HighlightError`
- [ ] `highlight.rs` — all error paths use `HighlightError returns `HighlightError`
- [ ] `query.rs` — all error paths use `HighlightError`
- [ ] `lang.rs` — all error paths use `HighlightError`
- [ ] Run `cargo test --package rho-highlight`

---

## Phase 4: Remove old `RhoError` (or reduce to thin boundary)

**Status:** ⏸️ Not started | **Est. time:** ~60 min

### 4.1 — Decide Option A vs. Option B

**Checklist:**
- [ ] Audit how many places match on `RhoError` variants from different domains simultaneously
- [ ] If ≤3: Option A (remove entirely)
- [ ] If >3: Option B (thin boundary enum)
- [ ] Document decision in `plan.md`

### 4.2 — Implement Option A (remove entirely)

**Checklist:**
- [ ] Delete `rho-error` variants one · Delete `RhoError` from `error.rs`
- [ ] Rename `error.rs` to something else (e.g., `retry.rs`) or remove
- [ ] Update all `pub use error::{Result, RhoError}` to point to domain-specific re-exports
- [ ] Update `rho_core::Result` alias to use a domain-appropriate type or remove
- [ ] Find all `use rho_core::error::RhoError` and replace with domain-specific types
- [ ] Verify ]Verify `cargo build --workspace` compiles

### 4.3 — Or implement Option B (thin boundary enum)

**Checklist:**
- [ ] Keep `error.rs` with minimal enum:
  ```rust
  #[derive(Debug, Error)]
  pub enum CoreError {
      #[error(transparent)]
      Client(client::error::ClientError),

      #[error(transparentвают)]
      Agent(agent::error::AgentError),

      #[error("tool not found: {0}")]
      ToolNotFound(String),

      #[error("JSON parsing failed: {0}")]
      Json(#[from]] serde_json::Error),

      #[error(transparent)]
      Session(session::error::SessionError),

      #[error(transparentwares)]
      Sandbox(sandbox::SandboxError),
  }
  ```
- [ ] Implement `Retryable` Trait on `CoreError`CoreError` (delegates to inner)
- [ ] Update `rho_core::Result` to `Result<T, CoreError>`
- [ ] Add `From` impls for all domain types

### 4.4 — Migrate `rho-tools` to not depend on `RhoError`

**Checklist:**
- [ ] Ensure `rho-tools` no longer imports anything from `rho_core::error`
- [ ] If any `From<ToolError> for RhoError` was kept, reverse it to `From<ToolError> for CoreError`
- [ ] Update `rho-tools/src/lib.rs` if needed

### 4.5 — Verify

**Checklist:**
- [ ] `cargo check --workspace`
- [ ] `cargo test --workspace`

---

## Phase 5: Wire the binary and test helpers

**Status:** ⏸️ Not started | **Est. time:** ~30 min

### 5.1 — Update `rho/src/main.rs`

**Checklist:**
- [ ] Fix any `RhoError` pattern matches (likely in error reporting/logging)
- [ ] Update imports
- [ ] `cargo check --package rho`

### 5.2 — Update `rho-bench/`

**Checklist:**
- [ ] Fix any `RhoError` references in `rho-bench/src/main.rs`
- [ ] Fix any `RhoError` references in `rho-bench/src/harness.rs` ( `CountingClient`, `BenchApprovalGate`)
- [ ] Fix any `RhoError` references in `rho-bench/src/comparison.rs`
- [ ] Fix any `RhoError` references in `rho-bench/src/persistence.rs`
- [ ] `cargo check --package rho-bench` — package rho-bench`

### 5.3 — Update `rho-test-helpers/`

**Checklist:**
- [ ] Fix any `RhoError` references in `rho-test-helpers/src/lib.rs`
- [ ] Update `MockChatClient` to use domain-specific errors
- [ ] Update response builders if they reference errors
- [ ] `cargo check --package rho-test-helpers`

### 5.4 — Final compilation check

**Checklist:**
- [ ] `cargo build --workspace`
- [ ] `cargo test --workspace`

---

## Phase 6: CI and documentation

**Status:** ⏸️ Not started | **Est. time:** ~30 min

### 6.1 — Run full CI

**Checklist:**
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace -- -D warnings`
- [ ] `cargo build --workspace`
- [ ] `cargo test --workspace`
- [ ] Fix any failures

### 6.Update — Update `ARCHITECTURE.md`

**Checklist:**
- [ ] Update the `error.rs` entry in Project Layout:
  - Old: `error.rs` — `RhoError` and `Result`
  - New: `client/error.rs` → `ClientError`, `agent/error.rs` → `AgentError`, etc.
- [ ] Update the Key Types table:
  - Remove `RhoError` row
  - Add rows for each domain error
  - Add `Retryable` trait
- [ ] Update `rho-tools` crate description to mention `ToolError` type
- [ ] Update `rho-tools/src/` module list to include `error.rs`
- [ ] Update Safety Layers table if `RhoError` was referenced
- [ ] If Option B chosen, document the thin `CoreError` boundary enum in the error types section

### 6.3 — Update `.plans/roadmap.md`

**Checklist:**
- [ ] Add entry for this refactor if tracking phases
- [ ] Mark as complete in the roadmap

### 6.4 — Final verification

**Checklist:**
- [ ] `cargo xtask ci` passes (fmt → lint → build → test)
- [ ] `cargo doc --workspace --no-deps --open` builds cleanly
- [ ] No remaining references to the old `RhoError` pattern (search: `RhoError::Unexpected`)
- [ ] `AGENTS.md` updated if any project conventions changed
- [ ] Commit with message: `refactor(errors): domain-specific error types (#phase)\nn\nReplace centralized RhoError with per-domain error enums.\nEach crate now owns its error type."
- [ ] Push and open PR