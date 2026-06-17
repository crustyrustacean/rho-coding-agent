## [0.81.3] - 2026-06-17

### 🚀 Features

- **(ai)** Built-in model catalog with 306 models generated from `OpenRouter`'s public API via `cargo xtask generate-models`, with manual thinking/reasoning overrides in `rho-ai/model-overrides.json`. The catalog exposes `find()`, `by_provider()`, `search()`, and user-model overrides merged on top of built-ins.
- Model resolution enriched with catalog data: context window, max output tokens, cost, and thinking support are derived from the catalog instead of relying solely on user configuration. `completion_reserve` is capped at the model's `max_tokens`, and `reasoning_effort` is auto-enabled only for thinking-capable models (suppressed for non-thinking models to avoid invalid params).
- Default model selection ensures rho never errors with 'no model configured'. Resolution priority: `--model` CLI flag → `agent.model` config → `agent.provider` default_model → first provider's default_model → `RHO_MODEL` env → built-in default (`anthropic/claude-sonnet-4`). Startup presenter feedback (`model_catalog_info`) shows catalog enrichment.

### 🐛 Bug Fixes

- Route the built-in default model to a synthesized `OpenRouter` provider. The default (`anthropic/claude-sonnet-4`) is an `OpenRouter` model id, but the zero-config provider defaults to `localhost:1234`, so the first request failed with 'retry budget exhausted' after 4 attempts. When the built-in default is selected, app startup now registers an `OpenRouter` provider from the `openrouter` preset (picking up `OPENROUTER_API_KEY`); the external-provider consent gate still applies (`rho-code` passes `--accept-external-provider`; bare `rho` gets a clear consent error instead of a silent runtime failure).
- **(generator)** Fix doubled `//!` doc-comment markers in `catalog_generated.rs` caused by a Rust string-continuation bug in the `generate-models` xtask.

### 🚜 Refactor

- `resolve_model` no longer returns `Result` — it always succeeds after the built-in default fallback was added. Returns a `ResolvedModel` struct carrying catalog enrichment and an `is_builtin_default` flag used to synthesize the matching provider.
- Expose `rho_core::config::preset_endpoint` / `preset_api_key_env` to avoid duplicating the `OpenRouter` endpoint string when synthesizing the default provider.

### 🏗️ Internal

- Clean up pedantic clippy lints across the workspace (the generated catalog alone tripped 1594+ lints, leaving trunk red): add `#![allow(...)]` for generated-code lints in `catalog_generated.rs`, switch the generator's emit block to `writeln!`, fix redundant closures (`serde_json::Value::as_*`), `map_or_else`, `usize::try_from` for cast-truncation safety, add `Default for Catalog`, fill in missing field docs, and backtick `OpenRouter`/`LM Studio` in doc comments.

## [0.81.2] - 2026-06-15

### 📝 Documentation

- Remove context-pressure nudge feature from all documentation — the feature was deprecated in 0.58.0 and the config fields (`context_pressure_threshold`, `context_pressure_interval`) have been fully removed from the codebase
- Update `rho-ai` section in ARCHITECTURE.md with complete crate description
- Fix stray section header in ARCHITECTURE.md extensions type table
- Remove `context_pressure_threshold`/`context_pressure_interval` examples from configuration docs


### 🚀 Features

- **(rpc)** `agent/end` notification now includes full `AgentResult` fields: `iterations`, `usage` (token counts + cost), `toolCalls` (name, arguments, outcome, duration per call), `durationMs`, and `finishReason`. Frontends can display cost, timing, and tool history without a separate `getSessionStats` call.

### 📝 Documentation

- **(docs)** Updated all docs to reflect enriched `agent/end` notification: `ARCHITECTURE.md` (added missing `resumeSession`/`listTools` methods, updated notification table), `rpc-mode.md` (example sessions), `workspace-layout.md`, `crate-responsibilities.md`, `README.md`.

### 🏗️ Internal

- `TurnResult::Reply(String)` → `TurnResult::Done(Box<AgentResult>)` to carry the full result through the RPC layer.
- Added `Default` derive to `ContextStats`.
- Added wire-format types: `TokenUsageWire`, `ToolCallOutcomeWire`, `ToolCallRecordWire` with `From` impls from kernel types.

## [0.80.0] - 2026-06-15

### 🚀 Features

- **(rpc)** Typed wire-format layer (`rho/src/rpc_wire.rs`) — every JSON-RPC method param, result, and notification now has a typed Rust struct with `Serialize`/`Deserialize`. Dispatch deserializes inbound params early and serializes outbound results through structs, replacing hand-built `json!({...})` calls.
- **(rpc)** `OpenRPC 1.3.1` schema shipped at `docs/rpc-schema/openrpc.json`, documenting all 16 methods with param and result types for machine-readable client discovery.
- **(xtask)** New `cargo xtask schema` command injects the workspace version into the `OpenRPC` schema.

### 📝 Documentation

- Updated `README.md`, `ARCHITECTURE.md`, `docs/src/rpc-mode.md`, `crate-responsibilities.md`, and `workspace-layout.md` to reference the typed wire layer, `OpenRPC` schema, and `cargo xtask schema`.

## [0.79.0] - 2026-06-15

### 🚀 Features

- **(core)** `run_loop` now returns `Result<AgentResult>` instead of `Result<String>`, providing structured output with reply text, iteration count, token usage (`TokenUsage`), ordered tool call history (`Vec<ToolCallRecord>`), wall-clock duration, finish reason (`LoopFinishReason`), and context stats snapshot.
- **(core)** `CollectingObserver` always records tool call events into `AgentResult.tool_calls` — no custom observer needed by consumers.
- **(core)** `LoopFinishReason` enum classifies loop termination: `Stop`, `MaxIterations`, `Cancelled`, `RetryBudgetExhausted`, `ConsecutiveEmptyResponses`.
- **(core)** `LoopParams` bundles `run_loop` parameters (client, registry, config, cancel, gate, observer, compaction_client).

### 📚 Documentation

- **(docs)** Update ARCHITECTURE.md, README.md, and mdBook docs to reflect `AgentResult` return type, new key types, updated `run_loop` signature, and `CollectingObserver`.

## [0.78.0] - 2026-06-14

### 🚜 Refactor

- **Breaking:** Make `AgentObserver` trait async via `#[async_trait]`. All notification methods (`on_state_change`, `on_text_delta`, `on_tool_call`, etc.) now return boxed futures. `on_tool_call_intercept` remains sync — it's a pure policy decision.
- **Breaking:** Remove `Transport::write_sync()`. The async `AgentObserver` trait eliminates the need for a synchronous write path — `RpcObserver` now uses `Transport::write_message().await` directly.
- *(rpc)* `RpcObserver` writes JSON-RPC notifications via `write_message().await` instead of the removed `write_sync`.
- *(ext)* `DenoObserver` calls extension hooks via direct `.await` instead of `tokio::spawn` fire-and-forget, eliminating race conditions during shutdown.
- *(core)* `classify_call`/`advance_to_next_call`/`classify_first_call` are now async; restructured mutual recursion into a loop to avoid async recursion boxing.

### Migration

If you implement `AgentObserver`:
- Add `#[async_trait]` to your `impl AgentObserver` blocks
- Change `fn on_*` to `async fn on_*`
- Add `.await` at every call site (e.g., `observer.on_text_delta(delta).await`)

If you implement `Transport`:
- Remove the `write_sync` method (no longer required by the trait)

## [0.77.2] - 2026-06-14

### 🚀 Features

- *(rpc)* Extract pluggable `Transport` trait with `StdioTransport` implementation, decoupling the JSON-RPC loop from stdin/stdout. Enables future WebSocket, Unix socket, and TCP transports without modifying dispatch logic.

## [0.77.1] - 2026-06-13

### 🚀 Features

- *(rho-ai)* Request stream usage from providers via stream_options
## [0.77.0] - 2026-06-13

### 🚀 Features

- *(rpc)* Add resumeSession and listTools JSON-RPC methods

### 🐛 Bug Fixes

- *(tools)* Resolve 31 clippy warnings across hashline, edit, file_ops

### 🎨 Styling

- *(tools)* Run cargo fmt on new modules, suppress too_many_lines

### ⚙️ Miscellaneous Tasks

- Update dependencies and refactor rpc variable naming
- Remove outdated PLAN.md

# Changelog

## [0.76.0] - 2026-06-10

### 🚀 Features

- *(instrumentation)* Add structured tracing across rho-ai, rho-core, rho-tools, and rho RPC (~115 log points)
- *(app)* Extract `SessionConfig`/`SessionMode` from 10-param `build_session`
- *(tools)* Split `files.rs` god object (2022 lines) into `hashline.rs`, `edit.rs`, `file_ops.rs`

### 🐛 Bug Fixes

- *(tests)* Use epsilon comparison for float assertions in `ApiUsage` tests

### 🚜 Refactor

- Remove unused `client_factory` and `Default` impl from `RhoAiClient`
- Remove `rho-eval` and `rho-bench` crates

### 📚 Documentation

- Remove rho-eval and rho-bench references from all docs, clean Phase wording

### ⚙️ Miscellaneous Tasks

- Remove `.plans/` directory

## [0.75.0] - 2026-06-10

### 🚀 Features

- *(session)* Add ApiUsage tracking for cumulative token counts

## [0.74.0] - 2026-06-10

### 🚀 Features

- *(tools)* Add batch_read for multi-file reads in a single turn

### ⚙️ Miscellaneous Tasks

- *(release)* Prepare 0.74.0

## [0.73.0] - 2026-06-10

### 🚀 Features

- *(provider)* Config-as-truth model resolution, zero network at startup
- *(provider)* Add agent.provider for provider-first model selection
- *(reasoning)* Send reasoning_effort parameter to thinking-capable models

### 📚 Documentation

- Update model resolution docs for config-as-truth
- Final cleanup — remove stale REPL references, add reasoning_effort

### ⚙️ Miscellaneous Tasks

- *(release)* Prepare 0.73.0

## [0.72.0] - 2026-06-10

### 🐛 Bug Fixes

- *(tests)* Eliminate flaky persist tests with UUID-based isolation

### 🚜 Refactor

- *(repl)* Remove rho-repl crate
- *(prompts)* Slash system prompt by ~50% to reclaim context window
## [0.71.3] - 2026-06-08

### 🐛 Bug Fixes

- *(agent)* Teach cwd parameter instead of cd chaining for subdirectory work
## [0.71.2] - 2026-06-08

### 🐛 Bug Fixes

- Add backticks around provider names in doc comment
- *(client)* Add timeouts and logging to list_models for reliable /providers reachability
- *(client)* Make ModelInfo object field optional for OpenRouter compat
## [0.71.1] - 2026-06-08

### 🐛 Bug Fixes

- *(provider)* Fixed model listing for providers with non-standard base paths (OpenRouter `/api/v1/…`, Groq `/openai/v1/…`) — `list_models` now derives the models URL from the chat-completions endpoint instead of hardcoding `/v1/models`

## [0.69.1] - 2026-06-07

### 🐛 Bug Fixes

- *(provider)* `/model` now discovers and switches the correct provider when changing models at runtime — previously only updated the session model string, leaving requests routed to the startup provider
- *(rpc)* `setModel` response now includes the active provider name
- *(repl)* `/model` output now shows the active provider alongside the model name

## [0.68.1] - 2026-06-06

### 🚀 Features

- *(rpc)* [**breaking**] Switch to JSON-RPC 2.0 protocol

## [0.68.0] - 2026-06-06

### ⚙️ Miscellaneous Tasks

- *(release)* Prepare 0.67.0
## [0.67.0] - 2026-06-06

### 🐛 Bug Fixes

- Add missing backtick in doc comment, add .gitattributes for LF

### ⚙️ Miscellaneous Tasks

- *(release)* Prepare 0.66.0
## [0.66.0] - 2026-06-06

### 🛠️ Fixes

- Set extension sandbox cwd to project root instead of extension directory — user-level and project-level extensions can now read/write project files via relative paths
- Fix system prompt instructing model to write extensions to `~/.rho/extensions/` (outside sandbox) — changed to `<project>/.rho/extensions/`
- Add command denylist and cwd enforcement to extension `rho.runCommand()` — parity with built-in `RunCommand` tool
- Clarify network access in system prompt — no direct internet, but extensions may provide it when enabled via `network = true`
- Warn to stderr when no project marker is found and CWD becomes the sandbox root

### 🗑️ Removals

- Remove dead `SandboxConfig` struct and `sandbox.enabled` config field — the sandbox is always enabled and cannot be disabled

## [0.58.0] - 2026-05-31

### 🚀 Features

- Enhanced context management: phase-aware compaction summaries, selective turn-internal eviction, tool-specific structural outlines, session phase detection, model-aware token estimation, auto-compaction
- Add `session_summary` tool for context recovery
- Add priority pinning for eviction protection
- Structured JSON output for `CargoTest` via `--format json`
- Configurable `completion_reserve` in `config.toml`
- Add progress checkpointing instructions to system prompt
- System prompt teaches agents they can author TypeScript extensions

### 🛠️ Fixes

- Fix UTF-8 char boundary panics in compaction and REPL truncation
- Fix bare `/model` command (now lists models instead of erroring)
- Fix Windows test failures in rho-ext
- Fix extension config reload from disk on `/reload`
- Fix JSON handling in web-probe extension
- Fix loaded extensions not injected into system prompt
- Remove context-pressure nudge, superseded by structural mechanisms
- Relax hashline perf test threshold for CI runners

### 🏗️ Refactors

- Split `session.rs` into 9 submodules (builder, accessors, extensions, context, tree, truncation, append, header/stats)
- Reduce tool-result amplification with stricter truncation
- Fix all 69 rustdoc warnings across workspace

## [0.54.0] - 2026-05-29

### 🛠️ Fixes

- Fix `console.time`/`console.timeEnd` bug in extension host shim
- Add `network` risk level for `rho.fetchUrl()` tool
- Accept JSON Schema parameter definitions in extension tool manifests
- Collapse `HostState` fields for simpler initialization

## [0.53.0] - 2026-05-29

### 🚀 Features

- Replace one-off `HttpExecutor` with general-purpose `AsyncDispatcher` for safe async→sync bridging
- Add standard Web API shims for extensions: `console`, `fetch()`, `URL`, `URLSearchParams`, `Headers`, `Response`, `Request`, `btoa`/`atob`, `TextEncoder`/`TextDecoder`, `structuredClone`, `setTimeout`/`setInterval` (stubs)
- Add `op_rho_url_parse`, `op_rho_url_parse_search_params`, `op_rho_url_serialize_search_params` ops (backed by `url` crate)
- Add boilerplate-reduction macros: `err!`, `json!`, `require_perm!`, `require_field!`, `js_fn!`, `js_json_fn!`, `rho_js!`, `ops_list!`

### 🛠️ Internal

- Change extension JS loading from ESM to plain `js` scripts (deno_core 0.401.0 limitation)
- Block-scope `std_shim.js` to avoid `const` collisions with `host_shim.js`
- Refactor existing ops to use new macros (-76 lines boilerplate)

### 🧪 Tests

- Add 74 new tests (219 unit + 7 integration, all passing)

## [0.52.0] - 2026-05-29

### 🚀 Features

- Add `rho.fetchUrl()` host op for HTTP requests from extensions (requires `network = true` permission)
- Add `HttpExecutor` background thread for async I/O from synchronous V8 ops
- Add `allow_network` permission to extension `HostState`

### 🛠️ Fixes

- Switch extension thread backing tokio runtime from `current_thread` to multi-threaded, fixing flaky async I/O (hyper cancels requests on `current_thread`)
- Fix `web-fetch` extension calling `rho.fetchUrl()` which did not exist

## [0.51.0] - 2026-05-28

### 🚀 Features

- Wire rho-ext extension system into the rho binary — extensions are discovered, loaded, and registered at startup
- Add `/reload` REPL command for hot-reloading extensions at runtime
- Add `/extensions` REPL command to list loaded extension names
- Add `CompositeObserver` to fan out agent-loop events to REPL + extension observers (first Block wins for interception)
- Propagate model changes to extensions via `/model` and RPC `set_model`
- Fire extension `onLoad` hooks after app construction
- Add `loaded_names()` and `set_model_all()` to `ExtensionLoader`

### 🐛 Bug Fixes

- Fix tracing log output silently dropped — `WorkerGuard` is now stored in `App` to keep the non-blocking writer alive for the full application lifetime


## [0.50.0] - 2026-05-28

### 🚀 Features

- **(rho-ext)** Add `ExtensionLoader` with hot-reload support — mtime-based change detection, selective respawn, automatic tool registry update (12 tests)
- **(rho-core)** Add `ToolRegistry::unregister` and `unregister_by_prefix` for dynamic tool management during hot reload (5 tests)
- **(rho-ext)** Add `ExtensionConfig` and `ExtensionPermissions` in rho-core config — TOML `[extensions]` section with enabled/disabled allowlists, default and per-extension permission overrides (10 tests)
- **(rho-ext)** Add `filter_by_config` and `resolve_permissions` to discovery pipeline — config-driven extension filtering (7 tests)
- **(rho-ext)** Ship `rho.d.ts` type definitions for extension author IntelliSense — `ExtensionManifest`, `ToolDefinition`, `RhoGlobal`, and all supporting types

## [0.49.0] - 2026-05-28

### 🚀 Features

- **rho-ext**: Add TypeScript extension runtime with V8 isolate, manifest extraction, module loader, and host functions (50 tests)

## [0.47.1] - 2026-05-26

### 🚀 Features

- *(repl)* Add \paste command for multi-line input
- Add `-c`/`--continue` flag to resume the last session, `/sessions` REPL command
- Add context window status bar and `/status` REPL command
- Scaffold rho-ai crate with unified types, trait, SSE parser, and retry
- *(rho-ai)* Implement OpenAI-compatible provider
- *(rho-core)* Wire rho-ai into rho-core, replace LocalChatClient

### 🐛 Bug Fixes

- Write REPL observer output to stdout instead of stderr
- Set mtime on both files in find_latest test to prevent CI flake
- Move approval gate and REPL error output to stdout to prevent stderr colour leakage
- Strip ANSI escape codes from shell command output
- Make completions_url idempotent, clean up tests

### 💼 Other

- Rho-ai-openai-provider into trunk

### 🚜 Refactor

- Redesign agent loop as state machine, extract LoopParams
- Unify LLM types on rho-ai, replace ChatClient with LlmService (Phases 1-3)
- Extract send_streaming into three testable helpers
- Replace ToolSchema with rho_ai::ToolDefinition in Session and ToolRegistry
- Rewrite Session::send_current() to use LlmService
- Remove ChatClient trait and deprecated adapters (Phase 5)

### 📚 Documentation

- Add session discovery & context visibility phase plan, update roadmap
- Sync documentation with 0.45.0-0.46.0 features

### 🎨 Styling

- Collapse ShellOutput::new call to single line in test

### ⚙️ Miscellaneous Tasks

- *(rho-core)* Clean up LocalChatClient references
- Resolve clippy lints across rho-ai and rho-core
- Clean up stale comment in Provider::llm_service()

---

## [0.41.1] - 2026-05-21

### 🐛 Bug Fixes

- *(tools)* **Hashline collision saturation** — expanded hash from 2 characters (256 values, 22% collision rate on a 106-line file) to 4 characters (65,536 values, 1.9% collision rate). Birthday-problem 50% threshold moves from ~19 lines to ~302 lines.
- *(tools)* **Hashline retry spiral** — replaced hard-fail on hash mismatch with tiered fuzzy anchor matching: exact match → relax on high-information lines → neighborhood search ±5 lines → hard fail with fresh hashes. Eliminates the model round-trip penalty on stale anchors after chained edits.
- *(tools)* **Diff display bug** — `format_hashline_diff` was showing new content on both `-` and `+` lines of a modification. The `-` line now correctly shows old content.
- *(tools)* **Fresh anchors block** — successful edits now include a `<fresh-anchors>` block with ±5 lines of freshly-hashed context around changed regions, giving the model actionable anchors for chained edits without re-reading.

### 🧪 Evaluation

- Validated against deepseek-v4-flash via OpenRouter: 8/10 eval scenarios pass (consistent with pre-fix baseline). No hashline-related regressions or stuck loops observed.

---

## [0.41.0] - 2026-05-21

### ✨ New Features

- *(tools)* **Hashline editing** — `read_file` now outputs content with `LINE#HASH:` prefix by default, providing content-addressed line references for reliable editing.
- *(tools)* `edit_file` accepts hashline anchors (`{op, pos, lines}`) for replace, append, prepend, and delete operations. Hash mismatches fail with fresh hashes for the surrounding ±3 lines.
- *(tools)* Successful hashline edits return a `<diff>` block with hashline anchors for chained editing without re-reading.
- *(core)* System prompt updated with hashline editing workflow and examples.
- *(tools)* New module `rho-tools/src/hashline.rs` — custom 2-character hash computation (alphabet `ZPMQVRWSNKTXJBYH`, 256 combinations, zero dependencies).

### 🔄 Changed

- *(tools)* `read_file` default output format is now hashline (opt-out via `hashline: false`).
- *(tools)* `edit_file` description updated to document hashline operations.
- *(tools)* Legacy `old_text`/`new_text` format continues to work; mixed hashline+legacy edits supported.
- *(eval)* Scenario 06 verifier broadened to accept all valid Option-handling patterns (`match`, `if let`, `.copied()`, `.and_modify()`), not just entry-API shortcuts.

---

## [0.36.9] - 2026-05-17

### ✨ New Features

- *(tools)* Add `rustdoc_lookup` tool — resolves Rust stdlib queries to local
  rustdoc HTML, extracts documentation, and returns it to the model without
  network access. Supports bare names (Vec), qualified paths
  (std::collections::HashMap), method queries (Option::map), primitives,
  macros, and section filtering.
- *(eval)* Add scenario 06 — tests that the agent can use rustdoc_lookup to
  understand HashMap::get return type and fix a compilation error.

### 🔄 Changed

- *(core)* Send `max_tokens` in API requests (set to completion reserve) so
  thinking/reasoning models receive explicit output budget from the server.

## [0.36.5] - 2026-05-16

### 🐛 Bug Fixes

- *(repl)* Fix REPL hang using tokio::spawn_blocking for stdin reads
- *(repl)* Properly isolate blocking stdin operations from async runtime
- *(deps)* Remove unnecessary tracing dependency from rho binary
- *(deps)* Keep io-std feature in tokio for spawn_blocking compatibility

### 🧹 Cleanup

- Remove all temporary debugging code from REPL
- Clean up debug output from main function
- Remove test files created during debugging

---

## [0.36.4] - 2026-05-16

### 🐛 Bug Fixes

- *(repl)* Fix REPL hang caused by reqwest 'stream' feature interfering with blocking stdin
- *(repl)* Replace blocking stdin with async stdin (tokio::io::stdin()) to work with streaming
- *(deps)* Add 'io-util' feature to tokio for async stdin support

---

## [0.36.3] - 2026-05-16

### 🐛 Debug

- Add stderr debug output and tracing dependency to diagnose empty log files
- Add tracing test log on startup to verify logging system works

---

## [0.36.2] - 2026-05-15

### 🐛 Bug Fixes

- *(debug)* Add enhanced logging for SSE stream parsing to diagnose REPL output issues

---

## [0.36.1] - 2026-05-15

### 🐛 Bug Fixes

- *(core)* Fix streaming bug where `stream: false` was sent instead of `stream: true`, causing no model output

---

## [0.35.2] - 2026-05-12

### 🚜 Refactor

- Consolidate `single_text_turn` and `single_tool_turn` helpers into rho-test-helpers
- Rename `phase_2_5_tests.rs` to `session_integration_tests.rs` for clarity

### 🧪 Testing

- Update test documentation to reflect new helpers in rho-test-helpers

---

## [0.35.0] - 2026-05-11

### 🚀 Features

- *(cli)* Make model and system prompt configurable via CLI arguments
- *(core)* Implement Phase 1a agent loop and Phase 1b security surface
- Resolve Phase 1 review items
- *(core)* Add ShellExecutor trait and PowerShellExecutor implementation
- *(tools)* Add command denylist, path normalization, and working directory warning
- Add ListDir and EditFile tools, multi-tool-call support (v0.7.0)
- Implement minimal config loader (Phase 2, Task 6)
- Config-driven redaction (task 7) and PowerShell-aware prompt (task 8)
- Egress enforcement, provider consent warning, expanded deserialization tests (Phase 2 Tasks 10–12)
- Configurable token budget, raise default 8K → 32K (Phase 2 Task 13)
- Cross-platform support, auto-detect model/project, compact prompt, HTTP error retryability
- Add structured tracing instrumentation for data model assessment
- Add --prompt-file CLI flag and amnesia test scenarios
- *(core)* Add EntryId newtype and uuid dependency (Phase 2.5, tasks 1–2)
- *(core)* Add Entry, EntryPayload, EntryResolution, CompactionSummary types (Phase 2.5, task 3)
- *(core)* Add Session, SessionHeader, SessionId, TokenEstimator (Phase 2.5, task 4)
- *(core)* Fix context window amnesia bug, add Phase 2.5 session tree foundation
- *(core)* Add tree navigation and context building (Phase 2.5, tasks 7–8)
- *(session)* Add CompactionStrategy trait and MechanicalCompactionStrategy (Phase 2.5 Task 9)
- *(session)* Add ExtensionEntry and ExtensionMessageEntry traits (Phase 2.5 Task 10)
- Wire agent loop to Session, add JSONL persistence, update binary and docs (Phase 2.5 Tasks 11-15)
- Add working directory awareness (Phase 3 CWD tasks 1-3)
- Adaptive context fixes 1-2, details store, budget diagnostics, cleanup
- Stuck-loop detection, run_command cwd parameter, edit_file hint improvements
- Context:end sentinel, SessionEnded trailer, improved edit_file hints
- *(phase-3)* Add rho-highlight crate with tree-sitter Rust grammar
- Add CargoCheck tool with structured diagnostic parsing (Phase 3 Task 2)
- Add CargoClippy tool (Phase 3 Task 3)
- Add RustcExplain tool (Phase 3 Task 4)
- Add CargoTest tool (Phase 3 Task 5)
- Add CargoFix tool (Phase 3 Task 6)
- Add AST context for diagnostic spans (Phase 3 Tasks 7-8)
- Add tree-sitter node-splitting validation to EditFile (Phase 3 Task 9)
- Add Rust-aware system prompt extension (Phase 3 Task 10)
- Create rho-eval behavioural benchmark suite (Phase 3 Task 12)
- Surface reasoning content from reasoning models in agent output
- Add show_reasoning config flag (default: summary-only)
- Add rho-bench multi-model benchmark harness
- Add external model provider support and OpenRouter integration for rho-bench
- *(rho)* Add CLI flags for endpoint, api-key-env, and max-iterations

### 🐛 Bug Fixes

- *(test)* Pin base_prompt SHA-256 to LF hash for CI compatibility
- *(test)* Correct base_prompt SHA-256 hash in pinned test
- *(test)* Normalize line endings before hashing base_prompt SHA-256
- *(security)* Address H1–H6 from Phase 2 review
- Address M1, M3, M9, M10, M11, M12 from Phase 2 review
- Address L1, L4, L7, L9, L11, L12 from Phase 2 review
- Default tracing filter to info when RHO_LOG is unset
- Clean up tracing instrumentation
- *(edit_file)* Detect regex patterns in old_text and surface diagnostic hint
- Append tool result even when tool execution fails
- Detect finish_reason=length and recover instead of silently dropping response
- Handle empty stop responses and unblock context window eviction
- Resolve relative paths against sandbox root in file tools
- *(eval)* Correct E0308 verifier false negative
- Inline format args and isolate config test from user config
- *(rho)* Auto-allow egress host when --endpoint is set

### 💼 Other

- Program functions end to end, takes a chat message, send it to the model, returns the response and prints it to the console
- Add github workflow to build docs

### 🚜 Refactor

- Consolidate assert_no_orphan_tool_results into rho-test-helpers
- Remove egress enforcement

### 📚 Documentation

- Add AGENTS.md and fix trailing semicolon lint
- *(plans)* Add data model and tree-sitter goals to roadmap
- *(plans)* Add TDD discipline and test suite audits to all phases
- *(plans)* Add dependency philosophy and per-phase dependency analysis
- Split roadmap into phase files, scaffold mdbook docs
- *(plans)* Add Phase 2 pre-planning and ShellExecutor task plan
- *(plans)* Add combined summary for Phase 2 tasks 1 and 2
- *(plans)* Add token budget configurability and close context management gap
- Add Phase 2.5 adaptive-resolution context plan to .plans
- Mark Phase 2 complete, add Phase 2.5 to roadmap
- Add 5 prompt scenarios exercising Rust tooling
- Add bash scenario runners and mark Phase 3 complete in plan docs
- Add Phase 4 readiness assessment
- Fill in all TODO stubs, update book to reflect current state
- Add external provider guide and startup validation for non-OpenAI endpoints
- Remove egress references and add new CLI flags

### 🧪 Testing

- Add CargoCheck → EditFile → CargoCheck integration test (Phase 3 Task 11)
- Extract JSON fixtures and add fixture-based tests (Phase 3 Task 13)
- Add alternate prompt variants for Scenario 01

### ⚙️ Miscellaneous Tasks

- Initial project scaffold
- Bump version to 0.1.1
- Resolve clippy lints from CI
- *(release)* Prepare 0.2.2
- *(release)* Prepare 0.3.0
- Add CI and security audit GitHub workflows, bump version to 0.3.1
- *(release)* Prepare 0.4.0
- Update release.toml for cargo-release 1.x
- Update plan to reflect completed phases, clean up test arrtifacts
- *(release)* Prepare 0.6.0
- *(release)* Prepare 0.11.0
- Add logs/ to .gitignore
- Update plans to match status of code base
- *(release)* Prepare 0.23.0
- Cleanup of test suite
- *(release)* Bump version to 0.27.0
- *(release)* Bump version to 0.28.0 — Phase 3 complete
- Fix clippy pedantic warnings across rho-bench and rho-eval
- *(release)* Bump version to 0.34.0