# Sessions

A **session** is rho's unit of persistent conversation state. It replaces the older flat `Conversation` type with a tree-structured model where every turn, tool call, and compaction is an entry in an append-only log.

## Why sessions?

The original `Conversation` type stored messages in a `Vec<ChatMessage>`. When the context window filled up, the sliding window evicted old messages — including, in some cases, the user's original request. The session model fixes this by:

1. **Tree structure** — branching, not deletion. `/clear` and compaction create new branches; the old tree is preserved.
2. **Typed entries** — user messages, assistant messages, tool calls, tool results, compaction summaries, and extension entries are all first-class nodes.
3. **Resolution levels** — each entry carries a resolution (`Full`, `Compacted`, or `Attached`) that tells the context builder how to render it.
4. **Persistence** — sessions auto-flush to JSONL files, surviving process restarts and crashes.

## Tree structure

A session is a tree of `Entry` nodes, each identified by an `EntryId` (a UUID). Every entry has a `parent_id` pointing to its predecessor, forming a single chain (the *trunk*) with possible branches.

```text
[System] ──→ [User: "fix the bug"] ──→ [Assistant: "ok"] ──→ [User: "read another file"]
                                         │
                                         └──→ [Compaction: "user asked to fix the bug"]
```

The **leaf** is the entry the agent loop is currently building from. `branch_to(id)` moves the leaf to any existing entry, creating a divergence point. The old branch stays in the tree for later inspection or resumption.

## Persistence

Sessions are stored as append-only JSONL files under `~/.rho/sessions/`:

```text
~/.rho/sessions/
  └── <project-hash>/
        └── <unix-timestamp>_<session-id>.jsonl
```

- **Project hash**: first 16 hex characters of SHA-256 of the canonical project root path. This groups sessions by project.
- **Auto-flush**: every append writes to disk immediately. A crash loses at most the last entry.
- **Format**: each line is a JSON object with a `type` discriminator (`Header` or `Entry`). The first line is always a `Header` with session metadata.

Example JSONL file:

```json
{"type":"Header","id":"4b021d5d","version":1,"created_at_secs":1777859536,"cwd":"/home/user/project","parent_session":null}
{"type":"Entry","id":"a1b2c3d4","parent_id":null,"timestamp":{...},"resolution":"Full","payload":{"Message":{"content":"you are a coding assistant","role":"system"}}}
{"type":"Entry","id":"e5f6a7b8","parent_id":"a1b2c3d4","timestamp":{...},"resolution":"Full","payload":{"Message":{"content":"fix the bug","role":"user"}}}
```

### Resuming a session

Use `--session <path>` to load a previously saved session:

```sh
rho --session ~/.rho/sessions/abcdef1234567890/1777859536_4b021d5d.jsonl
```

The resumed session picks up where it left off. The model, token budget, redactor, and tool set are updated from the current configuration so a session started with one model can be continued with another.

### Ephemeral mode

Use `--ephemeral` to run without any disk persistence:

```sh
rho --ephemeral
```

All conversation state lives only in memory and is lost when rho exits. Useful for one-shot commands, CI pipelines, or when you don't want session files accumulating.

## Entry types

Each entry in the tree has a `payload` that describes what it represents:

| Payload | Description |
|---|---|
| `Message` | A chat message (system, user, or assistant) |
| `ToolCall` | A tool invocation requested by the model |
| `ToolResult` | The output of a tool execution |
| `Compaction` | A summary replacing older entries |
| `Custom` | Extension data (see [Extensions](../extensions.md)) |

## Resolution levels

Every entry carries a `resolution` field:

- **`Full`** — complete, verbatim content. Used for recent entries that fit within the context window.
- **`Compacted`** — replaced by a `Compaction` summary. The original content is still in the tree but the context builder renders the summary instead.
- **`Attached`** — lightweight reference (e.g., a tool call whose result has been compacted). Rendered as a brief mention rather than full content.

The context builder ([Context Management](./context-management.md)) uses resolution levels to decide what to send to the model — full detail where it matters, summaries where it doesn't.

## Bounded tool results

Tool results can be very large (entire file contents, command output). Sessions enforce a bounded output size:

- Tool output exceeding 50% of the prompt budget is truncated.
- The full output is preserved in `ToolResultDetails::FullOutput` for later retrieval.
- Truncation splits on the last newline boundary to avoid breaking UTF-8 sequences.

This prevents a single verbose tool call from consuming the entire context window.

## The `/clear` command

In the REPL, `/clear` branches back to the system message entry. This has the same practical effect as clearing the conversation, but the old tree is preserved on disk. You can resume the old branch later with `--session`.

## Session API

The `Session` type in `rho-core` exposes:

- **Constructors**: `Session::new()` (persisted), `Session::in_memory()` (no disk I/O), `Session::open(path)` (resume from JSONL)
- **Appenders**: `append_user_message()`, `append_assistant_message()`, `append_tool_result()`, `append_tool_call()`
- **Navigation**: `path_to_root()`, `children()`, `leaf()`, `branch_to()`, `branch_with_summary()`
- **Context**: `path_messages()` (messages along the current branch), `send_current(client)` (send to model with context fitting)
- **Compaction**: `compact_older_than()` (compact entries older than a threshold)
- **Extensions**: `write_custom_state()`, `read_custom_state()`, `write_custom_message()`, `read_custom_message()`
- **Persistence**: `save_path()`, `flush()`
