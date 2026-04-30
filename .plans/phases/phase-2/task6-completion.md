# Task 6: Minimal Config Loader — Completion Report

**Date:** 2026-04-30
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Implemented a minimal config loader in `rho-core` that reads and merges two-tier TOML configuration files, with full integration into the agent loop, approval policy, command denylist, and binary entry point.

### New module: `rho-core/src/config.rs`

**Config types (all TOML-deserializable, all with sensible defaults):**

| Type | Section | Purpose |
|---|---|---|
| `RhoConfig` | (top-level) | Merged application-wide configuration |
| `AgentLoopConfig` | `[agent]` | Model, max_iterations, retry_budget, initial_backoff_ms |
| `ProviderConfig` | `[provider]` | Provider type, endpoint URL, API key env var reference |
| `ApprovalConfig` | `[approval]` | Per-tool approval policies (`Auto`/`Ask`/`Deny`) |
| `ShellConfig` | `[shell]` | Additional denied commands and flag combinations |
| `SandboxConfig` | `[sandbox]` | Sandbox on/off toggle (default: on) |
| `ContextConfig` | `[context]` | Override scan list for project context files |
| `EgressConfig` | `[egress]` | Allowed hosts (in addition to localhost) |
| `RedactionConfig` | `[redaction]` | Redaction on/off toggle (default: on) |
| `SystemPromptConfig` | `[system_prompt]` | Additional prompt extension fragments |

**Loader:**

- `ConfigLoader::load(root)` reads two config files and merges them:
  1. **User-level:** `~/.rho/config.toml` (global defaults)
  2. **Project-level:** `<root>/.rho/config.toml` (per-project overrides)
- Project-level fields override user-level fields on a per-field basis
- Missing files are not errors — defaults apply
- Malformed TOML is an error (`ConfigLoadError` with `Io` or `Parse` variant)

**Security features:**

- **API keys via env var references** — `api_key_env = "OPENAI_API_KEY"`; the key itself is never in the config file
- **Egress allowlist** — `is_host_allowed()` always allows localhost; other hosts must be listed
- **Sandbox opt-out** — `sandbox.enabled = false` (not recommended, but available)
- **Redaction toggle** — `redaction.enabled = false` (not recommended)

### New approval policy: `ConfigApprovalPolicy`

Added to `rho-core/src/approval.rs`:

- Checks per-tool overrides from `ApprovalConfig` before falling back to `DefaultApprovalPolicy`
- `Auto` — always allow without confirmation
- `Ask` — require human confirmation
- `Deny` — refuse the tool (goes through the approval gate so the gate can issue a denial)
- `is_denied()` method lets the agent loop distinguish denial from ask

### Integration into `AgentConfig`

Added `AgentConfig::from_config(&RhoConfig)` which builds a config-driven `AgentConfig` using `ConfigApprovalPolicy` as the approval policy.

### Integration into `CommandDenylist`

Added `CommandDenylist::from_config(&RhoConfig)` in `rho-tools` which builds on the default PowerShell denylist and appends config-supplied additions.

### Integration into binary

Updated `rho/src/main.rs`:
- Loads config via `ConfigLoader::load()` (warns on error, falls back to defaults)
- Uses config model (overriding CLI default)
- Uses `AgentConfig::from_config()` for the agent loop
- Passes config to `register_all()` for denylist integration

### Updated `register_all` signature

Changed from `register_all(registry, root)` to `register_all(registry, root, config: Option<&RhoConfig>)` so the denylist can incorporate config-supplied additions while maintaining backward compatibility (pass `None` for default behavior).

---

## Test Coverage

### New unit tests in `config.rs` (18 tests)

| Test | What it verifies |
|---|---|
| `load_with_no_files_returns_defaults` | All defaults when no config files exist |
| `load_project_config_overrides_defaults` | Full config file overrides all defaults |
| `project_overrides_user` | Project fields override user fields; user fields preserved when project doesn't set them |
| `user_config_only_applies_when_no_project_config` | User-only config works |
| `malformed_toml_returns_error` | Invalid TOML produces a `Parse` error |
| `unknown_keys_are_ignored` | Forward compatibility — unknown scalar fields don't break parsing |
| `partial_agent_config_preserves_defaults` | Only setting model preserves other defaults |
| `config_with_only_provider_section` | Single-section config works |
| `localhost_always_allowed` | Egress allowlist always includes localhost/127.0.0.1/::1 |
| `unknown_host_denied_by_default` | Unlisted hosts are blocked |
| `allowed_hosts_permitted` | Listed hosts are allowed |
| `resolve_api_key_returns_none_when_not_configured` | No api_key_env → None |
| `resolve_api_key_returns_none_when_env_var_not_set` | Missing env var → None |
| `sandbox_enabled_by_default` | Sandbox is on by default |
| `sandbox_can_be_disabled` | Config can disable sandbox |
| `approval_action_serde_round_trip` | ApprovalAction round-trips through serde |
| `approval_action_toml_deserialize` | ApprovalAction deserializes from TOML |
| `shell_denied_commands_from_config` | Shell denylist reads from config |

### New security tests (11 tests)

| Test | What it verifies |
|---|---|
| `config_approval_auto_overrides_default` | Per-tool Auto overrides risk-based default |
| `config_approval_ask_overrides_default` | Per-tool Ask overrides risk-based default |
| `config_approval_deny_requires_approval` | Per-tool Deny requires approval gate interaction |
| `config_approval_falls_back_to_default` | Unlisted tools use DefaultApprovalPolicy |
| `egress_localhost_always_allowed` | Config egress: localhost always OK |
| `egress_unknown_host_blocked_by_default` | Config egress: unknown hosts blocked |
| `config_sandbox_enabled_by_default` | Config sandbox: on by default |
| `config_sandbox_can_be_disabled` | Config sandbox: can be disabled |
| `config_redaction_enabled_by_default` | Config redaction: on by default |
| `config_redaction_can_be_disabled` | Config redaction: can be disabled |
| `config_api_key_not_in_plaintext` | Config never stores API keys in plaintext |

### New tool tests (2 tests)

| Test | What it verifies |
|---|---|
| `denylist_from_config_includes_builtins_and_extras` | Built-in + config entries both present |
| `denylist_from_config_case_insensitive` | Config-supplied commands are case-insensitive |

### Test count progression

| Suite | Before | After |
|---|---|---|
| rho-core unit | 49 | 67 (+18) |
| rho-core integration | 22 | 22 |
| rho-core security | 20 | 31 (+11) |
| rho-tools unit | 24 | 24 |
| rho-tools shell executor | 13 | 13 |
| rho-tools tool | 56 | 58 (+2) |
| **Total** | **184** | **215 (+31)** |

---

## All File Changes

| File | Change |
|---|---|
| `rho-core/src/config.rs` | **New** — config types, loader, merge logic, error types, 18 unit tests |
| `rho-core/src/lib.rs` | Add `config` module, re-export config types and `ConfigApprovalPolicy` |
| `rho-core/src/agent.rs` | Add `AgentConfig::from_config()` constructor |
| `rho-core/src/approval.rs` | Add `ConfigApprovalPolicy` with per-tool overrides and fallback |
| `rho-tools/src/shell.rs` | Add `CommandDenylist::from_config()` |
| `rho-tools/src/lib.rs` | Update `register_all()` to accept `Option<&RhoConfig>` |
| `rho/src/main.rs` | Load config, use config-driven model, `AgentConfig::from_config()`, pass config to `register_all()` |
| `rho-core/tests/security_tests.rs` | Add 11 config-related security tests |
| `rho-tools/tests/tool_tests.rs` | Add 2 `CommandDenylist::from_config` tests |
| `AGENTS.md` | Add config module to layout, add config types to Key Types table |

---

## Design Decisions

1. **Two-tier merging with per-field override** — Project-level fields override user-level fields. For `Vec` fields (denylist, egress hosts, scan list), the project list replaces the user list (no appending). This keeps overrides predictable and avoids surprising composition effects.

2. **`ConfigLoadError` with boxed `source`** — The error variant containing `toml::de::Error` is large; boxing avoids the clippy `result_large_err` lint while keeping the error informative.

3. **`ApprovalAction::Deny` requires approval** — A denied tool still goes through the approval gate so the gate can issue a structured denial message. The agent loop can use `is_denied()` to distinguish denial from ask if needed in the future.

4. **API keys as env var references** — Config stores the *name* of an environment variable, not the key itself. `RhoConfig::resolve_api_key()` reads the env var at runtime. This is a security requirement: no plaintext secrets in config files.

5. **`register_all` takes `Option<&RhoConfig>`** — Allows existing callers (tests) to pass `None` for default behavior while the binary passes `Some(&config)`. This avoids breaking the test suite.

6. **Missing config files are not errors** — `ConfigLoader::load()` returns defaults when neither file exists. Malformed TOML *is* an error. This matches the principle of least surprise: an unconfigured rho should just work with defaults.

7. **`toml` crate already in dependency tree** — Used by `context_files.rs` for the trust store. No new external dependency was added.

---

## Out of Scope (deferred)

| Item | Deferred to |
|---|---|
| Custom redaction patterns (config-driven) | Task 7 |
| Provider switch warning (external provider consent) | Task 10 |
| Config-driven `LocalChatClient` endpoint selection | Task 10 |
| Egress allowlist enforcement in `LocalChatClient` | Task 10 |
| Config-driven context scan list in `ContextScanner` | Future wiring |
| Config-driven system prompt extensions | Future wiring |
| Config-driven redaction toggle in `Conversation` | Future wiring |
