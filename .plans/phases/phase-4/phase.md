# Phase 4: Terminal UI

**Goal:** Replace the bare REPL with a rich, interactive terminal experience.

**Milestone:** The agent displays in a split-pane TUI with syntax-highlighted output, tool call previews, and approval prompts.

**Current state (pre-Phase 4):** The agent loop operates on `Session` (tree-shaped, JSONL-persisted). `--session` and `--ephemeral` CLI flags exist for session management. The token budget is calibrated per-model via `HeuristicEstimator`. Tool results are bounded at append time. The `AgentState` state machine exposes `Thinking`, `AwaitingApproval`, `ExecutingTool`, `Idle` for UI rendering. Streaming is not yet implemented (`ChatClient::chat` is non-streaming).

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| `crossterm` **(foundation)** | `rho-tui` | Cross-platform terminal control — writing raw Win32 + VT100 escape sequences ourselves is not feasible |
| `ratatui` **(wrapped)** | `rho-tui` | Terminal UI framework — wraps crossterm, provides layout, rendering, and event handling. Deeply specialized, thousands of edge cases. We wrap it behind our own `View` trait so the agent loop and tools are not coupled to ratatui's API |
| `pulldown-cmark` **(wrapped)** | `rho-tui` | Markdown parser — CommonMark + GFM is a large spec with many edge cases. We wrap it behind our own `MarkdownRenderer` trait |
| `tree-sitter-powershell` **(evaluated)** | `rho-highlight` | PowerShell grammar for syntax highlighting — defer to Phase 4. Only add if the grammar is mature; otherwise, fall back to regex-based highlighting for PowerShell |
| `tree-sitter-toml` **(evaluated)** | `rho-highlight` | TOML grammar — same evaluation as PowerShell |
| `tree-sitter-json` **(evaluated)** | `rho-highlight` | JSON grammar — may not be worth it; JSON is simple enough for regex highlighting |
| `tree-sitter-markdown` **(evaluated)** | `rho-highlight` | Markdown grammar — complex grammar, worth it if we want accurate inline code block detection |

## Decisions

**Markdown rendering:** Use `pulldown-cmark` **(wrapped)** wrapped behind our own `MarkdownRenderer` trait. Full CommonMark + GFM is a large spec with many edge cases (nested lists, link parsing, escaped characters) — a ~300-line custom parser will likely hit 600–800 lines before it handles them reliably. `pulldown-cmark` is well-maintained and handles the full spec. We wrap it behind our own trait so the agent loop and tools are not coupled to its API. If it proves insufficient, the trait boundary makes swapping trivial.

**Diff rendering:** We write our own unified diff generator. We have the old text and the new text — computing line-level diffs is ~150 lines using a simple LCS algorithm. If we need word-level diffs later, consider `similar`.

## Exit Criteria

The agent is usable as a daily terminal tool. The REPL feels responsive, informative, and safe (approval on destructive actions). The token budget auto-sizes to the loaded model's context window.
