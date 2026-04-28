# Phase 4 Tasks

1. **Create the `rho-tui` crate** with `ratatui` + `crossterm`.

2. **Implement the input pane:**
   - Multi-line editor (Shift+Enter for newline, Enter to submit)
   - Command history (up/down arrows)
   - Autocomplete for tool names and file paths
   - Slash-command parsing and dispatch (invokes `rho-core` APIs from the command surface table: `/clear`, `/history`, `/system`, `/model`, `/provider`, `/tools`, `/context`, `/config`, `/help`, `/quit`)
   - Autocomplete for slash commands

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
   - Switch from `POST and wait for full response` to SSE/streaming API
   - Render tokens as they arrive
   - Show "thinking" indicator while waiting

6. **Implement a diagnostic panel:**
   - Render structured `Diagnostic` objects with file/line context
   - Color-code severity (error = red, warning = yellow)
   - Show suggested fix inline

7. **Status bar:**
   - Current model, conversation turn count, agent state (idle / thinking / executing)

8. **Test suite audit:**
   - TUI rendering tests are inherently fragile — prefer snapshot tests for rendered output over pixel-level assertions
   - Extract a `TestBackend` (ratatui's `TestBackend`) helper for rendering assertions
   - Ensure streaming tests use deterministic mock token streams (no timing-dependent assertions)
   - Audit approval flow tests for coverage: approve, deny, skip, and edge cases (tool call with missing arguments)
   - Review the full test suite across all crates — are there helpers that should be promoted to `rho-test-helpers`? Are there fixture files that are now shared across 3+ crates and should be consolidated?
