# Phase 2 Tasks

1. **Expand `RunCommand`:**
   - Detect `pwsh` vs `powershell` availability
   - Set appropriate execution policy flags
   - Normalize path separators in arguments
   - Capture and return structured output (stdout, stderr, exit code)
   - Timeout support
   - **Command denylist** — refuse execution of dangerous commands by default: `Remove-Item`, `Invoke-WebRequest`, `Invoke-RestMethod`, `Start-Process`, `New-Service`, `Set-ExecutionPolicy`, and any command with `-Recurse -Force`. The denylist is configurable in `.rho/config.toml`.
   - **Working directory** — execute within the sandbox root. Flag commands that attempt to `cd` outside the project directory.

2. **Abstract shell execution behind a `ShellExecutor` trait in `rho-core`:**
   - `ShellExecutor` defines the interface for running commands (execute, capture output, timeout)
   - `PowerShellExecutor` is the first (and default) implementation
   - This abstraction makes cross-platform support a future possibility (e.g., `BashExecutor` for Unix) without rewriting the tool layer
   - `RunCommand` depends on the trait, not on PowerShell directly

3. **Add `ListDir` tool** (recursive directory listing with `.gitignore` awareness).

4. **Add `EditFile` tool:**
   - Exact-match replacement (old text → new text)
   - Non-overlapping edits in a single call
   - Validation: refuse if old text is not found or is ambiguous
   - Tree-sitter node-splitting validation is deferred to Phase 3 — exact-match only in Phase 2 keeps the feedback loop fast

5. **Handle multi-tool-call responses in the agent loop:**
   - The `AssistantResponse` already carries `Vec<ModelToolCall>` (from Phase 1)
   - The loop now iterates over all tool calls, executing them in sequence and appending each `Role::Tool` result before re-sending
   - Parallel execution is a future optimisation (tracked in Design Decisions)

6. **Implement a minimal config loader in `rho-core`:**
   - Read `.rho/config.toml` for: model selection, system prompt extensions, approval policies (per-tool, not just read/write), command denylist, sandbox opt-out, project context file scan list (overrides the default: `AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`)
   - Read `~/.rho/config.toml` for: default model, API endpoint, provider selection (`local`, `openai`, `anthropic`, etc.), egress allowlist
   - Provider configuration is a first-class concern: the config specifies which `ChatClient` implementation to use and its settings (base URL, model overrides)
   - **API keys are never stored in plaintext config.** Config references environment variables: `api_key_env = "OPENAI_API_KEY"`. The provider reads the key from the env var at runtime. If a system credential store is available (Windows Credential Manager), it may be used via an optional `keyring` dependency.
   - **Egress allowlist** — `LocalChatClient` defaults to `localhost` only. External providers add their API hostname. The agent refuses to contact hosts not on the allowlist.
   - This splits config loading out of Phase 5 so the TUI (Phase 4) can use approval policies and provider selection
   - Full extension/plugin API remains in Phase 5
   - Uses `toml` crate **(foundation)** — added as a dependency in this phase

7. **Implement secret redaction in `rho-core`:**
   - Tool results pass through a redaction layer before being appended to the conversation as `Role::Context` messages
   - Patterns: `sk-[a-zA-Z0-9]{20,}` (OpenAI), `ghp_[a-zA-Z0-9]{36}` (GitHub), `xox[bpas]-[a-zA-Z0-9-]+` (Slack), env var values matching common secret patterns
   - Redacted to `[REDACTED]`
   - This prevents secrets from being sent to model APIs or persisted in conversation logs
   - Configurable: users can add custom patterns or disable redaction (not recommended)

8. **Compose a PowerShell-aware system prompt:**
   - "You are running on Windows. Use PowerShell commands."
   - Common PowerShell idioms for file operations, process management, etc.
   - Few-shot examples of correct PowerShell usage

9. **Add the `ChatRequest.tools` serialization** so tool definitions are sent to the model API.

10. **Add a provider switch warning:** when the config selects an external provider (not `local`), the agent displays a clear warning on startup: "Your conversation, including file contents, will be sent to [provider]. Continue? [y/n]". This is enforced in the binary, not the provider trait — it's a user consent concern, not a provider capability.

11. **Add deserialization tests** for tool-call responses (JSON fixtures with `finish_reason: "tool_calls"`).

12. **Add security tests:**
    - Command denylist: verify `RunCommand` refuses denied commands and allows others
    - Egress allowlist: verify `LocalChatClient` only contacts `localhost`, verify external providers only contact their allowlisted host
    - Secret redaction: verify tool results containing API keys and tokens are redacted before entering conversation history
    - Credential storage: verify config never writes API keys to plaintext, verify env var lookup works
    - Approval policy per-tool: verify custom policies from config are respected

13. **Make the token budget configurable:**
    - Add `token_budget: Option<u32>` to `AgentLoopConfig` in `rho-core/src/config.rs` (default: `32768` when `None`)
    - Raise `TokenBudget` default from `8_192` to `32_768` — the original 8K default was a Phase 1a placeholder that leaves only ~3.5K tokens for conversation after the system prompt, which is insufficient for even 1–2 tool-call rounds
    - Wire the config field through to `Conversation::with_token_budget()` in the binary
    - Add `--token-budget` CLI flag for quick override without editing config
    - The `ContextManager` trait and `SlidingWindowContextManager` are unchanged — this task only changes the budget *value*, not the eviction strategy
    - Document the config field in the TOML schema: `[agent] token_budget = 32768`
    - **Why this wasn't in the original task list:** The 8K default was set in Phase 1a as a conservative placeholder. It was never revisited because the plan assumed context management strategy (summarisation, retrieval-augmented) was a far-future concern. Real-world testing with mid-size models (qwen3-14B) revealed the budget is the binding constraint *before* strategy matters — the model can't self-correct when it only has room for 1–2 turns. Making the budget configurable is the pragmatic fix; sophisticated strategies remain a Phase 5+ concern.

14. **Cross-platform support, auto-detection, and error resilience:**
    - Make path normalization platform-aware (`/` → `\` on Windows only; identity on Unix)
    - Make process killing platform-aware (`taskkill` on Windows; `kill -9` on Unix)
    - Auto-detect project root via `find_project_root()` walking up from CWD looking for markers
    - Auto-detect model from server `/v1/models` endpoint when not specified in config or CLI
    - Add `--compact` flag and `compact.md` system prompt for small-context-window models
    - Add `RhoError::HttpError` variant to preserve HTTP status for retry classification
    - Add `FinishReason::Other(String)` for non-standard providers
    - Add `serde(default)` on `ModelUsage` fields for providers that omit them
    - Add `ModelInfo`/`ModelList` types for `/v1/models` endpoint
    - Improve error messages: context-window-exceeded diagnostics, truncated body snippets
    - Gate platform-specific tests with `#[cfg]`
    - **Why this wasn't in the original task list:** The Phase 2 plan was Windows/PowerShell-first. Real-world testing revealed that developers run on macOS/Linux too, and PowerShell (`pwsh`) is available cross-platform. Making rho work on all three platforms was a natural extension that also improved the Windows experience (better error handling, auto-detection, compact prompt).

15. **Test suite audit:**
    - Promote PowerShell command execution into `rho-test-helpers` (handle `pwsh` vs `powershell` detection once)
    - Extract file-system test fixtures into a tempdir helper in `rho-test-helpers` (create/verify/cleanup)
    - Ensure `EditFile` tests cover: exact match, ambiguous match, no match, overlapping edits
    - Deduplicate any JSON fixture overlap with Phase 1 fixtures — consolidate into shared fixture files
    - Ensure config loading tests cover: missing files, malformed TOML, unknown keys
    - Ensure security tests are isolated and deterministic (no real network calls, no real credential store access)
