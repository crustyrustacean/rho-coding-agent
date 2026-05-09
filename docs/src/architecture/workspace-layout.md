# Workspace Layout

```
rho-coding-agent/
├── Cargo.toml              # Workspace root — version, edition, lints
├── CHANGELOG.md             # Generated via git-cliff
├── cliff.toml               # git-cliff configuration
├── AGENTS.md                # Project instructions for AI assistants
├── docs/                    # This book (mdBook)
├── rho/                     # Binary entry point (`rho` CLI)
│   └── src/main.rs          # REPL loop, tool wiring, CLI dispatch
├── rho-core/                # Agent kernel
│   └── src/
│       ├── lib.rs           # Module declarations, convenience re-exports
│       ├── agent.rs         # Agent loop state machine, `run_loop`
│       ├── approval.rs      # `ApprovalPolicy`, `ApprovalGate` traits
│       ├── client.rs        # `ChatClient` trait, `LocalChatClient`
│       ├── config.rs        # `RhoConfig`, `ConfigLoader`, two-tier TOML loading
│       ├── context.rs       # `ContextManager`, `SlidingWindowContextManager`, `TokenBudget`
│       ├── context_files.rs # Project context file scanner, `TrustStore`, prompt composition
│       ├── conversation.rs  # `Conversation`, `AssistantResponse`
│       ├── diagnostic.rs    # Structured compiler diagnostic types
│       ├── error.rs         # `RhoError` and `Result`
│       ├── message.rs       # `ChatMessage`, `ContentBlock`, `ModelToolCall`
│       ├── newtypes.rs      # `FilePath`, `ToolName`, `ToolCallId`, `EntryId`, `DiagnosticCode`
│       ├── prompts.rs       # `base_prompt()`, `compact_prompt()` (embedded from `prompts/`)
│       ├── redact.rs        # `Redactor` — secret pattern matching
│       ├── request.rs       # `ChatRequest`
│       ├── response.rs      # `ModelResponse`, `FinishReason`, `ModelUsage`
│       ├── sandbox.rs       # `SandboxRoot` — file sandbox validation
│       ├── schema.rs        # `ToolSchema` — wire-format tool definitions
│       ├── session.rs       # `Session`, `Entry`, tree persistence, compaction
│       └── shell.rs         # `ShellExecutor` trait, `ShellOutput`
├── rho-highlight/           # Tree-sitter syntax analysis
│   └── src/
│       ├── lib.rs           # Re-exports
│       ├── lang.rs          # `Language` enum
│       ├── parse.rs         # Tree-sitter parsing
│       ├── highlight.rs     # Token classification and highlighting
│       └── query.rs         # `node_at()` — AST node lookup by position
├── rho-tools/               # Built-in tool implementations
│   └── src/
│       ├── lib.rs           # `register_all()`
│       ├── files.rs         # `ReadFile`, `WriteFile`, `ListDir`, `EditFile`
│       ├── shell.rs         # `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
│       └── rust.rs          # `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
├── rho-test-helpers/        # Shared test infrastructure (dev-only)
│   └── src/lib.rs
├── rho-bench/               # Benchmark harness for multi-model evaluation
│   └── src/
│       ├── main.rs          # CLI: --models, --tasks, --repeats, --output
│       ├── harness.rs       # Task execution via run_loop, CountingClient
│       ├── comparison.rs    # Terminal table and per-task breakdown display
│       └── persistence.rs   # JSON result files (latest.json + timestamped)
├── rho-eval/                # Behavioural benchmarks (dev-only)
│   └── src/
│       ├── lib.rs
│       ├── task.rs          # EvalTask trait, TaskOutcome, TaskMetrics
│       ├── report.rs        # EvalRun, EvalReport — results + regression gating
│       └── tasks.rs         # 5 built-in task definitions
└── xtask/                   # Dev task runner
    └── src/main.rs          # `cargo xtask ci`, `cargo xtask test`, `cargo xtask changelog`
```
