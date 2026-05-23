# Phase — Session Discovery, Resume & Context Visibility — ✅ COMPLETE

**Goal:** Make the REPL less "flying blind" by adding session resumption, session listing, and context window visibility.

**Milestone:** Users can resume the most recent session with `rho -c`, list past sessions with `/sessions`, see context window pressure after every turn, and inspect detailed context breakdown with `/status`.

**Current state (pre-phase):** Sessions persist to `~/.rho/sessions/<project-hash>/` as JSONL files but there is no way to find or resume them without knowing the exact path. The `--session <path>` flag exists but requires manual path discovery. The REPL shows no token usage, context pressure, or budget information — the user has no idea how close they are to the context limit.

## What Was Done

### Task 1: Session discovery in `rho-core`

Added lightweight session discovery infrastructure to `rho-core/src/session/persist.rs`:

- **`SessionMetadata`** struct — carries `id`, `created_at`, `cwd`, `entry_count`, `mtime`, `path` without loading the full session
- **`list_sessions(cwd)`** — scans `~/.rho/sessions/<project-hash>/`, reads only header lines from JSONL files, returns `Vec<SessionMetadata>` sorted by filesystem mtime (newest first)
- **`find_latest_session(cwd)`** — convenience wrapper returning `Option<PathBuf>` for the most recent session
- **`read_session_metadata()`** — internal: parses first JSONL line as header, counts remaining non-empty lines for entry count
- Re-exported from `session.rs` and `lib.rs`
- 6 unit tests: empty dirs, mtime sorting, non-JSONL filtering, corrupt files

### Task 2: `--continue` / `-c` CLI flag

Added to `rho/src/cli.rs` and `rho/src/app.rs`:

- **`-c` / `--continue`** flag — auto-discovers and resumes the most recent session for the current project (mirrors `pi -c` convention)
- **`conflicts_with_all`** on all three of `--continue`, `--session`, `--ephemeral` — mutual exclusivity enforced at parse time by clap with clear error messages (also fixes pre-existing gap where `--session` and `--ephemeral` silently prioritized the first branch)
- **`resume_session()`** helper — extracted from `build_session`, shared by `--continue` and `--session` paths, includes stale-CWD detection
- **Startup hint** — when starting a fresh (non-resumed) session, prints `(N previous session(s) — use rho -c to resume)` if prior sessions exist

### Task 3: `/sessions` REPL command

Added to `rho/src/repl.rs`:

- Lists up to 10 recent sessions with timestamps, file sizes, and entry counts
- Marks the latest with `← latest`
- Date formatting using Howard Hinnant's civil calendar algorithm (no new dependency)
- Suggests `rho -c` to resume

### Task 4: Context window visibility

Added to `rho-core/src/session.rs` and `rho/src/repl.rs`:

- **`ContextStats`** struct — snapshot of context window usage: `context_window`, `completion_reserve`, `estimated_used`, `message_count`, `entry_count`, `path_entry_count`
- **`ContextStats::estimated_remaining()`** and **`ContextStats::utilization_percent()`** — convenience calculations
- **`Session::context_stats()`** — computes a `ContextStats` from the fitted message path using the session's calibrated estimator
- Re-exported `ContextStats` from `rho-core` public API
- 3 unit tests for edge cases (zero budget, over-budget saturation)

**REPL status bar** — printed after every agent turn:
```
[████████████░░░░░░░░░░░░] 12.3k/32k tokens (50%) │ 12.3k remaining │ 10 messages
```
Color-coded: green (<60%), yellow (60-80%), red (>80%).

**`/status` command** (aliased as `/context`) — detailed breakdown:
- Context window, completion reserve, prompt budget
- System prompt overhead, tool schema overhead, conversation tokens
- Estimated used/remaining, utilization percentage
- Fitted message count, path entries, total entries, message budget
- Model name and session file path

## Modified Files

| File | Changes |
|---|---|
| `rho-core/src/session/persist.rs` | `SessionMetadata`, `list_sessions()`, `find_latest_session()`, `read_session_metadata()`, 6 tests |
| `rho-core/src/session.rs` | `ContextStats` struct, `Session::context_stats()`, 3 tests |
| `rho-core/src/lib.rs` | Re-exports for `SessionMetadata`, `ContextStats`, discovery functions |
| `rho/src/cli.rs` | `--continue` / `-c` flag with `conflicts_with_all`, `#[allow(clippy::struct_excessive_bools)]` |
| `rho/src/app.rs` | `build_session` 4-way dispatch, `resume_session()` helper, startup hint |
| `rho/src/repl.rs` | `/sessions`, `/status`/`/context` commands, `print_context_bar()`, `show_context_stats()`, date formatting helpers |

## Exit Criteria

- [x] `rho -c` resumes the most recent session for the current project
- [x] `rho -c`, `--session`, `--ephemeral` are mutually exclusive at parse time
- [x] `/sessions` lists recent sessions with metadata
- [x] Context status bar appears after every REPL turn
- [x] `/status` shows detailed context breakdown
- [x] All new code has unit tests
- [x] CI green: fmt, clippy, 886 tests passing

## Remaining / Future Work

### Exact token counts from streaming API
The current context bar uses **estimated** token counts via `HeuristicEstimator`. These self-correct over time but are not exact. The streaming path (`StreamChunk::Done`) discards `usage` from the SSE response. To surface exact counts:
1. Extend `StreamChunk::Done` to carry `Option<ModelUsage>`
2. Parse `usage` from the final SSE chunk (some providers include it)
3. Thread usage through `send_streaming` → `run_loop` → observer or session
4. Display exact `prompt_tokens` / `completion_tokens` in the status bar when available

### Running cost tracking
Requires:
1. Exact token counts (above)
2. Per-model pricing tables (input $/M tokens, output $/M tokens)
3. Accumulation across turns within a session
4. Likely a `CostTracker` in `rho-core` that the agent loop feeds after each API call

### Model-aware context window sizing
Currently the context window is user-configured (default 32K). The agent could query the model API's `/v1/models` endpoint for `max_context_length` and auto-size the budget. Noted as a Phase 4 concern in the roadmap.

### Estimator calibration persistence
The `HeuristicEstimator` calibrates per-session but resets on restart. Persisting calibration data to `~/.rho/calibration.json` would give accurate estimates from turn 1 of a new session. Flagged in the roadmap (decision #28).
