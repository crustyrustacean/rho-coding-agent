# Phase 4 Tasks

1. **Create the `rho-tui` crate** with `ratatui` + `crossterm`.

2. **Implement the input pane:**
   - Multi-line editor (Shift+Enter for newline, Enter to submit)
   - Command history (up/down arrows)
   - Autocomplete for tool names and file paths
   - Slash-command parsing and dispatch (invokes `rho-core` APIs from the command surface table: `/clear`, `/history`, `/system`, `/model`, `/provider`, `/tools`, `/context`, `/config`, `/help`, `/quit`)
   - Slash-command parsing uses `Session` APIs: `/clear` branches to root, `/history` shows `path_messages()`
   - Session navigation commands (new): `/tree` shows the session tree, `/fork` branches, `/resume` lists saved sessions

3. **Implement the output pane:**
   - Markdown rendering (headers, bold, code blocks)
   - Syntax-highlighted code blocks via `rho-highlight` (supports Rust, PowerShell, TOML, JSON, Markdown)
   - Diff view for file edits (show old → new, both syntax-highlighted)
   - Inline diagnostic context with syntax-highlighted source lines
   - Incremental re-rendering: as streaming tokens arrive, only re-parse the changed region

4. **Implement tool call display:**
   - Show tool name, risk level, and arguments before execution
   - Show tool result after execution (collapsible), with secrets already redacted by `rho-core`
   - Ask for user approval on destructive operations (WriteFile, EditFile, RunCommand) — the approval gate is in `rho-core`, but the TUI renders a rich preview: the full command or diff, the file path, and the risk level
   - Allow/deny/skip approval with a single keypress
   - Batch approval: when the model returns multiple tool calls, the TUI can present them all and allow batch approve/deny

5. **Implement streaming output:**
   - ✅ Streaming API implemented (Pre-Work 1): `ChatClient::chat_stream` returns `Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>` with default impl wrapping `chat`
   - ✅ SSE parsing with line buffering in `LocalChatClient`
   - ✅ `run_loop` uses streaming path; `StreamChunk::accumulate()` reconstructs `AssistantResponse`
   - ✅ SSE parser fixed for external providers: tool-call deltas first, empty content skipped
   - ✅ `rho-bench` `CountingClient` delegates `chat_stream()` to inner client
   - ✅ `ChatStream` re-exported from `rho-core`
   - ✅ Validated against 4 remote models via OpenRouter (Gemini 2.0 Flash, Gemma 4 26B, DeepSeek v4 Flash, GLM 5.1)
   - 🔜 TUI integration: render tokens as they arrive, show "thinking" indicator while waiting

6. **Implement a diagnostic panel:**
   - Render structured `Diagnostic` objects with file/line context
   - Color-code severity (error = red, warning = yellow)
   - Show suggested fix inline

7. **Implement model-aware context sizing:**
   - Query the OpenAI-compatible `/v1/models` endpoint (or equivalent) for the loaded model's `max_context_length`
   - Auto-size the `TokenBudget` to match the model's actual context window, unless the user has explicitly overridden it via config or CLI
   - Display the resolved budget in the status bar
   - The `HeuristicEstimator` (Phase 2.5) already calibrates per-model ratios; this task adds the *initial* auto-sizing of the `context_window` component.
   - Warn if the system prompt alone exceeds 50% of the budget

8. **Status bar:**
   - Current model, conversation turn count, agent state (idle / thinking / executing), resolved token budget
   - Session info: session ID, save path, branch depth

9. **Test suite audit:**
   - TUI rendering tests are inherently fragile — prefer snapshot tests for rendered output over pixel-level assertions
   - Extract a `TestBackend` (ratatui's `TestBackend`) helper for rendering assertions
   - Ensure streaming tests use deterministic mock token streams (no timing-dependent assertions)
   - Audit approval flow tests for coverage: approve, deny, skip, and edge cases (tool call with missing arguments)
   - Review the full test suite across all crates — are there helpers that should be promoted to `rho-test-helpers`? Are there fixture files that are now shared across 3+ crates and should be consolidated?
   - Verify model-aware context sizing tests: mock `/v1/models` response, budget auto-sizing, explicit override wins over auto-size, warning when system prompt exceeds threshold
