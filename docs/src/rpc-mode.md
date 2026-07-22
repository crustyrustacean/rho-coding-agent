# RPC Mode

`rho` runs as a headless agent communicating via **JSON-RPC 2.0** over stdin/stdout. All requests must include `"jsonrpc": "2.0"`, a `method` field, optional `params`, and a numeric or string `id` for response correlation. Streaming events are delivered as JSON-RPC notifications (no `id` field).

Diagnostic output (warnings, budget info, session status) is written to **stderr**, keeping **stdout** exclusively for the protocol.

> **Machine-readable schema.** An [`OpenRPC 1.3.1` schema](../rpc-schema/openrpc.json) documents every method, its params, and result types. Use it with any `OpenRPC`-compatible code generator to produce type-safe client stubs in your language of choice.

## Usage

```sh
echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"fix the bug"},"id":1}' | \
  rho --model my-model
```

Or with an external provider:

```sh
echo '{"jsonrpc":"2.0","method":"prompt","params":{"message":"explain this function"},"id":1}' | \
  rho --endpoint https://openrouter.ai/api/v1/chat/completions \
    --api-key-env OPENROUTER_API_KEY \
    --model deepseek/deepseek-v4-flash \
    --accept-external-provider
```

## Protocol

### Methods (stdin → rho)

| Method | Params | Description |
|---|---|---|
| `prompt` | `{message: string, steer?: boolean}` | Send a user message to the agent (or a mid-turn steering nudge when `steer` is true) |
| `abort` | — | Cancel the current operation |
| `clear` | — | Clear conversation history |
| `getState` | — | Return model, provider, and cwd |
| `getMessages` | — | Return all messages on active path |
| `setModel` | `{model: string}` | Switch model (`id` or `provider:id`) |
| `listModels` | — | List available models from providers |
| `listProviders` | — | List configured providers with reachability |
| `getSessionStats` | — | Return token budget / usage info |
| `listSessions` | — | List previous sessions for project |
| `listExtensions` | — | List loaded extensions and tools |
| `reloadExtensions` | — | Reload extensions from disk |
| `compact` | — | Trigger context compaction |
| `resumeSession` | `{path: string}` | Resume a previous session from JSONL |
| `newSession` | — | Start a fresh session (preserves model, provider, tools) |
| `listTools` | — | List registered tools with schemas and risk levels |
| `approvalResponse` | `{approved: boolean, message?: string}` | Respond to an `approval/request`; when `approved` is false and `message` is set, the agent treats it as a redirect |

### Notifications (rho → stdout, no `id`)

| Method | Params | Description |
|---|---|---|
| `ready` | — | Emitted once on startup |
| `agent/start` | — | Agent began processing a prompt |
| `agent/end` | `{reply, iterations, usage, toolCalls, durationMs, finishReason}` | Agent finished with full result |
| `agent/error` | `{error: string}` | Agent loop encountered an error |
| `state/change` | `{state: string}` | Loop state transition (`thinking`, `executing_tool`, `awaiting_approval`, `idle`) |
| `message/delta` | `{delta: string}` | Streaming text chunk |
| `reasoning/delta` | `{delta: string}` | Streaming reasoning chunk |
| `tool/call` | `{name, arguments}` | Model requested a tool call |
| `tool/result` | `{name, is_error, output}` | Tool finished executing |
| `tool/denied` | `{name}` | Tool call denied by approval gate |
| `approval/request` | `{tool, arguments, risk}` | Approval required — send `approvalResponse` |
| `usage` | `{iteration, usage, context}` | Per-iteration token/cost delta and live context snapshot |

### Error codes

| Code | Meaning |
|---|---|
| `-32700` | Parse error |
| `-32600` | Invalid request |
| `-32601` | Method not found |
| `-32602` | Invalid params |
| `-32603` | Internal error |

## Approval flow

When rho emits an `approval/request` notification, it blocks until it reads an `approvalResponse` method from stdin:

```json
{"jsonrpc": "2.0", "method": "approvalResponse", "params": {"approved": true}, "id": 2}
```

Sending `approved: false` denies the tool call and lets the agent continue.

When `approved: false` and `message` is provided, the agent treats it as a **redirect**: the user's alternative instructions are injected as a new conversation turn and the model returns to thinking to re-plan. No `tool/denied` notification is emitted for a redirect — the tool batch is abandoned and the model gets another turn.


Example interaction with a destructive tool:

```json
→ {"jsonrpc":"2.0","method":"prompt","params":{"message":"delete the temp files"},"id":1}
← {"jsonrpc":"2.0","method":"agent/start"}
← {"jsonrpc":"2.0","method":"state/change","params":{"state":"thinking"}}
← {"jsonrpc":"2.0","method":"tool/call","params":{"name":"run_command","arguments":"{\"command\":\"Remove-Item temp/*\"}"}}
← {"jsonrpc":"2.0","method":"approval/request","params":{"tool":"run_command","arguments":"{\"command\":\"Remove-Item temp/*\"}","risk":"destructive"}}
→ {"jsonrpc":"2.0","method":"approvalResponse","params":{"approved":true},"id":2}
← {"jsonrpc":"2.0","method":"tool/result","params":{"name":"run_command","is_error":false,"output":""}}
← {"jsonrpc":"2.0","method":"state/change","params":{"state":"idle"}}
← {"jsonrpc":"2.0","method":"agent/end","params":{"reply":"Done — the temp files have been removed.","iterations":1,"usage":{"inputTokens":420,"outputTokens":28,"totalTokens":448,"totalCost":0.0,"requestCount":1},"toolCalls":[{"name":"run_command","arguments":"{\"command\":\"Remove-Item temp/*\"}","outcome":{"kind":"success"},"durationMs":45}],"durationMs":1200,"finishReason":"stop"}}
← {"jsonrpc":"2.0","result":{"reply":"Done..."},"id":1}
```

## Headless startup policy

In RPC mode there is no interactive terminal, so the startup phases that normally prompt the user use safe defaults instead:

| Phase | Headless behavior |
|---|---|
| Provider consent | Requires `--accept-external-provider` flag (returns an error if missing) |
| Context file trust | Already-trusted files load silently; new or changed files are auto-denied |
| Model picker | Resolves from config (`agent.model`, `agent.provider`, provider `default_model`) or CLI `--model`. Falls back to built-in default. No network calls at startup |

## Session management

RPC mode supports the same session options:

- `--ephemeral` — in-memory session, no disk I/O (recommended for scripts)
- `--continue` / `-c` — resume the most recent session
- `--session <path>` — resume a specific session file

Use `getSessionStats` to monitor context usage and `compact` to free space when the context fills up.

## Mid-turn steering

A `prompt` with `steer: true` is a **steering nudge** — it is queued and injected at the next tool-batch seam (between the last tool execution and the next LLM call), rather than starting a new turn. This lets the user provide course corrections while the agent is mid-turn without interrupting the active execution.

```json
→ {"jsonrpc":"2.0","method":"prompt","params":{"message":"also check the tests","steer":true},"id":3}
← {"jsonrpc":"2.0","result":{},"id":3}
```

The steering message is drained at the tool-batch seam and appended as a user message before the next LLM call, so the model re-plans with the user's mid-turn input. If no tool batch is in flight, the message is still queued and will be drained at the next seam.

Steering is backed by a concurrent reader architecture: a spawned reader task owns the transport and demuxes every inbound message. Time-sensitive messages (steering prompts, `approvalResponse`, `abort`) are routed inline so they arrive even while a turn is running on the main task. Ordinary prompts and all other methods are queued for serial processing.

## Testing

The RPC dispatch loop uses a concurrent reader architecture decoupled from I/O via the `Transport` trait (`rho/src/transport.rs`). `StdioTransport` (newline-delimited JSON over stdin/stdout) is the default; in-process integration tests use `ChannelTransport` (mpsc-backed, controls when each message becomes readable) and `SyncTool` (a tool that blocks until released) to inject messages deterministically mid-turn. Tests use `TestProvider` (from `rho-test-helpers`) to wrap a `MockChatClient` as a `Provider` and construct `App` directly, bypassing CLI startup. See [Testing](./development/testing.md) for details.

Run `cargo xtask schema` to update the version in the `OpenRPC` schema after a release.

### Example test session

```text
stdin:  {"jsonrpc":"2.0","method":"prompt","params":{"message":"say hello"},"id":1}
stdout: {"jsonrpc":"2.0","method":"ready"}
stdout: {"jsonrpc":"2.0","method":"agent/start"}
stdout: {"jsonrpc":"2.0","method":"state/change","params":{"state":"thinking"}}
stdout: {"jsonrpc":"2.0","method":"message/delta","params":{"delta":"hello"}}
stdout: {"jsonrpc":"2.0","method":"state/change","params":{"state":"idle"}}
stdout: {"jsonrpc":"2.0","method":"agent/end","params":{"reply":"hello","iterations":1,"usage":{"inputTokens":32,"outputTokens":5,"totalTokens":37,"totalCost":0.0,"requestCount":1},"toolCalls":[],"durationMs":800,"finishReason":"stop"}}
stdout: {"jsonrpc":"2.0","result":{"reply":"hello"},"id":1}
```

## Wire-format types

All wire-format structs live in `rho/src/rpc_wire.rs`. Every method param, result, and notification has a typed Rust struct with `Serialize` and (where applicable) `Deserialize`, so the dispatch layer catches malformed requests early and produces well-formed responses. The mdBook source for this page (`docs/src/rpc-mode.md`) duplicates the information in prose form; the single source of truth is the `OpenRPC` schema.