# Task 14: Cross-Platform Support, Auto-Detection, and Error Resilience — Completion Report

**Date:** 2026-05-01
**Branch:** Commit `5531a4c`
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Made rho run on Windows, macOS, and Linux by making path handling and process management platform-aware. Added auto-detection for project root and model selection. Added a compact system prompt for small-context-window models. Improved HTTP error handling for retryability and diagnostics.

### 1. Cross-platform path normalization

**`rho-tools/src/shell.rs`:**
- `normalize_path_separators()` now uses `cfg!(target_os = "windows")` — converts `/` to `\` only on Windows
- On non-Windows platforms, returns the command unchanged (forward slashes are native)
- Platform-specific tests gated with `#[cfg(target_os = "windows")]` and `#[cfg(not(target_os = "windows"))]`

### 2. Cross-platform process killing

**`rho-tools/src/shell.rs`:**
- `kill_process()` uses `taskkill /F /PID <pid>` on Windows and `kill -9 <pid>` on Unix
- `cfg!(target_os = "windows")` branching at runtime

### 3. Auto-detect project root

**`rho-core/src/sandbox.rs`:**
- Added `find_project_root()` — walks up from CWD looking for well-known project markers
- `PROJECT_MARKERS`: `.rho/config.toml`, `.git`, `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`
- Returns first directory containing any marker; falls back to CWD if none found
- Returns `SandboxRoot` directly (validated and canonicalized)

**`rho/src/main.rs`:**
- `--root` flag now optional — when omitted, auto-detects via `find_project_root()`

### 4. Auto-detect model from server

**`rho-core/src/client.rs`:**
- Added `list_models()` method on `LocalChatClient` — queries `/v1/models` endpoint
- Added `ModelInfo` and `ModelList` types for the `/v1/models` response
- `#[serde(default)]` on all `ModelUsage` fields for providers that omit them

**`rho/src/main.rs`:**
- Model resolution priority: config → CLI → auto-detect from server
- `resolve_model()` queries `/v1/models` and uses the first loaded model
- Clear error messages when the server is unreachable or has no models

### 5. Compact system prompt

**`rho-core/src/prompts/compact.md`:**
- New minimal system prompt (~100 tokens vs ~2,000 for base.md)
- Retains core identity, safety rules, and PowerShell mandate
- Omits idioms table, pipeline patterns, Rust commands, and detailed examples

**`rho-core/src/prompts.rs`:**
- Added `compact_prompt()` function, embedded from `prompts/compact.md`
- SHA-256 pinned in integration tests

**`rho/src/main.rs`:**
- Added `--compact` CLI flag — swaps the full prompt for the compact version

### 6. HTTP error retryability improvements

**`rho-core/src/error.rs`:**
- Added `RhoError::HttpError { status, message }` — preserves HTTP status code for retry classification
- Non-2xx responses previously wrapped as `RhoError::Http(reqwest::Error)`, which lost the status code and made all HTTP errors appear retryable (status `None`)
- `RetryBudgetExhausted` now carries the last error: `RetryBudgetExhausted(u32, Box<RhoError>)`
- `is_retryable()` refined: decode errors and builder errors are not retryable; only 429/500/502/503/504 are retryable for `HttpError`

**`rho-core/src/client.rs`:**
- `chat()` now reads the response body as text first, then checks status, then deserializes
- Non-2xx responses produce `RhoError::HttpError` with enhanced body diagnostics
- `enhance_http_body()` detects common error patterns (e.g., context window exceeded from llama.cpp) and provides actionable suggestions
- Parse failures include truncated response body in error message

### 7. `FinishReason::Other` for non-standard providers

**`rho-core/src/response.rs`:**
- Added `FinishReason::Other(String)` variant
- Custom `Deserialize` maps unknown finish reasons to `Other` instead of failing
- `ModelUsage` fields now have `#[serde(default)]` — providers that omit usage stats get zero defaults

### 8. `base.md` updated for platform neutrality

**`rho-core/src/prompts/base.md`:**
- Changed backslash-specific examples to forward slashes
- Added note about using platform-appropriate path separators
- Preserved PowerShell-specific guidance (still the default shell)

---

## Test Coverage

### New unit tests in `shell.rs` (5 tests, 4 platform-gated)

| Test | What it verifies |
|---|---|
| `normalize_preserves_forward_slash_unix` | Unix: forward slashes unchanged (gated `#[cfg(not(target_os = "windows"))]`) |
| `normalize_converts_path_slashes` | Windows: path slashes converted (gated `#[cfg(target_os = "windows")]`) |
| `normalize_converts_drive_colon_slash` | Windows: drive paths converted (gated) |
| `normalize_preserves_already_backslash` | Windows: backslashes unchanged (gated) |
| `normalize_mixed_slashes` | Windows: mixed slashes normalized (gated) |

### New unit tests in `client.rs` (3 tests)

| Test | What it verifies |
|---|---|
| `default_endpoint_derives_models_url` | `/v1/models` URL derived correctly |
| `custom_endpoint_derives_models_url` | Custom endpoint derives models URL |
| `trailing_slash_endpoint_still_derives_models_url` | Trailing slash handled |

### New integration tests (2 tests)

| Test | What it verifies |
|---|---|
| `compact_prompt_sha256_is_pinned` | Compact prompt hash pinned against changes |
| `compact_prompt_contains_required_sections` | Compact prompt has identity and rules |

### Test count progression

| Suite | Before | After |
|---|---|---|
| Total | 277 | 280 (+3) |

Note: Several path normalization tests were reorganized with `#[cfg]` gates rather than added — the total count is net new.

---

## File Changes

| File | Change |
|---|---|
| `rho-core/src/sandbox.rs` | Added `find_project_root()`, `PROJECT_MARKERS` |
| `rho-core/src/client.rs` | Added `list_models()`, `ModelInfo`, `ModelList`, `enhance_http_body()`, `truncate_error_body()`, body-first response handling, 3 unit tests |
| `rho-core/src/error.rs` | Added `RhoError::HttpError`, refined `is_retryable()`, `RetryBudgetExhausted` carries last error |
| `rho-core/src/response.rs` | Added `FinishReason::Other`, custom `Deserialize`, `#[serde(default)]` on `ModelUsage` |
| `rho-core/src/prompts.rs` | Added `compact_prompt()`, updated `base_prompt_warns_no_cmd_bypass` comment |
| `rho-core/src/prompts/base.md` | Updated for platform neutrality (forward slashes, notes) |
| `rho-core/src/prompts/compact.md` | **New** — compact system prompt |
| `rho-core/src/agent.rs` | Retry logging, `RetryBudgetExhausted` carries last error |
| `rho-core/src/config.rs` | Minor: updated `WireAgentLoopConfig` for new fields |
| `rho-core/src/lib.rs` | Re-export `ModelInfo`, `ModelList`, `compact_prompt` |
| `rho-tools/src/shell.rs` | Platform-aware `normalize_path_separators()`, `kill_process()`, `#[cfg]`-gated tests |
| `rho-tools/tests/shell_executor_tests.rs` | Updated tests for cross-platform shell behavior |
| `rho/src/main.rs` | Auto-detect project root, auto-detect model, `--compact` flag, `--root` optional, helper extraction |
| `rho-core/tests/integration_tests.rs` | Compact prompt SHA-256 pin test |
| `AGENTS.md` | Updated key types table with new types and variants |

---

## Design Decisions

1. **Runtime `cfg!()` over compile-time `#[cfg]` for core logic** — `normalize_path_separators()` and `kill_process()` use `cfg!(target_os = "windows")` in the function body rather than separate `#[cfg]`-gated implementations. This keeps the function signatures identical across platforms and makes the branching visible in one place.

2. **`#[cfg]` gates on tests, not on logic** — Platform-specific *tests* use `#[cfg(target_os = "windows")]` so they only run where relevant. Platform-specific *logic* uses runtime `cfg!()` so the binary is cross-platform without recompilation.

3. **`find_project_root()` walks up from CWD** — Simple and predictable. Searches for markers in priority order within each directory, then moves to the parent. The first directory with any marker wins.

4. **Model auto-detection via `/v1/models`** — When no model is specified, queries the server for loaded models and uses the first one. This matches the common LM Studio / Ollama workflow where a single model is loaded and the user just wants to start chatting.

5. **Compact prompt is opt-in** — The `--compact` flag is not auto-selected based on context window size. The user must explicitly choose it. Auto-detection of context window size from `/v1/models` is unreliable (many servers report incorrect values).

6. **Body-first response handling** — Reading the response body as text before deserializing allows: (a) including the body in error messages on parse failures, (b) detecting known error patterns in the body for enhanced diagnostics, and (c) preserving the status code for retry classification.

7. **`enhance_http_body()` detects llama.cpp patterns** — The `n_keep` + `n_ctx` pattern in a 400 response is the standard context-window-exceeded error from llama.cpp. Detecting it and providing actionable suggestions (use `--compact`, increase context, use bigger model) turns a cryptic server error into a clear user-facing message.

8. **`FinishReason::Other` is lossless** — Unknown finish reasons are preserved as strings, not discarded. This allows logging and debugging of non-standard provider behavior without failing the deserialization.

---

## Out of Scope (deferred)

| Item | Deferred to |
|---|---|
| Auto-detect context window size from `/v1/models` | Future (unreliable server reporting) |
| `BashExecutor` for Unix (non-PowerShell) | Future (PowerShell on Unix works via `pwsh`) |
| Shell auto-detection (`bash` vs `pwsh`) | Future |
| Platform-aware system prompt (PowerShell vs bash) | Future (compact prompt is shell-agnostic) |
