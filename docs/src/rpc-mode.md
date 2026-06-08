# RPC Mode

`rho` runs as a headless agent communicating via **JSON-RPC 2.0** over stdin/stdout. All requests must include `"jsonrpc": "2.0"`, a `method` field, optional `params`, and a numeric or string `id` for response correlation. Streaming events are delivered as JSON-RPC notifications (no `id` field).

Diagnostic output (warnings, budget info, session status) is written to **stderr**, keeping **stdout** exclusively for the protocol.

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
| `prompt` | `{message: string}` | Send a user message to the agent |
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
| `approvalResponse` | `{approved: boolean}` | Respond to an `approval/request` notification |

### Notifications (rho → stdout, no `id`)

| Method | Params | Description |
|---|---|---|
| `ready` | — | Emitted once on startup |
| `agent/start` | — | Agent began processing a prompt |
| `agent/end` | `{reply: string}` | Agent finished; full text reply |
| `agent/error` | `{error: string}` | Agent loop encountered an error |
| `state/change` | `{state: string}` | Loop state transition (`thinking`, `executing_tool`, `awaiting_approval`, `idle`) |
| `message/delta` | `{delta: string}` | Streaming text chunk |
| `reasoning/delta` | `{delta: string}` | Streaming reasoning chunk |
| `tool/call` | `{name, arguments}` | Model requested a tool call |
| `tool/result` | `{name, is_error, output}` | Tool finished executing |
| `tool/denied` | `{name}` | Tool call denied by approval gate |
| `approval/request` | `{tool, arguments, risk}` | Approval required — send `approvalResponse` |

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
← {"jsonrpc":"2.0","method":"agent/end","params":{"reply":"Done — the temp files have been removed."}}
← {"jsonrpc":"2.0","result":{"reply":"Done..."},"id":1}
```

## Headless startup policy

In RPC mode there is no interactive terminal, so the startup phases that normally prompt the user use safe defaults instead:

| Phase | Headless behavior |
|---|---|
| Provider consent | Requires `--accept-external-provider` flag (returns an error if missing) |
| Context file trust | Already-trusted files load silently; new or changed files are auto-denied |
| Model picker | Requires `--model` flag (returns an error instead of interactive selection) |

## Session management

RPC mode supports the same session options:

- `--ephemeral` — in-memory session, no disk I/O (recommended for scripts)
- `--continue` / `-c` — resume the most recent session
- `--session <path>` — resume a specific session file

Use `getSessionStats` to monitor context usage and `compact` to free space when the context fills up.

## Testing

The RPC core loop is generic over I/O (`run_rpc_on<R, W>`), enabling in-process integration tests that inject canned stdin via `Cursor<Vec<u8>>` and capture stdout without touching real file descriptors. Tests use `TestProvider` (from `rho-test-helpers`) to wrap a `MockChatClient` as a `Provider` and construct `App` directly, bypassing CLI startup. See [Testing](./development/testing.md) for details.

### Example test session

```text
stdin:  {"jsonrpc":"2.0","method":"prompt","params":{"message":"say hello"},"id":1}
stdout: {"jsonrpc":"2.0","method":"ready"}
stdout: {"jsonrpc":"2.0","method":"agent/start"}
stdout: {"jsonrpc":"2.0","method":"state/change","params":{"state":"thinking"}}
stdout: {"jsonrpc":"2.0","method":"message/delta","params":{"delta":"hello"}}
stdout: {"jsonrpc":"2.0","method":"state/change","params":{"state":"idle"}}
stdout: {"jsonrpc":"2.0","method":"agent/end","params":{"reply":"hello"}}
stdout: {"jsonrpc":"2.0","result":{"reply":"hello"},"id":1}
```
