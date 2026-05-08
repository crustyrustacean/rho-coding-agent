# Phase 4 Readiness Assessment

**Date:** 2026-05-07 | **Version:** 0.28.0 | **Tests:** 708 passing

## Executive Summary

**Ready to start Phase 4, with two pre-work items.**

The architecture was designed for this — `AgentState`, `ApprovalGate`, `ToolOutcome::Streamed`, `Session` tree navigation, and `rho-highlight` were all built with Phase 4 in mind. The foundation is solid. Two targeted pre-work tasks will clear the path for clean TUI development.

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

All 5 prompt scenarios pass end-to-end against `qwen/qwen3.6-27b`, proving the agent loop + Rust tooling + approval flow work correctly. The scenarios exercise the exact code paths the TUI will render.

---

## ⚠️ Concerns

### Streaming Not Implemented — 🟡 Medium Severity

`ChatClient::chat` is synchronous (full request → full response). Phase 4 Task 5 requires SSE/streaming API support. This is a significant API addition:

- New `chat_stream` method on `ChatClient` trait
- Token-by-token rendering with backpressure
- The `ToolOutcome::Streamed` variant exists but `chat_stream` does not

**This is the hardest task in Phase 4.** It's a trait-breaking change that affects `MockChatClient`, `LocalChatClient`, and every test that constructs a `ChatClient`. Must be designed first, before any TUI code.

### `rust.rs` Size — 🟡 Medium Severity

2,134 lines, 8 responsibilities, debtmap score 309.9 (CRITICAL). Modularizing before adding TUI code reduces cognitive load when working in `rho-tools` alongside the new `rho-tui` crate. Low-risk refactor (1–2 days), all tests stay green.

### Remaining Test Cleanup — 🟢 Low Severity

6 open items (#3–#7) in the test suite review, totaling ~130 lines of duplication. None are blockers. The test suite is healthy at 708 tests. Can be done in parallel with Phase 4 or as pre-work.

### Model-Aware Context Sizing — 🟢 Low Severity

Task 7 requires querying `/v1/models` for `max_context_length`. The endpoint already exists and `ModelInfo` is already parsed in `LocalChatClient`. Straightforward wiring — not a design risk.

### Additional Tree-Sitter Grammars — 🟢 Low Severity

PowerShell, TOML, JSON, Markdown grammars are "evaluated" — may not be mature enough. The `rho-highlight` architecture supports feature-gating. Individual grammars can be deferred without blocking the TUI.

---

## 🔴 Pre-Work Items

These two tasks should be completed before starting Phase 4 TUI development:

### Pre-Work 1: Design the Streaming API (1 day)

Add `chat_stream` to `ChatClient` with a default impl that wraps `chat`, so existing providers and tests don't break. This is the most important design decision of Phase 4.

**Design sketch:**

```rust
#[async_trait]
pub trait ChatClient: Send + Sync {
    /// Non-streaming: send request, get full response.
    async fn chat(&self, request: ChatRequest) -> Result<ModelResponse>;

    /// Streaming: send request, get tokens as they arrive.
    /// Default implementation wraps `chat` for backward compatibility.
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>> {
        // Default: fall back to non-streaming, emit entire response as one chunk
        let response = self.chat(request).await?;
        let stream = futures::stream::once(async move { Ok(StreamChunk::from(response)) });
        Ok(Box::pin(stream))
    }
}
```

**Impact analysis:**
- `MockChatClient` — implement `chat_stream` with configurable chunk timing for deterministic tests
- `LocalChatClient` — implement real SSE parsing against OpenAI-compatible `/v1/chat/completions` with `stream: true`
- `run_loop` — add a streaming branch that renders tokens incrementally, falling back to the current non-streaming path
- All existing tests continue to use `chat` — no breakage

### Pre-Work 2: Modularize `rust.rs` (1–2 days)

Split `rho-tools/src/rust.rs` (2,134 lines) into focused submodules:

```
rho-tools/src/
├── lib.rs                  (unchanged)
├── files.rs                (unchanged)
├── shell.rs                (unchanged)
├── rust/
│   ├── mod.rs              (re-exports, public API)
│   ├── tools.rs            (CargoCheck, CargoClippy, CargoTest, CargoFix, RustcExplain)
│   ├── parse.rs            (NDJSON parsing, message extraction, filtering)
│   ├── format.rs           (diagnostic formatting, AST context, suggestions)
│   └── validate.rs         (node-splitting checks, path filtering, error codes)
```

**Expected outcome:**
- 5 modules of ~400–500 LOC each
- Debt score for rust tooling drops from 309.9 → ~150
- All existing tests pass unchanged
- Improved readability for Phase 4 TUI integration

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

**Last updated:** 2026-05-07
