# Refactor: Clean up `rho-core/src/client.rs`

**Status:** 📋 Planned | **Branch:** `refactor/client-cleanup` | **Created:** 2026-05-14

## Motivation

The `rho-core/src/client.rs` module has some code smells:

1. **Duplicate default endpoint value** — The same URL appears in two places:
   - `DEFAULT_ENDPOINT` constant used only in `client_factory()`
   - Hardcoded in `LocalChatClient::new()` constructor

2. **Potentially redundant constructor** — `LocalChatClient::new()` may not be needed since:
   - `client_factory()` is the production path (used by both `rho` and `rho-bench`)
   - `with_endpoint()` is more flexible
   - Tests can use `with_endpoint()` or `client_factory()`

3. **Duplicate test comment** — Two `// ── Endpoint derivation ──────────────────────────────────────────────` comment blocks in tests

## Goal

Simplify and clarify the `client.rs` module by removing redundancy and improving code organization.

---

## Tasks

### Task 1: Remove duplicate default endpoint

**Problem:** The default endpoint URL is duplicated across the codebase.

**Current code:**
```rust
// Line ~200
const DEFAULT_ENDPOINT: &str = "http://localhost:1234/v1/chat/completions";

// Line ~48-53
pub fn new() -> Self {
    Self {
        http_client: Client::new(),
        endpoint: "http://localhost:1234/v1/chat/completions".to_owned(),  // Duplicated!
        api_key: None,
    }
}
```

**Solution steps:**
1. Replace `LocalChatClient::new()` implementation to use `DEFAULT_ENDPOINT`:
   ```rust
   pub fn new() -> Self {
       Self::with_endpoint(DEFAULT_ENDPOINT)
   }
   ```
2. Verify all tests still pass
3. Run `cargo test --package rho-core` to ensure no regressions

**Files to change:**
- `rho-core/src/client.rs`

**Verification:**
- All existing tests pass
- Behavior unchanged (default endpoint still `http://localhost:1234/v1/chat/completions`)
- No new clippy warnings

---

### Task 2: Evaluate whether to keep `new()` constructor

**Problem:** With `client_factory()` being the production path, `new()` may not provide much value.

**Options:**

**Option A: Keep `new()` as convenience** (conservative)
- Keep `new()` as a simple convenience method
- Value: Shorter than `with_endpoint("http://localhost:1234/v1/chat/completions")`
- Cost: One more method to maintain

**Option B: Remove `new()` entirely** (aggressive)
- Remove `new()` and rely on `with_endpoint()` or `client_factory()`
- Update any tests that use `new()` to use `with_endpoint()` instead
- Value: Simpler API, less surface area
- Cost: Slightly more verbose in tests

**Decision:** **Option A (keep `new()`)** — The convenience value justifies keeping it, especially for documentation and examples.

**Solution steps:**
1. Keep `new()` as-is (already using `DEFAULT_ENDPOINT` after Task 1)
2. Document in docstring that this is a convenience for default local endpoint
3. Update `Default` impl to explicitly call `new()` (already does this)

**Files to change:**
- `rho-core/src/client.rs` (documentation only)

**Verification:**
- Documentation clearly explains when to use `new()` vs `with_endpoint()` vs `client_factory()`

---

### Task 3: Remove duplicate test comment

**Problem:** Two identical comment sections exist in the test module.

**Current code:**
```rust
// Around line 218
// ── Endpoint derivation ──────────────────────────────────────────────

// Around line 316
// ── Endpoint derivation ──────────────────────────────────────────────
```

**Solution steps:**
1. Remove the duplicate comment at line ~316
2. Reorganize test module to have clear section headers:
   ```rust
   #[cfg(test)]
   mod tests {
       use super::*;

       // ── client_factory ───────────────────────────────────────────────

       // ... client_factory tests ...

       // ── resolve_api_key ────────────────────────────────────────────────

       // ... resolve_api_key tests ...

       // ── is_local_endpoint ────────────────────────────────────────────

       // ... is_local_endpoint tests ...

       // ── Endpoint derivation ────────────────────────────────────────────

       // ... list_models endpoint derivation tests ...
   }
   ```

**Files to change:**
- `rho-core/src/client.rs`

**Verification:**
- All tests still run and pass
- Test organization is clear and logical

---

### Task 4: Update documentation for constructors

**Problem:** The relationship between the three constructors isn't clearly documented.

**Current state:**
- `new()` — minimal doc
- `with_endpoint()` — minimal doc
- `with_endpoint_and_key()` — minimal doc
- `client_factory()` — good doc, but doesn't reference the constructors

**Solution steps:**
1. Add comprehensive documentation to each constructor explaining when to use it:
   ```rust
   /// Create a client at the default local endpoint.
   ///
   /// This is a convenience constructor for `with_endpoint(DEFAULT_ENDPOINT)`.
   /// For custom endpoints, use [`with_endpoint()`] or [`client_factory()`].
   pub fn new() -> Self { ... }

   /// Create a client at a custom endpoint URL.
   ///
   /// Use this for local servers with non-default ports or paths.
   /// For production use with config, prefer [`client_factory()`].
   pub fn with_endpoint(endpoint: impl Into<String>) -> Self { ... }

   /// Create a client at a custom endpoint URL with optional bearer authentication.
   ///
   /// Use this for quick configuration without loading config files.
   /// For production use with config, prefer [`client_factory()`].
   pub fn with_endpoint_and_key(endpoint: impl Into<String>, api_key: Option<String>) -> Self { ... }
   ```

2. Add cross-references between constructors
3. Update `client_factory()` docstring to mention it's the recommended production path

**Files to change:**
- `rho-core/src/client.rs`

**Verification:**
- Documentation renders correctly with `cargo doc --open`
- Each constructor's purpose is clear
- Relationships between constructors are documented

---

### Task 5: Verify all tests still pass

**Problem:** Need to ensure refactoring doesn't break anything.

**Solution steps:**
1. Run unit tests for `rho-core`:
   ```bash
   cargo test --package rho-core
   ```
2. Run integration tests:
   ```bash
   cargo test --package rho
   cargo test --package rho-bench
   ```
3. Run full CI pipeline:
   ```bash
   cargo xtask ci
   ```
4. Fix any failing tests

**Expected result:**
- All tests pass
- No new warnings
- Behavior unchanged

---

### Task 6: Update `AGENTS.md` (if needed)

**Problem:** If there are any references to `LocalChatClient::new()` that should use `client_factory()` instead, document the change.

**Solution steps:**
1. Search for any examples or documentation using `LocalChatClient::new()`
2. If found, update to use `client_factory()` or add context about when `new()` is appropriate
3. Ensure AGENTS.md doesn't recommend the wrong approach

**Files to change:**
- `AGENTS.md` (if needed)

**Verification:**
- Documentation is consistent with implementation

---

## Implementation Order

```
Task 1: Remove duplicate default endpoint (~5 min)
  ↓
Task 2: Evaluate whether to keep new() (~5 min)
  ↓
Task 3: Remove duplicate test comment (~5 min)
  ↓
Task 4: Update documentation for constructors (~15 min)
  ↓
Task 5: Verify all tests still pass (~10 min)
  ↓
Task 6: Update AGENTS.md (if needed) (~5 min)
```

**Total estimated time:** ~45 minutes

---

## Success Criteria

- [ ] Default endpoint is defined in only one place
- [ ] All constructors have clear documentation explaining when to use them
- [ ] Test module is well-organized with clear section headers
- [ ] All tests pass (`cargo test --package rho-core`)
- [ ] Full CI passes (`cargo xtask ci`)
- [ ] No new clippy warnings introduced
- [ ] Documentation builds successfully (`cargo doc --open`)

---

## Rollback Plan

If issues arise after this refactor:

```bash
# Revert changes and go back to trunk
git checkout trunk
git branch -D refactor/client-cleanup
git stash pop
```

All changes are self-contained within `rho-core/src/client.rs`, so rollback is straightforward.

---

## Related Work

- **Phase 3.4.0** — Added `client_factory()` and `client_factory` to `rho-core`
- **AGENTS.md** — May need updating to reflect recommended usage patterns
