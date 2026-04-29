## [0.4.0] - 2026-04-28

### 🚀 Features

- *(cli)* Make model and system prompt configurable via CLI arguments
- *(core)* Implement agent loop with retry/backoff and max-iteration guard
- *(core)* Add `Tool` trait, `ToolRegistry`, and `ToolRisk` classification
- *(tools)* Implement `ReadFile`, `WriteFile`, and `RunCommand` tools
- *(core)* Add file sandbox enforcement via `SandboxRoot` and `FilePath` validation
- *(core)* Add approval gate (`ApprovalPolicy` + `ApprovalGate`) for write/destructive tools
- *(core)* Add secret redaction (`Redactor`) for known prefix-shaped secrets
- *(core)* Add untrusted-data framing (`<context>` tags) on file contents
- *(core)* Add project context file scanner with SHA-256 trust verification
- *(core)* Add context window management (`SlidingWindowContextManager`, turn-aware eviction)
- *(core)* Add `ChatClient` trait abstraction with `LocalChatClient` default
- *(core)* Embed base identity prompt at compile time
- *(core)* Add `CancellationToken` (re-exported from `tokio_util::sync`)

### 💼 Other

- Program functions end to end, takes a chat message, send it to the model, returns the response and prints it to the console

### 📚 Documentation

- Add AGENTS.md and fix trailing semicolon lint
- *(plans)* Add data model and tree-sitter goals to roadmap
- *(plans)* Add TDD discipline and test suite audits to all phases
- *(plans)* Add dependency philosophy and per-phase dependency analysis
- Split roadmap into phase files, scaffold mdbook docs

### ⚙️ Miscellaneous Tasks

- Initial project scaffold
- Bump version to 0.1.1
- Resolve clippy lints from CI
- *(release)* Prepare 0.2.2
- *(release)* Prepare 0.3.0
- Add CI and security audit GitHub workflows, bump version to 0.3.1
- *(release)* Prepare 0.4.0
