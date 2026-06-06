# Sessions

A **session** is rho's unit of persistent conversation state. It replaces the older flat `Conversation` type with a tree-structured model where every turn, tool call, and compaction is an entry in an append-only log.

## Why sessions?

The original `Conversation` type stored messages in a `Vec<ChatMessage>`. When the context window filled up, the sliding window evicted old messages — including, in some cases, the user's original request. The session model fixes this by:

1. **Tree structure** — branching, not deletion. `/clear` and compaction create new branches; the old tree is preserved.
2. **Typed entries** — user messages, assistant messages, tool calls, tool results, compaction summaries, and extension entries are all first-class nodes.
3. **Resolution levels** — each entry carries a resolution (`Full`, `Outlined`, `Summarized`, `Pinned`, `Compacted`, or `Attached`) that tells the context builder how to render it.
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

## Session discovery

rho can scan saved sessions without loading the full entry tree. This enables session listing and quick-resume features.

### `find_latest_session(cwd)`

Returns the path to the most recently modified JSONL file for the given project directory. Used by `rho -c` to auto-resume. Reads only the header line — does not parse entry lines.

### `list_sessions(cwd)`

Returns all saved sessions for the project, sorted by filesystem modification time (most recent first). Each result includes:

| Field | Description |
|---|---|
| `id` | Session ID (8-char hex) |
| `created_at` | Creation timestamp from the JSONL header |
| `cwd` | Working directory the session was started in |
| `entry_count` | Number of entries (lines minus header) |
| `mtime` | Filesystem modification time |
| `path` | Full path to the JSONL file |

The `/sessions` REPL command uses this to display the 10 most recent sessions, marking which one `rho -c` will resume.

### Resuming a session

Use `rho -c` to resume the most recent session for the current project:

```sh
rho -c
```

Or resume a specific session with `--session <path>`:

```sh
rho --session ~/.rho/sessions/abcdef1234567890/1777859536_4b021d5d.jsonl
```

The `-c`, `--session`, and `--ephemeral` flags are mutually exclusive (enforced at parse time by clap).

The resumed session picks up where it left off. The model, token budget, redactor, and tool set are updated from the current configuration so a session started with one model can be continued with another.

### Ephemeral mode

Use `--ephemeral` to run without any disk persistence:

```sh
rho --ephemeral
```

All conversation state lives only in memory and is lost when rho exits. Useful for one-shot commands, CI pipelines, or when you don't want session files accumulating.

### Startup hint

When rho creates a new persisted session, it checks for previous sessions for the same project. If any exist, it prints a hint:

```text
session: /home/user/.rho/sessions/abcdef12/1777859536_4b021d5d.jsonl
  (3 previous session(s) for this project — use rho -c to resume the latest)
```

## Context stats

The `ContextStats` struct provides a rich snapshot of context window usage, available through `Session::context_stats()`:

```rust
pub struct ContextStats {
    // Basic stats
    pub context_window: usize,
    pub completion_reserve: usize,
    pub estimated_used: usize,
    pub message_count: usize,
    pub entry_count: usize,
    pub path_entry_count: usize,

    // Token distribution
    pub role_tokens: RoleTokenDistribution,           // system, user, assistant, tool
    pub resolution_tokens: ResolutionTokenDistribution, // full, outlined, summarized, pinned
    pub phase_tokens: PhaseTokenDistribution,        // exploration, execution, verification, conclusion, unclassified

    // Compaction tracking
    pub compaction_tokens: usize,
    pub compacted_entry_count: usize,
}
```

Key methods:
- `utilization_percent()` — context utilization as 0–100% (based on prompt budget)
- `estimated_remaining()` — remaining tokens in the prompt budget (saturates at 0)
- `prompt_budget()` — context window minus completion reserve

The REPL prints a compact one-line status bar after every turn:

```text
[████████████░░░░░░░░] 12.3k/32k tokens (50%) │ 12.3k remaining │ 10 messages
```

Color-coded green (<60%), yellow (60–80%), red (>80%). The `/status` REPL command (aliased as `/context`) shows a detailed multi-line breakdown including token distribution by role, resolution level, compaction stats, system prompt overhead, tool schema overhead, and conversation token breakdown.

## Entry types

Each entry in the tree has a `payload` that describes what it represents:

| Payload | Description |
|---|---|
| `Message` | A chat message (system, user, assistant, or tool result) |
| `Compaction` | A phase-aware summary replacing older entries |
| `BranchSummary` | A summary created when branching |
| `CustomMessage` | Extension message (sent to model as synthetic user message) |
| `Custom` | Extension state (never sent to model) |
| `Label` | Branch checkpoint label |
| `LeafMoved` | Record of a leaf movement |
| `ModelChange` | Record of a model switch |
| `SessionInfo` | Session metadata |
| `SessionEnded` | Clean shutdown trailer |

## Resolution levels

Every entry carries a `resolution` field:

- **`Full`** — complete, verbatim content. Used for recent entries that fit within the context window.
- **`Outlined`** — structural summary: tool name, key arguments, truncated output. Consumes ~10-20% of original tokens.
- **`Summarized`** — prose summary: a one-line description of what the entry represents. Consumes ~5-10% of original tokens.
- **`Pinned`** — protected from eviction and downgrade. Rendered at its native resolution.
- **`Compacted`** — replaced by a `Compaction` summary. The original content is still in the tree but the context builder renders the summary instead.
- **`Attached`** — lightweight reference. Rendered as a brief mention rather than full content.

Entries are downgraded through `Full → Outlined → Summarized` by the selective eviction planner before turn-level eviction is needed. Pinned entries are protected from all downgrade and eviction.

The context builder ([Context Management](./context-management.md)) uses resolution levels to decide what to send to the model — full detail where it matters, summaries where it doesn't.

## Bounded tool results

Tool results can be very large (entire file contents, command output). Sessions enforce a bounded output size:

- Tool output exceeding a fraction of the prompt budget is truncated.
- The full output is preserved in `ToolResultDetails::FullOutput` for later retrieval.
- Truncation splits on the last newline boundary to avoid breaking UTF-8 sequences.

This prevents a single verbose tool call from consuming the entire context window.

## The `/clear` command

In the REPL, `/clear` branches back to the system message entry. This has the same practical effect as clearing the conversation, but the old tree is preserved on disk. You can resume the old branch later with `rho -c` or `--session`.

## Session API

The `Session` type in `rho-core` exposes:

- **Constructors**: `Session::new()` (persisted), `Session::in_memory()` (no disk I/O), `Session::open(path)` (resume from JSONL)
- **Appenders**: `append_user_message()`, `append_assistant_message()`, `append_tool_result()`, `append_tool_call()`
- **Navigation**: `path_to_root()`, `children()`, `leaf()`, `branch_to()`, `branch_with_summary()`
- **Context**: `path_messages()` (messages along the current branch), `send_current()` (send to model with context fitting), `context_stats()` (usage snapshot), `prepare_context()` (selective downgrade)
- **Compaction**: `compact_older_than()` (compact entries older than a threshold)
- **Discovery**: `find_latest_session(cwd)`, `list_sessions(cwd)` (header-only session scanning)
- **Extensions**: `write_custom_state()`, `read_custom_state()`, `write_custom_message()`, `read_custom_message()`
- **Persistence**: `save_path()`, `flush()`
