# RPC Mode

rho supports a headless **RPC mode** for integration with editors, bots, custom UIs, and scripts. Instead of an interactive REPL, rho reads newline-delimited JSON commands from stdin and writes newline-delimited JSON events to stdout.

## Usage

```sh
echo '{"type":"prompt","message":"fix the bug"}' | \
  rho --mode rpc \
    --ephemeral \
    --endpoint http://localhost:1234/v1/chat/completions \
    --model my-model
```

Or with an external provider:

```sh
echo '{"type":"prompt","message":"explain this function"}' | \
  rho --mode rpc \
    --ephemeral \
    --endpoint https://openrouter.ai/api/v1/chat/completions \
    --api-key-env OPENROUTER_API_KEY \
    --model deepseek/deepseek-v4-flash \
    --accept-external-provider
```

## Protocol

Every outbound line is a compact JSON object followed by `\n`. Every inbound command must be a JSON object with at least a `"type"` field.

### Commands (stdin → rho)

| `type` | Required fields | Description |
|---|---|---|
| `prompt` | `message` | Send a user message to the agent |
| `abort` | — | Cancel the current operation |
| `get_state` | — | Return current model and provider name |
| `get_messages` | — | Return all messages on the active session path |
| `set_model` | `model` | Switch the active model |
| `get_session_stats` | — | Return token budget and context usage |
| `compact` | — | Trigger context compaction |

### Events (rho → stdout)

| `type` | Key fields | Description |
|---|---|---|
| `ready` | — | Emitted once on startup |
| `agent_start` | — | Agent began processing a prompt |
| `agent_end` | `reply` | Agent finished; full text reply |
| `agent_error` | `error` | Agent loop encountered an error |
| `state_change` | `state` | Loop state transition (`thinking`, `executing_tool`, `awaiting_approval`, `idle`) |
| `message_update` | `delta` | Streaming text chunk |
| `reasoning_delta` | `delta` | Streaming reasoning / chain-of-thought chunk |
| `tool_call` | `name`, `arguments` | Model requested a tool call |
| `tool_result` | `name`, `is_error`, `output` | Tool finished executing |
| `tool_denied` | `name` | Tool call denied by approval gate |
| `approval_request` | `tool`, `arguments`, `risk` | Approval required — respond with `approval_response` |
| `response` | `success`, [`error`] | Command acknowledgment (for non-prompt commands) |

## Approval flow

When rho emits an `approval_request` event, it blocks until it reads an `approval_response` from stdin:

```json
{"type": "approval_response", "approved": true}
```

Send `"approved": false` (or any non-boolean / missing field) to deny the tool call. The agent continues after denial — the model sees the denial reason and can adapt.

Example interaction with a destructive tool:

```json
→ {"type":"prompt","message":"delete the temp files"}
← {"type":"agent_start"}
← {"type":"state_change","state":"thinking"}
← {"type":"tool_call","name":"run_command","arguments":"{\"command\":\"Remove-Item temp/*\"}"}
← {"type":"approval_request","tool":"run_command","arguments":"{\"command\":\"Remove-Item temp/*\"}","risk":"destructive"}
→ {"type":"approval_response","approved":true}
← {"type":"tool_result","name":"run_command","is_error":false,"output":""}
← {"type":"state_change","state":"idle"}
← {"type":"agent_end","reply":"Done — the temp files have been removed."}
```

## Headless startup policy

In RPC mode there is no interactive terminal, so the startup phases that normally prompt the user use safe defaults instead:

| Phase | Headless behavior |
|---|---|
| Provider consent | Requires `--accept-external-provider` flag (returns an error if missing) |
| Context file trust | Already-trusted files load silently; new or changed files are auto-denied |
| Model picker | Requires `--model` flag (returns an error instead of interactive selection) |

## Session management

RPC mode supports the same session options as REPL mode:

- `--ephemeral` — in-memory session, no disk I/O (recommended for scripts)
- `--continue` / `-c` — resume the most recent session
- `--session <path>` — resume a specific session file

Use `get_session_stats` to monitor context usage and `compact` to free space when the context fills up.

## Testing

The RPC core loop is generic over I/O (`run_rpc_on<R, W>`), enabling 43 in-process integration tests that inject canned stdin via `Cursor<Vec<u8>>` and capture stdout without touching real file descriptors. Tests use `TestProvider` (from `rho-test-helpers`) to wrap a `MockChatClient` as a `Provider` and construct `App` directly, bypassing CLI startup. See [Testing](./development/testing.md) for details.

### Example test session

```text
stdin:  {"type":"prompt","message":"say hello"}
stdout: {"type":"ready"}
stdout: {"type":"agent_start"}
stdout: {"type":"state_change","state":"thinking"}
stdout: {"type":"message_update","delta":"hello"}
stdout: {"type":"state_change","state":"idle"}
stdout: {"type":"agent_end","reply":"hello"}
```
