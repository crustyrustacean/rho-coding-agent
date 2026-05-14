# Tasks — Refactor: Clean up `rho-core/src/client.rs`

Detailed checklist of tasks to complete the client.rs cleanup refactor.

---

## Task 1: Remove duplicate default endpoint

**Status:** ⏸️ Not started

**Checklist:**
- [ ] Identify duplicate default endpoint in code
- [ ] Replace `new()` to use `DEFAULT_ENDPOINT` constant
- [ ] Run `cargo test --package rho-core` to verify no regressions
- [ ] Confirm default endpoint still resolves to `http://localhost:1234/v1/chat/completions`
- [ ] No new clippy warnings

**Commands:**
```bash
# Run tests
cargo test --package rho-core

# Check for clippy warnings
cargo clippy --package rho-core -- -D warnings
```

---

## Task 2: Evaluate whether to keep `new()` constructor

**Status:** ⏸️ Not started

**Decision:** Keep `new()` as convenience method (conservative approach)

**Checklist:**
- [ ] Document decision in plan
- [ ] Update `new()` docstring to clarify it's a convenience for `with_endpoint(DEFAULT_ENDPOINT)`
- [ ] Update docstring to reference `with_endpoint()` and `client_factory()` as alternatives
- [ ] Verify `Default` impl is consistent

**Code changes:**
```rust
/// Create a client at the default local endpoint
/// (`http://localhost:1234/v1/chat/completions`).
///
/// This is a convenience constructor for `with_endpoint(DEFAULT_ENDPOINT)`.
/// For custom endpoints, use [`with_endpoint()]`. For production use
/// with config, prefer [`client_factory()`].
pub fn new() -> Self {
    Self::with_endpoint(DEFAULT_ENDPOINT)
}
```

---

## Task 3: Remove duplicate test comment

**Status:** ⏸️ Not started

**Checklist:**
- [ ] Find duplicate `// ── Endpoint derivation ──────────────────────────────────────────────` comment (around line 316)
- [ ] Remove the duplicate comment
- [ ] Reorganize test module with clear section headers
- [ ] Run all tests to ensure no issues

**Test organization after restructure:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── client_factory ───────────────────────────────────────────────
    // [client_factory tests]

    // ── resolve_api_key ────────────────────────────────────────────────
    // [resolve_api_key tests]

    // ── is_local_endpoint ────────────────────────────────────────────
    // [is_local_endpoint tests]

    // ── Endpoint derivation (list_models) ───────────────────────────
    // [endpoint derivation tests]
}
```

---

## Task 4: Update documentation for constructors

**Status:** ⏸️ Not started

**Checklist:**
- [ ] Update `new()` docstring with cross-references
- [ ] Update `with_endpoint()` docstring with cross-references and usage guidance
- [ ] Update `with_endpoint_and_key()` docstring with usage guidance
- [ ] Update `client_factory()` docstring to mention constructors
- [ ] Build documentation: `cargo doc --package rho-core --open`
- [ ] Verify all links resolve correctly

**Docstring templates:**

### `new()`:
```rust
/// Create a client at the default local endpoint
/// (`http://localhost:1234/v1/chat/completions`).
///
/// This is a convenience constructor equivalent to
/// `with_endpoint(DEFAULT_ENDPOINT)`.
///
/// # When to use
///
/// - Quick local development with LM Studio or Ollama at default port
/// - Examples and documentation
///
/// # When not to use
///
/// - Custom port or path: use [`with_endpoint()`]
/// - Production with config: use [`client_factory()`]
pub fn new() -> Self { ... }
```

### `with_endpoint()`:
```rust
/// Create a client at a custom endpoint URL.
///
/// # When to use
///
/// - Local server at non-default port or path
/// - Quick configuration without loading config files
///
/// # When not to use
///
/// - Production with config: use [`client_factory()`]
/// - Need API key authentication: use [`with_endpoint_and_key()`]
pub fn with_endpoint(endpoint: impl Into<String>) -> Self { ... }
```

### `with_endpoint_and_key()`:
```rust
/// Create a client at a custom endpoint URL with optional bearer authentication.
///
/// # When to use
///
/// - External API providers (OpenRouter, OpenAI, etc.)
/// - Quick configuration without loading config files
///
/// # When not to use
///
/// - Production with config: use [`client_factory()`]
pub fn with_endpoint_and_key(endpoint: impl Into<String>, api_key: Option<String>) -> Self { ... }
```

### `client_factory()` (update existing):
```rust
/// Construct a fully-configured [`LocalChatClient`] from [`RhoConfig`].
///
/// This is the **recommended** way to construct clients in production.
/// It respects config values, handles API key resolution from environment
/// variables, and applies CLI overrides.
///
/// For quick testing without config, you may use [`new()`], [`with_endpoint()`],
/// or [`with_endpoint_and_key()`] directly.
pub fn client_factory(...) -> LocalChatClient { ... }
```

---

## Task 5: Verify all tests still pass

**Status:** ⏸️ Not started

**Checklist:**
- [ ] Run unit tests: `cargo test --package rho-core`
- [ ] Run integration tests: `cargo test --package rho`
- [ ] Run bench tests: `cargo test --package rho-bench`
- [ ] Run full CI: `cargo xtask ci`
- [ ] Verify test count is unchanged
- [ ] No new warnings or errors

**Commands:**
```bash
# Unit tests
cargo test --package rho-core

# Integration tests
cargo test --package rho
cargo test --package rho-bench

# Full CI
cargo xtask ci

# Count tests before and after
cargo test --package rho-core 2>&1 | grep "test result"
```

---

## Task 6: Update `AGENTS.md` (if needed)

**Status:** ⏸️ Not started

**Checklist:**
- [ ] Search for `LocalChatClient::new()` usage in documentation
- [ ] Update any examples that should use `client_factory()` instead
- [ ] Add note about when `new()` is appropriate vs `client_factory()`
- [ ] Verify documentation examples work correctly

**Search command:**
```bash
grep -n "LocalChatClient::new()" AGENTS.md
grep -n "client_factory" AGENTS.md
```

**Example documentation update (if needed):**
```markdown
## Client Configuration

rho provides three ways to construct a chat client:

### Quick Local Development

```rust
use rho_core::LocalChatClient;

let client = LocalChatClient::new();  // Default: http://localhost:1234
```

### Production with Config

```rust
use rho_core::{RhoConfig, client_factory};

let config = ConfigLoader::load()?;
let client = client_factory(&config, None, None);  // Respects config.toml
```

### Custom Endpoint

```rust
let client = LocalChatClient::with_endpoint("http://localhost:8080/v1/chat/completions");
```
```

---

## Completion Checklist

**Overall Progress:** 0/6 tasks started

**Ready to merge when:**
- [ ] All tasks complete
- [ ] All tests pass (`cargo xtask ci`)
- [ ] No new clippy warnings
- [ ] Documentation builds successfully
- [ ] Code review completed
- [ ] Change documented in CHANGELOG.md

---

## Notes

- **Estimated total time:** ~45 minutes
- **Risk level:** Low (self-contained refactor, tests provide good coverage)
- **Impact:** Improves code maintainability and documentation clarity
- **Breaking changes:** None (API unchanged)
