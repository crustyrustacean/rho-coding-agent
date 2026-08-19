# Workspace Layout

```
rho-coding-agent/
├── Cargo.toml              # Workspace root — version, edition, lints
├── CHANGELOG.md             # Generated via git-cliff
├── cliff.toml               # git-cliff configuration
├── AGENTS.md                # Project instructions for AI assistants
├── docs/                    # This book (mdBook)
│   └── rpc-schema/
│       └── openrpc.json    # OpenRPC 1.3.1 schema (machine-readable API spec)
├── rho/                     # Binary entry point (headless JSON-RPC 2.0)
│   └── src/
│       ├── main.rs           # Thin: parse CLI, build App, run
│       ├── lib.rs            # Module declarations
│       ├── cli.rs            # `Cli` — CLI flags with clap; `Command::Extension` subcommand
│       ├── app.rs            # `App` — runtime state, build/run orchestration, extension loading
│       ├── ext_cli.rs        # `rho extension` subcommand handlers (sync/install/remove/list)
│       ├── ext_observer.rs   # `CompositeObserver` — fans out to RPC observer + extension observers
│       ├── rpc.rs            # JSON-RPC 2.0: `run_rpc`, `run_rpc_on`, observer, approval gate
│       ├── rpc_wire.rs       # Typed wire-format structs (params, results, notifications)
│       ├── transport.rs      # `Transport` trait, `StdioTransport` (newline-delimited JSON)
│       └── presenter/
│           └── rpc.rs        # `RpcPresenter` — diagnostic output to stderr
├── rho-ai/                 # Unified LLM provider abstraction
│   └── src/
│       ├── lib.rs           # Re-exports: `LlmService`, `EventStream`, unified types
│       ├── service.rs       # `LlmService` trait
│       ├── openai.rs        # `OpenAiService` — Chat Completions HTTP + SSE
│       ├── responses.rs     # `ResponsesService` — OpenAI Responses HTTP + SSE
│       ├── sse.rs           # Server-sent event parser
│       ├── retry.rs         # Exponential backoff retry logic
│       ├── catalog.rs       # `Catalog`, `Model`, `ModelCost` — model registry + pricing
│       ├── catalog_generated.rs # Generated model list (`cargo xtask generate-models`)
│       ├── types.rs         # `LlmMessage`, `LlmRequest`, `StreamEvent`, `AccumulatedResponse`
│       └── error.rs         # `ProviderError`
│   └── model-overrides.json # Manual catalog overrides (thinking flags, compatibility)
├── rho-ext/                # TypeScript extension runtime (V8/deno-core)
│   └── src/
│       ├── lib.rs          # Re-exports: `ExtensionLoader`, `DenoTool`, `DenoObserver`, etc.
│       ├── runtime.rs      # `ExtensionRuntime` — V8 isolate on dedicated OS thread
│       ├── async_dispatcher.rs # Async bridge between V8 isolate and tokio
│       ├── loader.rs       # `ExtensionLoader` — discovery, filtering, spawning, hot reload
│       ├── deno_tool.rs    # `DenoTool` — `Tool` trait wrapper
│       ├── deno_observer.rs# `DenoObserver` — `AgentObserver` wrapper
│       ├── manifest.rs     # `LoadedExtension` — manifest parsing and validation
│       ├── discover.rs     # Extension discovery (single-file and multi-file)
│       ├── transpile.rs    # TS → JS transpilation
│       ├── module_loader.rs# `RhoModuleLoader` — ESM module resolution
│       ├── host.rs         # `rho.*` host ops (log, readFile, writeFile, runCommand, getModel, getCwd)
│       ├── ops.rs          # Helper macros for host op boilerplate
│       ├── error.rs        # `ExtensionError`
│       ├── host_shim.js    # ESM shim for extension module loading
│       └── std_shim.js    # Standard library shim for extensions
│   └── types/
│       └── rho.d.ts        # TypeScript type definitions for extension authors
├── rho-core/                # Agent kernel
│   └── src/
│       ├── lib.rs           # Module declarations, convenience re-exports
│       ├── agent.rs         # Agent loop state machine, `run_loop`, `AgentResult`, `CollectingObserver`, `LoopFinishReason`, phase tracking, auto-compact
│       ├── builder.rs       # `Agent` + `AgentBuilder` — embeddable orchestration core, `TurnInputs`, model resolution
│       ├── approval.rs      # `ApprovalPolicy`, `ApprovalGate` traits
│       ├── client.rs        # `RhoAiClient`, `resolve_api_key`, `is_local_endpoint`, `ModelInfo`, `ModelList`
│       ├── client/          # Module directory
│       │   └── error.rs      # `ClientError`
│       ├── config.rs        # `RhoConfig`, `ConfigLoader`, two-tier TOML loading
│       ├── context.rs       # `ContextManager`, `SlidingWindowContextManager`, `TokenBudget`
│       ├── context_files.rs # Project context file scanner, `TrustStore`, prompt composition
│       ├── conversation.rs  # `Conversation`, `AssistantResponse`
│       ├── denylist.rs      # `CommandDenylist` — shell command denylist
│       ├── diagnostic.rs    # Structured compiler diagnostic types
│       ├── error.rs         # `RhoError` and `Result`
│       ├── message.rs       # `ChatMessage`, `ContentBlock`, `ModelToolCall`
│       ├── model_match.rs   # Model name matching / fuzzy matching
│       ├── newtypes.rs      # `FilePath`, `ToolName`, `ToolCallId`, `EntryId`, `DiagnosticCode`
│       ├── prompts.rs       # `base_prompt()`, `compact_prompt()` (embedded from `prompts/`)
│       ├── redact.rs        # `Redactor` — secret pattern matching
│       ├── request.rs       # `ChatRequest`
│       ├── response.rs      # `ModelResponse`, `FinishReason`, `ModelUsage`
│       ├── sandbox.rs       # `SandboxRoot` — file sandbox validation
│       ├── schema.rs        # `ToolSchema` — wire-format tool definitions
│       ├── provider.rs      # `Provider` trait, `OpenAiCompatibleProvider`, `ProviderRegistry`
│       ├── session.rs       # `Session` struct, module declarations, re-exports
│       ├── session/
│       │   ├── accessors.rs    # Header, leaf, entry, flush, path, getters/setters
│       │   ├── append.rs       # Append ops, close, extension typed methods, append_entry core
│       │   ├── builder.rs      # `new`, `in_memory`, `with_*` builder methods
│       │   ├── compaction.rs   # `CompactionStrategy`, `MechanicalCompactionStrategy`, `LlmCompactionStrategy`
│       │   ├── context.rs      # `path_messages`, `send_current`, `compact_older_than`, `context_stats`, `prepare_context`
│       │   ├── context_stats.rs # `ContextStats`, `RoleTokenDistribution`, `ResolutionTokenDistribution`, `PhaseTokenDistribution`
│       │   ├── entry.rs        # `Entry`, `EntryPayload`, `EntryResolution`, `CompactionPhase`
│       │   ├── error.rs        # `SessionError`
│       │   ├── estimator.rs    # `TokenEstimator`, `HeuristicEstimator`
│       │   ├── eviction.rs     # Selective downgrade planner (`plan_downgrades`)
│       │   ├── extensions.rs   # `ExtensionEntry`, `ExtensionMessageEntry` traits
│       │   ├── header.rs       # `SessionHeader`
│       │   ├── outliner.rs     # Tool-specific structural summary formatters
│       │   ├── persist.rs      # JSONL persistence, `SessionMetadata`
│       │   ├── phase.rs        # `SessionPhase` enum, `classify_tool_phase()`, `transition_phase()`
│       │   ├── tree.rs         # `path_to_root`, `branch_to`, `branch_with_summary`
│       │   └── truncation.rs   # Bounded truncation, char boundary, footer
│       ├── shell.rs         # `ShellExecutor` trait, `ShellOutput`
│       ├── stream.rs        # `StreamChunk` — unified streaming event type
│       └── tool.rs          # `Tool` trait, `ToolRegistry`, `ToolRisk`, `ToolOutcome`
├── rho-memory/             # Persistent knowledge base (SQLite/FTS5)
│   ├── src/
│   │   ├── lib.rs           # Re-exports: `Memory`, `Document`, `SearchResult`
│   │   ├── brain.rs         # `Memory` — public knowledge-base API
│   │   ├── db.rs            # `Database` — raw SQLite operations
│   │   ├── models.rs        # `Document`, `SearchResult`, `CreateRequest`, `Stats`
│   │   └── error.rs         # Knowledge-base errors
│   └── migrations/
│       └── 001_initial.sql  # Schema: documents, FTS5, triggers
├── rho-highlight/           # Tree-sitter syntax analysis
│   └── src/
│       ├── lib.rs           # Re-exports
│       ├── lang.rs          # `Language` enum
│       ├── parse.rs         # Tree-sitter parsing
│       ├── highlight.rs     # Token classification and highlighting
│       ├── query.rs         # `node_at()` — AST node lookup by position
│       └── error.rs         # `HighlightError`
├── rho-tools/               # Built-in tool implementations
│   └── src/
│       ├── lib.rs           # `register_all()`
│       ├── files.rs         # Re-exports file tools from `file_ops` + `edit`
│       ├── file_ops.rs      # `ReadFile`, `WriteFile`, `BatchRead`, `ListDir`
│       ├── edit.rs          # `EditFile` — hashline-anchored editing
│       ├── hashline.rs      # Hashline content-addressed editing
│       ├── shell.rs         # `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
│       ├── crates_io.rs     # `CratesIoLookup`
│       ├── session_summary.rs # `SessionSummary` — compressed session history tool
│       ├── memory.rs        # `MemoryTool` — knowledge base (via `rho-memory`)
│       ├── error.rs         # Tool error types
│       └── rust/            # Rust tooling (module root: `rust.rs`)
│           ├── tools.rs     # `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
│           ├── rustdoc.rs   # `RustdocTool` — local rustdoc HTML parsing
│           ├── parse.rs     # NDJSON output parsing
│           ├── convert.rs   # Diagnostic conversion
│           ├── format.rs    # Diagnostic formatting
│           └── types.rs     # Shared Rust tool types
├── rho-test-helpers/        # Shared test infrastructure (dev-only)
│   └── src/lib.rs           # `MockChatClient`, `TestProvider`, response builders, helpers
└── xtask/                   # Dev task runner
    └── src/
        ├── main.rs          # CLI dispatch
        └── tasks.rs         # `ci`, `test`, `build`, `release`, `changelog`, `fmt`, `fmt-fix`, `lint`, `run`, `clean`, `status`, `schema`, `generate-models` tasks
```
