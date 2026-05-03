## [0.19.0] - 2026-05-03

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
- *(core)* Add ToolResultDetails extensible enum with FullOutput variant (Phase 2.5, task 13)
- *(core)* Implement HeuristicEstimator with per-model EMA calibration (Phase 2.5, task 6)
- *(core)* Add Session append operations with bounded tool-result handling (Phase 2.5, task 5)
- *(core)* Restructure TokenBudget with prompt_budget() and completion_reserve (P2.5-10)
- *(core)* Pin first user turn and last turn in SlidingWindowContextManager to prevent amnesia
- *(core)* Backport bounded tool results and first-turn pinning to Conversation for live fix

### 🐛 Bug Fixes

- *(test)* Pin base_prompt SHA-256 to LF hash for CI compatibility
- *(test)* Correct base_prompt SHA-256 hash in pinned test
- *(test)* Normalize line endings before hashing base_prompt SHA-256
- *(security)* Address H1–H6 from Phase 2 review
- Address M1, M3, M9, M10, M11, M12 from Phase 2 review
- Address L1, L4, L7, L9, L11, L12 from Phase 2 review
- Default tracing filter to info when RHO_LOG is unset
- *(core)* Fix context window amnesia bug — oversized tool results now truncated with full output preserved; first user turn never evicted (closes P2.5-1, P2.5-9, P2.5-10)
- Clean up tracing instrumentation

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

### ⚙️ Miscellaneous Tasks

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
## [0.2.0] - 2026-04-25

### 💼 Other

- Program functions end to end, takes a chat message, send it to the model, returns the response and prints it to the console

### ⚙️ Miscellaneous Tasks

- Initial project scaffold
- Bump version to 0.1.1
