# Task 7: Secret Redaction Improvements — Completion Report

**Date:** 2026-04-30
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Extended the `Redactor` with config-driven custom regex patterns and a disable toggle, and wired both into the conversation and binary.

### 1. Custom regex patterns from config

**`rho-core/src/redact.rs`:**
- `Redactor` now holds `enabled: bool` and `custom_regexes: Vec<regex::Regex>`
- `Redactor::from_config(enabled, custom_patterns)` — compiles regex patterns from config strings, silently skipping invalid ones (with stderr warning)
- `Redactor::is_enabled()` — returns whether redaction is active
- `Redactor::redact()` — when disabled, returns input unchanged; when enabled, applies built-in patterns then custom regexes
- `Redactor::new()` and `Default` remain backward-compatible (enabled, no custom patterns)

**New dependency:** `regex = "1"` in `rho-core/Cargo.toml`

### 2. Disable toggle wired into Conversation

**`rho-core/src/conversation.rs`:**
- Added `Conversation::with_redactor(redactor: Redactor)` builder method (consistent with `with_context_manager` and `with_token_budget`)
- `push_tool_result` already called `self.redactor.redact()` — now respects the `enabled` flag

**`rho/src/main.rs`:**
- Constructs `Redactor::from_config(rho_config.redaction.enabled, &rho_config.redaction.custom_patterns)`
- Passes it to `Conversation::new(...).with_redactor(redactor)`

### 3. Config: `custom_patterns` field

**`rho-core/src/config.rs`:**
- Added `custom_patterns: Vec<String>` to `RedactionConfig`
- Added `WireRedactionConfig` with per-field `Option<bool>` enabled and `Option<Vec<String>>` custom_patterns
- Updated merge logic: `custom_patterns` follows the "project replaces user" rule (same as other `Vec` fields)
- `enabled` follows per-field override (project overrides user, default `true`)
- TOML deserialization: `[redaction] custom_patterns = ["my-key-[a-zA-Z0-9]{32}", "token: \\S+"]`

---

## Test Coverage

### New unit tests in `redact.rs` (8 tests)

| Test | What it verifies |
|---|---|
| `disabled_redactor_returns_input_unchanged` | `enabled = false` passes secrets through unchanged |
| `enabled_redactor_redacts_normally` | `enabled = true` applies built-in patterns |
| `is_enabled_reflects_state` | `is_enabled()` returns correct bool |
| `custom_regex_pattern_redacts` | Custom regex pattern catches matches |
| `custom_pattern_applied_after_builtin` | Custom patterns run after built-ins |
| `custom_pattern_and_builtin_both_match` | Both built-in and custom patterns can match in same text |
| `invalid_custom_pattern_is_skipped` | Invalid regex doesn't panic; built-in patterns still work |
| `no_custom_patterns_is_same_as_new` | `from_config(true, &[])` produces same behavior as `new()` |
| `multiple_custom_patterns` | Multiple custom patterns all apply |

### New unit tests in `config.rs` (4 tests)

| Test | What it verifies |
|---|---|
| `redaction_custom_patterns_from_config` | TOML loading and deserialization of custom patterns |
| `redaction_project_custom_patterns_replace_user` | Project patterns replace user patterns (no appending) |
| `redaction_user_patterns_preserved_when_no_project` | User patterns apply when project doesn't set them |
| `redaction_enabled_from_project_overrides_user` | Per-field override of `enabled` bool |
| `redaction_defaults_to_enabled_empty_patterns` | Default `RedactionConfig` is enabled with no custom patterns |

### New security tests (2 tests)

| Test | What it verifies |
|---|---|
| `disabled_redactor_skips_builtin_patterns` | End-to-end: disabled redactor passes `sk-` key through Conversation unchanged |
| `enabled_redactor_with_custom_pattern_redacts_in_conversation` | End-to-end: custom regex pattern redacts in Conversation |

### Test count progression

| Suite | Before | After |
|---|---|---|
| rho-core unit | 67 | 81 (+14) |
| rho-core integration | 22 | 22 |
| rho-core security | 31 | 33 (+2) |
| **Total** | **144** | **160 (+16)** |

---

## All File Changes

| File | Change |
|---|---|
| `rho-core/Cargo.toml` | Added `regex = "1"` dependency |
| `rho-core/src/redact.rs` | Extended `Redactor` with `enabled` field, `custom_regexes`, `from_config()`, `is_enabled()`, updated doc comments, 8 new unit tests |
| `rho-core/src/config.rs` | Added `custom_patterns: Vec<String>` to `RedactionConfig`, added `WireRedactionConfig` with per-field merge, 5 new config tests |
| `rho-core/src/conversation.rs` | Added `with_redactor()` builder method |
| `rho-core/tests/security_tests.rs` | Fixed `RedactionConfig` construction for new field, added 2 end-to-end redaction security tests |
| `rho/src/main.rs` | Constructs config-driven `Redactor` and passes to `Conversation` |

---

## Design Decisions

1. **`regex` crate as foundation dependency** — The standard Rust regex library. Custom patterns need regex for user-expressible pattern matching. The built-in fast scanner is retained for known prefixes; regex is only used for custom patterns.

2. **Built-in patterns run first, then custom patterns** — Built-in patterns are more precise (prefix+body matching) and run first. Custom regexes are applied after. A match by any pattern replaces text with `[REDACTED]`, which doesn't match any built-in prefix, so there's no re-scanning issue.

3. **Invalid regex patterns are silently skipped** — Config errors shouldn't crash the agent. Invalid patterns print a warning to stderr and are dropped. The built-in patterns still apply.

4. **`Redactor::from_config()` takes primitives** — Takes `enabled: bool` and `custom_patterns: &[String]` rather than `&RedactionConfig` to avoid coupling the redactor module to the config module types. The binary extracts the values.

5. **`enabled = false` is a complete bypass** — When disabled, `redact()` returns the input string directly without any pattern scanning. This is the fastest possible path and makes the intent explicit.

6. **Custom patterns use "project replaces user" merge rule** — Consistent with other `Vec` fields (`denied_commands`, `allowed_hosts`). The project-level list replaces, not appends, the user-level list.

7. **`enabled` uses per-field merge** — Project's `enabled` overrides user's `enabled`. Default is `true`.

8. **`Conversation::with_redactor()` builder** — Consistent with existing builder methods (`with_context_manager`, `with_token_budget`). Backward compatible: `Conversation::new()` still creates a default (enabled, no custom patterns) redactor.

---

## Out of Scope (deferred)

| Item | Deferred to |
|---|---|
| Config-driven context scan list wiring in `ContextScanner` | Future wiring |
| Config-driven system prompt extensions wiring | Future wiring |
| Config-driven `LocalChatClient` endpoint selection | Task 10 |
| Egress allowlist enforcement in `LocalChatClient` | Task 10 |
| Provider switch warning (external provider consent) | Task 10 |