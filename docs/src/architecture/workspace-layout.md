# Workspace Layout

```
rho-coding-agent/
├── Cargo.toml              # Workspace root — version, edition, lints
├── CHANGELOG.md             # Generated via git-cliff
├── cliff.toml               # git-cliff configuration
├── AGENTS.md                # Project instructions for AI assistants
├── docs/                    # This book (mdBook)
├── rho/                     # Binary entry point + library crate (`rho` CLI)
│   └── src/
│       ├── main.rs           # Thin: parse CLI, build App, run
│       ├── lib.rs            # Module declarations
│       ├── cli.rs            # `Cli` — 17 CLI flags with clap
│       ├── app.rs            # `App` — runtime state, build/run orchestration
│       ├── model.rs          # Model resolution + interactive picker
│       ├── gate.rs           # Approval gate module root
│       ├── gate/
│       │   └── interactive.rs  # `ReplApprovalGate` — y/N from stdin
│       ├── rpc.rs            # RPC mode: `run_rpc`, `run_rpc_on`, observer, approval gate (68 tests)
│       ├── repl.rs           # `run_repl()` — REPL loop
│       ├── presenter.rs      # Presenter module root
│       ├── presenter/
│       │   ├── repl.rs       # `ReplPresenter` — all REPL terminal output
│       │   └── rpc.rs        # `RpcPresenter` — startup output for RPC mode
├── rho-core/                # Agent kernel
│   └── src/
│       ├── lib.rs           # Module declarations, convenience re-exports
│       ├── agent.rs         # Agent loop state machine, `run_loop`
│       ├── approval.rs      # `ApprovalPolicy`, `ApprovalGate` traits
│       ├── client/
│       │   ├── mod.rs        # `RhoAiClient`, `ProviderRegistry`, `provider_factory`
│       │   └── error.rs      # `ClientError`
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
│       ├── provider.rs      # `Provider` trait, `OpenAiCompatibleProvider`, `ProviderRegistry`
│       ├── session.rs       # `Session`, `Entry`, tree persistence, compaction
│       ├── session/
│       │   ├── compaction.rs # `CompactionStrategy`, `MechanicalCompactionStrategy`
│       │   ├── entry.rs     # `Entry`, `EntryPayload`, `EntryResolution`
│       │   ├── error.rs     # `SessionError`
│       │   ├── estimator.rs # `TokenEstimator`, `HeuristicEstimator`
│       │   └── persist.rs   # JSONL persistence, `SessionMetadata`
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
│       ├── hashline.rs      # Hashline content-addressed editing
│       ├── shell.rs         # `RunCommand`, `PowerShellExecutor`, `CommandDenylist`
│       └── rust.rs          # `CargoCheck`, `CargoClippy`, `CargoTest`, `CargoFix`, `RustcExplain`
├── rho-test-helpers/        # Shared test infrastructure (dev-only)
│   └── src/lib.rs           # `MockChatClient`, `TestProvider`, response builders, helpers
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
