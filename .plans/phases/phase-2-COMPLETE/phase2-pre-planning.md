# Phase 2 Pre-Planning Summary

## Codebase Status & Phase 2 Plan Review

### Current Codebase State (v0.5.0)

**Phases 1a + 1b are complete.** The agent loop, data model, security surface, and REPL are all working. A quick status check shows:

- ✅ **`cargo fmt`** — passes
- ✅ **`cargo clippy`** — passes (pedantic + cargo lints)
- ⚠️ **`cargo test`** — 1 failure: `base_prompt_sha256_is_pinned` (the hash in the test is stale — the actual hash is `79e4b4...` but the test expects `c600c6...`). This is a pre-existing issue, not a Phase 2 concern.

**What exists today (key files, ~3,700 LOC total):**

| Area | Status |
|---|---|
| **Agent loop** (`agent.rs`) | ✅ Complete. Currently processes only the **first** tool call — explicitly noted as "Phase 2 will iterate the full Vec" |
| **Tool trait + registry** (`tool.rs`) | ✅ Complete with `CancellationToken`, `ToolOutcome::Immediate/Streamed`, `ToolRisk` |
| **Conversation** (`conversation.rs`) | ✅ Complete. Redaction wired into `push_tool_result`. Tool-call persistence fixed. |
| **Messages** (`message.rs`) | ✅ Complete. Full serde round-tripping, `ContentBlock::Text`, `ModelToolCall`, etc. |
| **Client** (`client.rs`) | ✅ `ChatClient` trait + `LocalChatClient`. No egress allowlist yet. |
| **Approval** (`approval.rs`) | ✅ `ApprovalPolicy` trait + `DefaultApprovalPolicy` (auto-approve Read, require Write/Destructive). No per-tool config-driven policy yet. |
| **Sandbox** (`sandbox.rs`) | ✅ `SandboxRoot` with `validate` and `validate_for_write`. |
| **Redaction** (`redact.rs`) | ✅ 5 built-in patterns (OpenAI, GitHub, Slack, AWS, Bearer). No custom patterns or disable toggle yet. |
| **Context files** (`context_files.rs`) | ✅ Scanner + `TrustStore` + `compose_system_prompt`. |
| **Tools** (`rho-tools/`) | ✅ `ReadFile`, `WriteFile`, `RunCommand` (basic). No `ListDir`, `EditFile`, denylist, or `ShellExecutor` abstraction. |
| **Config** | ❌ Does not exist. No `.rho/config.toml` loading. |
| **ChatRequest.tools** (`request.rs`) | ✅ `tools: Vec<ToolSchema>` field exists and is serialized — task 9 may already be done. |

---

### Phase 2 Plan — 13 Tasks

| # | Task | What's needed | Overlap with existing code |
|---|---|---|---|
| **1** | Expand `RunCommand` | `pwsh` vs `powershell` detection, execution policy flags, path separator normalization, structured output (stdout/stderr/exit code), timeout, **command denylist**, working directory enforcement | Structured output + working dir already done. Need: `pwsh` fallback, denylist, timeout, path normalization, execution policy flags |
| **2** | `ShellExecutor` trait in `rho-core` | Abstract shell execution: `execute`, capture output, timeout. `PowerShellExecutor` as default. `RunCommand` depends on trait, not PowerShell directly. | Brand new. `RunCommand` currently shells out directly. |
| **3** | `ListDir` tool | Recursive directory listing with `.gitignore` awareness | Brand new. |
| **4** | `EditFile` tool | Exact-match replacement, non-overlapping edits, validation (refuse if not found or ambiguous) | Brand new. |
| **5** | Multi-tool-call in agent loop | Iterate full `Vec<ModelToolCall>` instead of `.next()` only. Execute sequentially, append each result. | The loop currently takes `calls.into_iter().next()` — needs to iterate all. |
| **6** | Config loader in `rho-core` | Read `.rho/config.toml` + `~/.rho/config.toml`. Model, system prompt extensions, per-tool approval policies, command denylist, sandbox opt-out, context file scan list, provider selection, API key via env var, egress allowlist. New dep: `toml` (already in Cargo.lock). | Brand new. No config infrastructure exists. |
| **7** | Secret redaction improvements | Tool results → redaction → conversation. Custom patterns, disable toggle. | `Redactor` exists with 5 built-in patterns. Needs: configurable custom patterns + disable flag. |
| **8** | PowerShell-aware system prompt | "You are running on Windows. Use PowerShell." + idioms + few-shot examples. | `base.md` already says PowerShell-first. May need enrichment. |
| **9** | `ChatRequest.tools` serialization | Tool definitions sent to the model API. | **Already done.** `ChatRequest.tools: Vec<ToolSchema>` exists and `ToolSchema` implements `Serialize`. The `Conversation::send_current` includes `self.tools.clone()` in the request. |
| **10** | Provider switch warning | When config selects external provider, display warning on startup. Enforced in binary, not provider trait. | Brand new (needs config first). |
| **11** | Deserialization tests for tool-call responses | JSON fixtures with `finish_reason: "tool_calls"`. | One fixture exists (`tool_call.json`). Need more. |
| **12** | Security tests | Command denylist, egress allowlist, secret redaction, credential storage, approval policy per-tool. | Some redaction + approval tests exist. Need denylist, egress, credential, per-tool policy tests. |
| **13** | Test suite audit | Promote PowerShell detection to `rho-test-helpers`, extract tempdir helpers, `EditFile` test matrix, fixture consolidation, config loading tests, security test isolation. | Some helpers already in `rho-test-helpers`. |

---

### Key Observations

1. **Task 9 is already done.** `ChatRequest` serializes `tools: Vec<ToolSchema>` and `ToolSchema` has `#[derive(Serialize)]`. The conversation sends tool schemas with every request.

2. **Task 5 is the highest-impact single change.** The agent loop's `.next()` on tool calls is the most glaring Phase 1 limitation. It's a small code change (iterate all calls instead of one) but has wide implications for the conversation persistence invariant (each tool result must follow the persisted assistant message).

3. **Task 6 (config) is the largest new subsystem.** It touches every other task — denylist config, per-tool approval, egress allowlist, provider selection, API key management. It should probably be designed early even if implemented incrementally.

4. **Task 2 (ShellExecutor trait) is an architectural prerequisite for Task 1.** Before expanding `RunCommand`, the `ShellExecutor` abstraction should land so `RunCommand` depends on the trait rather than PowerShell directly.

5. **The `toml` crate is already in the Cargo.lock** (used by `context_files.rs` for the trust store), so adding it to `rho-core`'s dependencies is a non-event.

6. **The pre-existing test failure** (`base_prompt_sha256_is_pinned`) should be fixed before starting Phase 2 work — CI won't pass otherwise.

7. **Suggested implementation order** based on dependencies:
   - Fix the SHA-256 test (prerequisite for CI)
   - Task 2: `ShellExecutor` trait
   - Task 1: Expand `RunCommand` (depends on ShellExecutor)
   - Task 5: Multi-tool-call loop
   - Task 4: `EditFile`
   - Task 3: `ListDir`
   - Task 6: Config loader (enables denylist integration, per-tool approval, egress, provider)
   - Task 7: Redaction improvements (config-driven patterns)
   - Task 8: PowerShell prompt enrichment
   - Task 10: Provider switch warning (needs config)
   - Task 11: Deserialization fixtures
   - Task 12: Security tests
   - Task 13: Test suite audit
