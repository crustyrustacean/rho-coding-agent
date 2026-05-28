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
