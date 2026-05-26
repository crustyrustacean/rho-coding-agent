# RPC Integration Test Suite — Design

**Project:** `~/dev/crustyrustacean/rho-coding-agent`
**Date:** 2026-05-26
**Status:** 📋 Design
**Depends on:** RPC mode (complete, `improvement-rpc-refactor`)

---

## Problem

`rho/src/rpc.rs` has 24 unit tests covering pure helpers (`state_name`, `risk_label`, `blocks_to_text`, `message_to_json`, event shapes via `write_event_to`). There are **zero tests** that exercise the actual RPC protocol loop — `run_rpc` reading JSONL commands from stdin, dispatching to handlers, driving `run_loop`, and emitting JSONL events to stdout. The core agent loop itself is well-tested via `rho-core/tests/integration_tests.rs` using `MockChatClient`, but the RPC adapter layer is a thin but critical shim that:

1. Parses JSONL from stdin into typed commands
2. Constructs `LoopParams` with `RpcObserver` and `RpcApprovalGate`
3. Drives `run_loop` and translates its callbacks to JSONL events
4. Handles the approval round-trip (emit `approval_request`, block for `approval_response`)
5. Manages session lifecycle (EOF → `session.close()`)

Any bug in this layer — a missing field, a wrong event order, a deadlock in the approval gate, a panic on malformed input — only shows up when a real client drives `rho --mode rpc`. We need deterministic, automated coverage.

---

## Design Constraints

1. **`App` has `pub(crate)` fields.** Integration tests must live inside the `rho` crate (not `rho/tests/`) to construct `App` without going through the full CLI startup.
2. **`run_rpc` hardcodes `io::stdin()` and `io::stdout()`.** Must be refactored to accept generic I/O for testability.
3. **`RpcObserver` and `RpcApprovalGate` wrap `Arc<Mutex<io::Stdout>>`.** Must be generalized to accept any `Write + Send + Sync` sink.
4. **No live model server.** Tests use `MockChatClient` from `rho-test-helpers`.
5. **No `Provider` mock exists.** Need a `TestProvider` that wraps `MockChatClient` and implements `Provider`.
6. **Existing tests must not break.** The refactor is purely additive.

---

## Approach: Abstract I/O + In-Process Testing

### Refactor 1: Generic I/O for `run_rpc`

Extract the core RPC loop into a generic function:

```rust
// rho/src/rpc.rs

/// Core RPC loop — generic over I/O for testability.
pub(crate) async fn run_rpc_on<R, W>(
    mut app: App,
    stdin: R,
    stdout: W,
) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Clone + Send + Sync + 'static,
{
    let out: Out = Arc::new(Mutex::new(stdout));
    // ... rest of current run_rpc body, reading from stdin instead of io::stdin()
}

/// Public entry point — delegates to run_rpc_on with real stdin/stdout.
pub async fn run_rpc(app: App) -> Result<()> {
    run_rpc_on(app, io::stdin().lock(), io::stdout()).await
}
```

The `Out` type changes from `Arc<Mutex<io::Stdout>>` to `Arc<Mutex<W>>` where `W: Write + Clone + Send + Sync`. Since `RpcObserver` and `RpcApprovalGate` both hold an `Out`, they become generic too, or we erase the type behind `Box<dyn Write + Send + Sync>`.

**Preferred approach:** Type-erase the writer at the `Out` level:

```rust
type Out = Arc<Mutex<Box<dyn Write + Send + Sync>>>;

fn make_out<W: Write + Send + Sync + 'static>(w: W) -> Out {
    Arc::new(Mutex::new(Box::new(w) as Box<dyn Write + Send + Sync>))
}
```

This keeps `RpcObserver` and `RpcApprovalGate` concrete (no generics) and lets `run_rpc_on` accept any `Write` impl.

### Refactor 2: Generic stdin reader

The stdin reading currently uses `tokio::task::spawn_blocking(|| io::stdin().read_line(...))`. The generic version takes ownership of the reader:

```rust
let stdin_owned = Arc::new(Mutex::new(stdin));
// In the loop:
let reader = Arc::clone(&stdin_owned);
let (buf, bytes_read) = tokio::task::spawn_blocking(move || {
    let mut guard = reader.lock().unwrap();
    let mut buf = String::new();
    let n = guard.read_line(&mut buf).unwrap_or(0);
    (buf, n)
}).await?;
```

The `RpcApprovalGate` also reads from stdin — it needs a reference to the same shared reader. Thread it through:

```rust
struct RpcApprovalGate {
    out: Out,
    stdin: Arc<Mutex<Box<dyn BufRead + Send>>>,
}
```

### Refactor 3: `TestProvider` in `rho-test-helpers`

```rust
// rho-test-helpers/src/lib.rs

pub struct TestProvider {
    name: String,
    client: MockChatClient,
    is_external: bool,
}

impl TestProvider {
    pub fn new(name: &str, client: MockChatClient) -> Self {
        Self { name: name.into(), client, is_external: false }
    }
}

#[async_trait]
impl Provider for TestProvider {
    fn name(&self) -> &str { &self.name }
    fn is_external(&self) -> bool { self.is_external }
    async fn list_models(&self) -> Result<ModelList> {
        Ok(ModelList { data: vec![] })
    }
    fn llm_service(&self) -> &dyn LlmService { &self.client }
    fn clone_boxed_service(&self) -> Box<dyn LlmService> {
        // MockChatClient is Clone-friendly (items/requests are Arc<Mutex>)
        Box::new(self.client.clone())
    }
}
```

### Refactor 4: `App` test constructor

Add a `#[cfg(test)]` method on `App` (or a free function in the test module):

```rust
#[cfg(test)]
fn test_app(client: MockChatClient, registry: ToolRegistry) -> App {
    let provider = TestProvider::new("test", client);
    let mut providers = ProviderRegistry::new();
    providers.add(Box::new(provider));

    let session = Session::in_memory(
        "mock-model",
        Some("test system prompt"),
        registry.tool_definitions(),
        "/tmp",
    );

    App {
        mode: Mode::Rpc,
        session,
        providers,
        active_provider_index: 0,
        registry,
        config: AgentConfig::default(),
        cancel: CancellationToken::new(),
    }
}
```

This lives in `rho/src/rpc.rs`'s test module (which can access `pub(crate)` fields).

---

## Test Categories

### 1. Lifecycle

| # | Test | What it proves |
|---|---|---|
| 1.1 | `ready_emitted_on_start` | First event on stdout is `{"type":"ready"}` |
| 1.2 | `clean_exit_on_eof` | stdin EOF → `session.close()` called, returns `Ok(())` |
| 1.3 | `empty_lines_skipped` | Blank lines between commands don't cause errors |

### 2. Command Dispatch

| # | Test | What it proves |
|---|---|---|
| 2.1 | `unknown_command_returns_error` | `{"type":"frob"}` → `{"type":"response","success":false,"error":"unknown command: frob"}` |
| 2.2 | `missing_type_returns_error` | `{"not_type":"x"}` → error about missing `"type"` field |
| 2.3 | `malformed_json_returns_error` | `{not json` → JSON parse error response |
| 2.4 | `abort_cancels_token` | `{"type":"abort"}` cancels the token and returns success |

### 3. Prompt Command — Text-Only Turn

| # | Test | What it proves |
|---|---|---|
| 3.1 | `prompt_text_only_event_sequence` | stdin: `prompt` → stdout: `agent_start` → `state_change(thinking)` → `message_update` deltas → `state_change(idle)` → `agent_end` with full reply |
| 3.2 | `prompt_empty_message_rejected` | `{"type":"prompt","message":""}` → error response |
| 3.3 | `prompt_missing_message_rejected` | `{"type":"prompt"}` → error response |
| 3.4 | `prompt_non_string_message_rejected` | `{"type":"prompt","message":123}` → error response |

### 4. Prompt Command — Tool Call Turn (Auto-Approved)

| # | Test | What it proves |
|---|---|---|
| 4.1 | `prompt_tool_call_approved_event_sequence` | Model requests tool → `tool_call` event → `tool_result` event → `agent_end` with reply |
| 4.2 | `prompt_multi_tool_call_sequential` | Model requests 2 tools → 2 `tool_call` + 2 `tool_result` events in order |
| 4.3 | `prompt_tool_error_reported` | Tool returns error → `tool_result` with `is_error: true` |

### 5. Approval Flow

| # | Test | What it proves |
|---|---|---|
| 5.1 | `approval_granted_flow` | Destructive tool → `approval_request` → stdin sends `approved:true` → `tool_result` → `agent_end` |
| 5.2 | `approval_denied_flow` | Destructive tool → `approval_request` → stdin sends `approved:false` → `tool_denied` → agent continues to `agent_end` |
| 5.3 | `approval_malformed_defaults_deny` | `approval_response` with no `approved` field → denied |
| 5.4 | `approval_with_reasoning_delta` | Reasoning model emits `reasoning_delta` events between `agent_start` and tool calls |

### 6. Agent Error Handling

| # | Test | What it proves |
|---|---|---|
| 6.1 | `agent_error_on_model_failure` | `MockChatClient` returns error → `agent_error` event, no panic |
| 6.2 | `agent_error_on_max_iterations` | Model loops forever → `agent_error` when max iterations exceeded |
| 6.3 | `agent_error_preserves_session` | After error, session still has the user message appended |

### 7. Query Commands

| # | Test | What it proves |
|---|---|---|
| 7.1 | `get_state_returns_model_and_provider` | `get_state` → `{"type":"response","success":true,"model":"...","provider":"..."}` |
| 7.2 | `get_messages_returns_path` | After 1 turn, `get_messages` → response with messages array containing user + assistant |
| 7.3 | `get_session_stats_returns_budget` | `get_session_stats` → response with `context_window`, `estimated_used`, etc. |
| 7.4 | `set_model_updates_session` | `set_model` → response confirms, subsequent `get_state` shows new model |
| 7.5 | `set_model_empty_rejected` | `{"type":"set_model","model":""}` → error |
| 7.6 | `compact_success` | After multi-turn, `compact` → success response |

### 8. Multi-Turn Session

| # | Test | What it proves |
|---|---|---|
| 8.1 | `two_prompts_session_persists` | Send 2 prompts, then `get_messages` shows 4 messages (2 user + 2 assistant) |
| 8.2 | `session_stats_grow` | `get_session_stats` shows increasing `estimated_used` after each turn |

### 9. Edge Cases

| # | Test | What it proves |
|---|---|---|
| 9.1 | `concurrent_abort_during_prompt` | Send `abort` while prompt is processing → cancellation propagates |
| 9.2 | `large_reply_streaming` | Model returns many deltas → all concatenated in `agent_end.reply` |
| 9.3 | `utf8_in_messages` | Non-ASCII characters in prompt and reply round-trip correctly |
| 9.4 | `special_chars_in_tool_arguments` | Tool arguments with quotes, newlines, backslashes survive JSON serialization |

### 10. JSONL Protocol Conformance

| # | Test | What it proves |
|---|---|---|
| 10.1 | `every_output_line_is_valid_json` | Parse every line from stdout as `serde_json::Value` — no panics |
| 10.2 | `every_output_line_ends_with_newline` | Binary protocol invariant |
| 10.3 | `no_interleaved_lines` | With concurrent observer + command loop writes, no partial lines appear |
| 10.4 | `event_order_invariant` | For a text-only turn: `agent_start` before any `state_change`, `agent_end` is last |

---

## Test Harness Design

### Core helper: `rpc_session`

```rust
/// Run the RPC loop with canned stdin, capture all stdout events.
///
/// Returns parsed JSON events in order. The stdin contains all commands
/// followed by EOF (empty remaining buffer) so the loop exits cleanly.
async fn rpc_session(
    client: MockChatClient,
    registry: ToolRegistry,
    stdin_lines: &[&str],
) -> Vec<Value> {
    let app = test_app(client, registry);
    let stdin = stdin_lines.join("\n");
    let reader = io::Cursor::new(stdin.into_bytes());
    let writer: Vec<u8> = Vec::new();

    run_rpc_on(app, reader, writer).await.unwrap();

    let output = /* extract from writer */;
    output.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}
```

### Approval flow helper: `rpc_session_with_midway_input`

For tests where the approval gate needs to read a response mid-stream, the stdin `Cursor` must have the `approval_response` line positioned *after* the prompt command. The test sets up stdin as:

```
{"type":"prompt","message":"do destructive thing"}
{"type":"approval_response","approved":true}
```

Since the RPC loop reads sequentially from the `BufRead`, the `approval_response` arrives at the right time.

### Assertions helper: `expect_event_sequence`

```rust
/// Assert that `events` contains the given event types in order,
/// possibly with other events interspersed.
fn expect_event_sequence(events: &[Value], expected_types: &[&str]) {
    let mut idx = 0;
    for event in events {
        if idx < expected_types.len() && event["type"] == expected_types[idx] {
            idx += 1;
        }
    }
    assert_eq!(idx, expected_types.len(),
        "expected event sequence {:?}, only matched first {idx}", expected_types);
}

/// Extract events of a given type from the output.
fn events_of_type<'a>(events: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["type"] == event_type).collect()
}
```

---

## Implementation Steps

| Step | Description | New tests |
|---|---|---|
| 1 | Refactor `Out` to `Arc<Mutex<Box<dyn Write + Send + Sync>>>` | 0 (existing tests still pass) |
| 2 | Refactor `run_rpc` → `run_rpc_on<R, W>` with generic I/O | 0 |
| 3 | Thread shared `stdin` through `RpcApprovalGate` | 0 |
| 4 | Add `rho-test-helpers` as dev-dependency of `rho` crate | 0 |
| 5 | Implement `TestProvider` in `rho-test-helpers` | provider tests |
| 6 | Implement `test_app()` in `rpc.rs` test module | 0 |
| 7 | Implement `rpc_session()` harness | 0 |
| 8 | Write lifecycle tests (category 1) | 3 |
| 9 | Write command dispatch tests (category 2) | 4 |
| 10 | Write prompt text-only tests (category 3) | 4 |
| 11 | Write prompt tool-call tests (category 4) | 3 |
| 12 | Write approval flow tests (category 5) | 4 |
| 13 | Write error handling tests (category 6) | 3 |
| 14 | Write query command tests (category 7) | 6 |
| 15 | Write multi-turn tests (category 8) | 2 |
| 16 | Write edge case tests (category 9) | 4 |
| 17 | Write JSONL conformance tests (category 10) | 4 |

**Total new tests:** ~41
**Refactor scope:** ~50 lines changed in `rpc.rs` (I/O abstraction), ~60 lines new in `rho-test-helpers` (`TestProvider`)

---

## Files Changed

```
rho/src/rpc.rs               — I/O abstraction, run_rpc_on, ~41 new integration tests
rho/Cargo.toml                — add rho-test-helpers as dev-dependency
rho-test-helpers/src/lib.rs   — add TestProvider
```

---

## What This Does NOT Cover

These are explicitly out of scope:

- **Subprocess tests** (running `rho --mode rpc` as a real binary). The design plan from pi-brain mentions this as a future addition. Our in-process tests cover the protocol logic; subprocess tests would validate arg parsing, process lifecycle, and signal handling.
- **Real model API calls.** All tests use `MockChatClient`. Live-model smoke tests remain manual.
- **REPL mode.** Unchanged.
- **TUI mode.** Not implemented yet.
- **Concurrent client connections.** RPC mode is single-client (one stdin/stdout pair). No concurrency to test.
- **`App::build` startup sequence in RPC mode.** Tested by the existing headless tests in `context_files.rs` and the CLI tests. Our integration tests construct `App` directly.
