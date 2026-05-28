# Phase 4 Readiness Assessment

**Date:** 2026-05-16 | **Version:** 0.36.6 | **Tests:** 560+ passing

## Executive Summary

**Ready to start Phase 4 TUI development.**

The architecture was designed for this — `AgentState`, `ApprovalGate`, `ToolOutcome::Streamed`, `Session` tree navigation, and `rho-highlight` were all built with Phase 4 in mind. The foundation is solid. **Pre-Work 1 (streaming API) is complete.** **Pre-Work 2 (modularize `rust.rs`) is complete.** External provider support (OpenRouter) has been validated with `rho-bench`.

---

## ✅ What's Ready

### Architecture Layering — Strong

`rho-core` is clean — no UI, no shell, no filesystem knowledge. The `AgentState` enum (`Idle` / `Thinking` / `AwaitingApproval` / `ExecutingTool`) is exactly what a TUI needs to render state. The `ApprovalGate` trait separates *whether* to approve (core) from *how* to ask (UI). The `ToolOutcome::Streamed` variant is already declared, ready for the streaming implementation.

### Session Model — Ready

Tree-shaped, JSONL-persisted, resolution-aware. The slash commands map directly to `Session` APIs:

| Command | Session API |
|---|---|
| `/clear` | `Session::branch_to(&root_id)` |
| `/history` | `Session::path_messages()` |
| `/fork` | `Session::branch_with_summary()` |
| `/tree` | Session tree traversal |
| `/resume` | Session file listing and reload |

### Syntax Highlighting — Ready

`rho-highlight` produces classified spans for Rust. The `Language` enum has reserved slots for PowerShell, TOML, JSON, and Markdown. Phase 4 evaluates each grammar's maturity before committing; feature-gating means individual grammars can be deferred without blocking the TUI.

### Diagnostics — Ready

`ToolResultDetails::Diagnostics(Vec<Diagnostic>)` gives the TUI structured error data with file/line/column/suggestions. No text scraping needed. The diagnostic panel can render directly from the type.

### Test Infrastructure — Solid

708 tests across all crates. `MockChatClient`, `MockShellExecutor`, `FixedResponseTool`, `assert_no_orphan_tool_results`, `FileTestEnv` — the harness is mature. The test suite audit (Task 9) can build on this foundation.

### Scenarios — Validated

All 5 prompt scenarios pass end-to-end against `qwen/qwen3.6-27b` and `google/gemma-4-26b-a4b-it` (4/5 via OpenRouter), proving the agent loop + Rust tooling + approval flow work correctly with both local and remote providers. The scenarios exercise the exact code paths the TUI will render.

---

## ⚠️ Concerns

### Streaming — ✅ Complete (Pre-Work 1 + OpenRouter Validation)

The streaming API is fully implemented and merged to trunk:

- `chat_stream` on `ChatClient` trait returns `Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>`
- Default impl wraps `chat` — no breakage for `MockChatClient` or other providers
- `LocalChatClient` implements real SSE parsing with line buffering and `[DONE]` detection
- `run_loop` always uses the streaming path; non-streaming providers work via the default wrapper
- `StreamChunk` enum: `TextDelta`, `ReasoningDelta`, `ToolCallDelta`, `Done`
- `StreamChunk::from_response()` converts a full `ModelResponse` into chunks for the default impl
- `accumulate()` reconstructs an `AssistantResponse` from a stream of chunks
- SSE parser fixed for external providers: tool-call deltas checked first, empty content strings skipped
- `rho-bench` `CountingClient` delegates `chat_stream()` to inner client (not default wrapper)
- `ChatStream` re-exported from `rho-core`
- All 560+ tests pass, `cargo xtask ci` green
- Validated against 4 models via OpenRouter: DeepSeek v4 Flash (4/5), GLM 5.1 (4/5), Gemini 2.0 Flash (3/5), Gemma 4 26B (4/5)

### `rust.rs` Modularization — ✅ Complete (Pre-Work 2)

`rho-tools/src/rust.rs` (2,134 lines) has been split into focused submodules:

```
rho-tools/src/rust/
├── mod.rs              (re-exports, public API)
├── tools.rs            (CargoCheck, CargoClippy, CargoTest, CargoFix, RustcExplain)
├── parse.rs            (NDJSON parsing, message extraction, filtering)
├── format.rs           (diagnostic formatting, AST context, suggestions)
├── convert.rs          (conversion from raw JSON to core diagnostic types)
└── types.rs            (raw cargo JSON types for deserialization)
```

All clippy lints fixed, all tests pass.

### Remaining Test Cleanup — 🟢 Low Severity

6 open items (#3–#7) in the test suite review, totaling ~130 lines of duplication. None are blockers. The test suite is healthy at 708 tests. Can be done in parallel with Phase 4 or as pre-work.

### Model-Aware Context Sizing — 🟢 Low Severity

Task 7 requires querying `/v1/models` for `max_context_length`. The endpoint already exists and `ModelInfo` is already parsed in `LocalChatClient`. Straightforward wiring — not a design risk.

### Additional Tree-Sitter Grammars — 🟢 Low Severity

PowerShell, TOML, JSON, Markdown grammars are "evaluated" — may not be mature enough. The `rho-highlight` architecture supports feature-gating. Individual grammars can be deferred without blocking the TUI.

---

## 🔴 Pre-Work Items

### Pre-Work 1: Design the Streaming API — ✅ Complete

Implemented on `improvement-chat-streaming` branch. See streaming section above for details.

### Pre-Work 2: Modularize `rust.rs` — ✅ Complete

Split `rho-tools/src/rust.rs` (2,134 lines) into focused submodules (mod.rs, tools.rs, parse.rs, format.rs, convert.rs, types.rs). All clippy lints fixed. All tests pass.

---

## Phase 4 Task Path

After pre-work, the recommended task order:

| Order | Task | Rationale |
|---|---|---|
| 1 | Create `rho-tui` crate | Scaffolding — must exist before anything else |
| 2 | Status bar | Simplest rendering surface; validates crossterm + ratatui setup |
| 3 | Input pane | The REPL replacement — multi-line editor, history, slash commands |
| 4 | Output pane | Markdown rendering, syntax highlighting, diff views |
| 5 | Tool call display | Approval prompts with rich previews — the core UX of the TUI |
| 6 | Streaming output | Requires streaming API from Pre-Work 1; the big integration task |
| 7 | Diagnostic panel | Structured rendering of `Diagnostic` objects |
| 8 | Model-aware context sizing | Query model metadata, auto-size budget, display in status bar |
| 9 | Test suite audit | Final pass — snapshot tests, streaming mocks, coverage audit |

This ordering ensures each task builds on the previous one, with streaming (the hardest part) landing after the basic TUI is functional.

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Streaming API design delays | Medium | High | Pre-work item; design before coding |
| ratatui learning curve | Low | Medium | ratatui has good docs and examples; wrap behind `View` trait early |
| PowerShell tree-sitter grammar immature | Medium | Low | Feature-gate; fall back to regex highlighting |
| TUI rendering tests fragile | High | Low | Use ratatui `TestBackend` for snapshot tests; avoid timing-dependent assertions |
| Context auto-sizing wrong for some models | Low | Medium | Explicit override always wins; log warning when auto-sized |

---

**Last updated:** 2026-05-16
