# rho-coding-agent — Roadmap

## Current Status

| Phase | Status | Summary |
|---|---|---|
| 1a: The Agent Loop | ✅ Complete | Agent loop, tool registry, `ChatClient` trait, conversation management |
| 1b: Security Surface | ✅ Complete | Approval gate, file sandbox, context-file trust, secret redaction, untrusted-data framing |
| 2: PowerShell, File Tools & Cross-Platform | ✅ Complete | PowerShell-native shell, `ListDir`/`EditFile`/`WriteFile` tools, config loader (two-tier TOML), command denylist, cross-platform support (Windows/macOS/Linux), auto-detection (project root, model), compact prompt, `RhoError::HttpError` for retry classification |
| 2.5: Adaptive-Resolution Context | ✅ Complete | Session tree, resolution levels, calibrated budget, tool-result bounding, amnesia fix, JSONL persistence, extension entries, compaction strategy |
| 3: Rust Tooling and Tree-Sitter | ✅ Complete | `rho-highlight` crate, structured diagnostics, `CargoCheck`/`CargoClippy`/`CargoTest`/`CargoFix`/`RustcExplain` tools, AST span mapping, `EditFile` node-splitting validation, Rust-aware system prompt, `rho-eval` benchmark suite (5 eval tasks), `rho-bench` harness, shared bootstrapping (`client_factory`, `compose_full_system_prompt`), test suite audit |
| 4: Terminal UI | 🔜 In Progress | Rich TUI replacing the bare REPL. Pre-Work 1 (streaming API) complete. |
| 5: Extensions and Polish | Planned | Custom tools, prompt composition with budget awareness |
| 6: LSP | Deferred | rust-analyzer integration |

**Workspace version:** 0.36.0

**Platform support:** Windows, macOS, Linux. PowerShell 7+ (`pwsh`) is the primary shell on all platforms; Windows PowerShell 5.1 (`powershell`) is the fallback on Windows only.

**Existing crates:** `rho` (binary), `rho-core`, `rho-tools`, `rho-highlight`, `rho-test-helpers`, `rho-eval`

**Not yet created:** `rho-tui`, `rho-ext`

## Architecture Overview

The project is organized as a layered workspace. Dependencies flow downward only:

```
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │  ← Assembles all layers, runs the app
├─────────────────────────────────────────────────┤
│                   rho-ext                        │  ← Extension API and runtime
├─────────────────────────────────────────────────┤
│                   rho-tui                        │  ← Terminal UI (display, input, approval)
├─────────────────────────────────────────────────┤
│                   rho-tools                      │  ← Built-in tool implementations
├─────────────────────────────────────────────────┤
│                   rho-highlight                  │  ← Tree-sitter grammars and syntax highlighting
├─────────────────────────────────────────────────┤
│                   rho-core                       │  ← Agent kernel (loop, types, traits, data model)
└─────────────────────────────────────────────────┘

   ┌──────────────────┐  ┌──────────────────┐
   │ rho-test-helpers  │  │    rho-eval       │  ← Dev-only: mocks, fixtures, benchmarks
   └──────────────────┘  └──────────────────┘
```

**Dependency rule:** a crate may only depend on crates below it in the stack. `rho-core` depends on nothing but external libraries. `rho-tools` depends on `rho-core`. And so on.

**Platform scope:** rho is a cross-platform, PowerShell-native agent running on Windows, macOS, and Linux. PowerShell (`pwsh`) is available on all three platforms and is the primary shell. Shell execution is abstracted behind a `ShellExecutor` trait so platform-specific shells can be added without rewriting the tool layer. Path normalization, process management, and project-root detection are all platform-aware. The original Windows-only focus was softened after real-world testing showed developers commonly work across platforms.

Two auxiliary crates sit outside the main stack:
- **`rho-test-helpers`** — shared test utilities (mock `ChatClient`, fixture loaders, tempdir helpers, shell detection, sandbox/trust helpers). Available as a dev-dependency to every crate.
- **`rho-eval`** — behavioural benchmark suite for evaluating agent success rate on canonical coding tasks.

## Crate Responsibilities

### `rho-core` — Agent Kernel

The foundation. Defines the contract everything else implements.

- **Data model** — The domain types that everything else builds on. These are the most important types in the project: if they're easy to work with, the whole system flows; if they're awkward, every layer pays the price. Design goals:
  - **Constructible** — types should be easy to create in code (tests, tools, extensions) without boilerplate
  - **Composable** — conversations, tool results, and diagnostic spans should combine naturally
  - **Serializable** — every domain type round-trips through serde (JSON for the API, TOML for config)
  - **Documented** — every public type, field, and variant has a doc comment explaining its purpose and constraints
  - **Newtype where it matters** — `FilePath`, `ToolName`, `ToolCallId`, `EntryId`, `DiagnosticCode` etc. as distinct types rather than raw strings
- **`ChatMessage` shape** — Modelled as a variant per role (`System`, `User`, `Assistant`, `Tool`) carrying a `Vec<ContentBlock>` rather than a flat `String`. The `Tool` variant carries `tool_call_id`; the `Assistant` variant carries `tool_calls`. This admits images, file references, tool-result binding, and future content kinds without rewriting downstream code. Serialization produces the existing OpenAI wire format (string content for the single-text-block case, array content otherwise).
- **Tool trait** — `Tool`: the interface all tools implement. Async, dyn-compatible (via `async-trait`), takes a `CancellationToken` so long-running tools can be aborted, and returns a `ToolOutcome` that admits both immediate and streaming forms. Phase 1a uses immediate only; the streaming variant exists so Phase 4's TUI streaming is a new variant rather than a workspace-wide signature change.
- **Tool registry** — Maps tool names to `Box<dyn Tool>` implementations. Tools are registered with a risk level (`Read`, `Write`, `Destructive`) that feeds into the approval policy.
- **Approval policy** — Every tool call passes through an `ApprovalPolicy` before execution (introduced in Phase 1b). The default policy requires human confirmation for destructive operations (`WriteFile`, `EditFile`, `RunCommand`). The approval gate lives in `rho-core`, not the UI layer — the TUI just renders the prompt and collects the response.
- **File sandbox** — File tools operate within a sandbox root (the project directory, or an explicit `--root` argument). `FilePath` canonicalises the path (resolving `..`, symlinks, junctions) and validates it's within the root. For not-yet-existing paths (the `WriteFile` case), the sandbox walks up to the nearest existing ancestor, canonicalises that, then verifies the would-be path stays within the root. Users can opt out in config (`sandbox = false`), but the default is safe.
- **Untrusted-data framing** — File contents and other model-untrusted text enter the conversation as `User` messages with a `<context>...</context>` wrapper inside a `ContentBlock::Text`. The system prompt instructs the model to treat anything inside `<context>` as data, not instructions. This is a defense-in-depth measure against prompt injection — not a guarantee. Tool results use the API-mandated `Tool` role with their `tool_call_id`, separately.
- **Secret redaction** — Tool results pass through a redaction layer before entering conversation history. Patterns for known prefix-shaped secrets (OpenAI `sk-...`, GitHub `ghp_...`, Slack tokens, AWS access key IDs) are replaced with `[REDACTED]`. Documented as best-effort: it catches recognisable formats and misses everything else. The approval gate remains the primary defense.
- **Command surface** — The set of operations the user can invoke beyond sending a message to the model. `rho-core` defines the capabilities; the UI layer (bare REPL or TUI) handles parsing and dispatch. See the command table below for the full surface.
- **Agent loop** — The core cycle: send prompt → receive response → if tool call, execute and feed back → repeat until `Stop`. The loop is modelled as an `AgentState` state machine with four orthogonal states: `Idle`, `Thinking`, `AwaitingApproval`, `ExecutingTool`. Errors and retries are *transition outcomes*, not states — represented as `Result<NextState, TransitionError>` with a separate retry counter. Retry semantics: retryable errors trigger exponential backoff up to a configurable budget; fatal errors terminate the loop immediately. A configurable max-iteration guard prevents infinite loops. The loop operates on a `Session` (Phase 2.5), not a `Conversation`.
- **Conversation** — Legacy message history management. Retained for backward compatibility; the agent loop now uses `Session` by default (Phase 2.5). Critically, when the model returns a tool call, the assistant message *and its `tool_calls` field* are persisted into history before the tool is executed. The subsequent `Tool` message (with matching `tool_call_id`) appends after. Skipping the persistence step causes the next API request to fail validation: a `tool` role message must immediately follow an assistant message containing the matching call.
- **Session** — Tree-shaped conversation model replacing the flat `Vec<ChatMessage>` in `Conversation`. Entries form a parent-linked tree with a movable leaf pointer. Each entry carries an explicit `EntryResolution` (`Full`, `Compacted`, `Attached`) controlling its visibility to the model. The agent loop appends typed entries (`Message`, `Compaction`, `BranchSummary`, `Custom`, etc.) and walks the leaf-to-root path to build context. Sessions persist to append-only JSONL files at `~/.rho/sessions/<project-hash>/` and reload cleanly. An in-memory mode is available for tests and ephemeral use. `Session` owns the `TokenEstimator`, `ContextManager`, `ToolSchema` list, `Redactor`, and `TokenBudget`.
- **Token estimation** — The `TokenEstimator` trait abstracts token counting. The default `HeuristicEstimator` uses per-model chars-per-token ratios that self-correct via exponential moving average (α = 0.3) against the API's actual `prompt_tokens`. Known model families bootstrap with specific ratios (Gemma 3.5, Qwen 2.5, Claude 3.7, GPT-4 4.0); unknown models default to 2.5 (conservative). Convergence is typically within 10% error by the third calibration call.
- **Tool-result bounding** — Tool results exceeding half the prompt budget are truncated at a UTF-8-safe character boundary. The truncated content enters the model's context; the full content is preserved as `ToolResultDetails::FullOutput { original_size, content }` for tools and extensions to consume. This is the core fix for the shipping amnesia bug.
- **Compaction** — The `CompactionStrategy` trait produces structured summaries of entry ranges. `MechanicalCompactionStrategy` is the deterministic default — no LLM calls. It extracts the original user request, aggregates tool-call statistics, and produces a `CompactionSummary` that `fit_path` renders as a synthetic `User` message. Compacted entries transition to `Compacted { into }` resolution but remain in the tree at lower resolution.
- **Extension entries** — The `ExtensionEntry` and `ExtensionMessageEntry` traits provide typed, versioned read/write access to structured extension state. `Custom` entries (resolution = Attached) don't consume context budget; `CustomMessage` entries (resolution = Full) participate in the LLM context. Kind strings follow the `"<author>.<feature>.v<n>"` convention for schema versioning.
- **Context window management** — A `ContextManager` trait keeps the conversation within the model's context limit. The default `SlidingWindowContextManager` pins the system message *and the first user turn* and evicts by *turn*, never splitting an assistant `tool_calls` message from its matching `tool` results. The `ContextManager` now exposes `fit_path`, a default-implemented method that walks the session tree's leaf-to-root path, filters by resolution, renders compaction summaries as synthetic messages, subtracts tool-schema and system-message overhead from the budget, and delegates to `fit` for final enforcement. The token budget splits into `context_window` and `completion_reserve` (default 4096) — all context-fitting uses `prompt_budget()`. The token budget defaults to 32K (raised from 8K in Phase 2) and is configurable via `[agent] token_budget` and `--token-budget`.
- **Base identity prompt** — The agent's core instructions live at `rho-core/src/prompts/base.md` and are included in the binary at compile time via `include_str!`. Exposed as `pub fn base_prompt() -> &'static str` in a `rho_core::prompts` module (a function rather than a `const` so runtime substitution can be added later without an API break). The `--system` CLI flag overrides it for experiments and tests; the default is always the bundled prompt. The prompt is the testable contract for agent behaviour: every integration test that exercises real model interactions runs against this known baseline. Prompt edits go through code review like any other change; `rho-eval` tracks pass-rate across prompt versions.
- **Project context files** — The agent detects and loads project-level instruction files (`AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`) from the sandbox root. These files go through the trust model: hash verification on first load, user confirmation required, re-confirmation if the file changes. Trust storage lives in `~/.rho/trusted_projects.toml`. Project context files are appended to the system prompt as clearly delimited sections, *not* as untrusted-data-framed `User` messages — they are intentional instructions the user placed in the project.
- **Provider abstraction** — A `ChatClient` trait that decouples the agent loop from any specific model provider. Local models are the primary target (LM Studio, Ollama over OpenAI-compatible endpoints), but the trait is designed so external providers (OpenAI, Anthropic, Google) can implement it without modifying `rho-core`. The concrete `LocalChatClient` (talking to `localhost`) ships as the default. Tests use a mock implementation from `rho-test-helpers`. Dyn-compatible via `async-trait` so providers can be swapped at runtime. `client_factory()` and `resolve_api_key()` in `rho-core/src/client.rs` provide shared construction logic used by both `rho` and `rho-bench`.
- **Error types** — `RhoError` and `Result`

**Command surface** — These operations must be supported by `rho-core` APIs. The UI layer decides how to expose them.

| Command | Agent operation | Required API |
|---|---|---|
| `/help` | List available commands | N/A (UI-only) |
| `/clear` | Reset conversation history (keep system message) | `Session::branch_to(&root_id)` |
| `/history` | Show conversation history | `Session::path_messages()` |
| `/system` | Show or replace the system prompt | `Session::system_prompt()` getter/setter |
| `/model` | Show or switch the current model | `Session::set_model()`, config access |
| `/provider` | Show or switch the provider | `ChatClient` swap, config access |
| `/tools` | List registered tools | `ToolRegistry::list()` |
| `/context` | Show loaded project context files and their trust status | Context file scanner API |
| `/config` | Show current configuration | Config access |
| `/quit` | Exit the agent | N/A (UI-only) |

Phase 1a implements the APIs that `rho-core` owns (`Session::branch_to()`, `ToolRegistry::list()`, etc.). The bare REPL handles `/quit` and `/clear` minimally. Phase 2.5 adds `--session` and `--ephemeral` CLI flags for session management. Phase 4 (TUI) builds out full slash-command parsing, autocomplete, and rendering. Phase 5 (extensions) may allow custom commands via `rho-ext`.

`rho-core` does **not** know about:
- PowerShell, shells, or any specific command execution
- File systems (beyond sandbox root validation)
- Terminal rendering
- Extensions or plugins
- Slash-command parsing or dispatch
- How approval prompts are rendered (it decides *whether* to approve; the UI decides *how* to ask)

### `rho-tools` — Built-in Tool Implementations

Concrete tools that ship with the agent. Split internally by domain:

**File operations:**
| Tool | Description |
|---|---|
| `ReadFile` | Read a file's contents (text or image). Output is wrapped in `<context>` framing before entering the conversation. |
| `WriteFile` | Create or overwrite a file. Uses sandbox-aware canonicalisation that handles not-yet-existing paths. |
| `EditFile` | Apply targeted replacements to a file (exact-match). Phase 3 adds tree-sitter node-splitting validation. |
| `ListDir` | List directory contents |

**Shell execution:**
| Tool | Description |
|---|---|
| `RunCommand` | Execute a PowerShell command, capture stdout/stderr and exit code. Respects the cancellation token from `Tool::execute` so Ctrl-C kills long-running commands. |

This tool is PowerShell-first. The system prompt instructs the model to generate PowerShell commands. On Windows, `pwsh` is the default; `powershell` is the fallback. On macOS and Linux, `pwsh` is required (PowerShell 7+ cross-platform). The tool normalizes path separators on Windows only (`/` → `\` in path contexts); on Unix, forward slashes are the native separator and are preserved. Process killing is platform-aware (`taskkill` on Windows, `kill -9` on Unix).

**Security controls:**
- **Command denylist** — `RunCommand` refuses to execute a configurable list of dangerous commands by default: `Remove-Item`, `Invoke-WebRequest`, `Invoke-RestMethod`, `Start-Process`, `New-Service`, `Set-ExecutionPolicy`, `curl`, `wget`, `bitsadmin`, `certutil`, and any command with `-Recurse -Force`. Substring patterns block .NET direct network access (`[System.Net.WebClient]`, `[System.Net.Http.HttpClient]`, `[System.Net.Sockets.TcpClient]`). Users can extend the denylist in `.rho/config.toml` via `[shell] denied_commands` and `[shell] denied_flag_combos`. The denylist is best-effort — the approval gate is the primary defense.
- **Working directory** — `RunCommand` executes within the sandbox root. Commands that attempt to `cd` outside the project directory are flagged.
- **Structured output** — stdout/stderr are captured and returned as structured data, never shell-interpolated back into command strings.

**Rust tooling (Phase 3):**
| Tool | Description |
|---|---|
| `CargoCheck` | Run `cargo check --message-format=json`, return parsed diagnostics |
| `CargoClippy` | Run `cargo clippy --message-format=json`, return parsed lints |
| `CargoTest` | Run `cargo test`, parse and return structured results |
| `CargoFix` | Apply machine-applicable compiler/clippy suggestions |
| `RustcExplain` | Run `rustc --explain <CODE>`, return the error explanation |
| `RustAnalyzer` | LSP client for hover, definitions, references, and completions |

The Rust tools parse structured JSON output rather than scraping text. This gives the model precise file, line, column, and suggested replacement — no guesswork.

`rho-tools` depends on `rho-core` and `rho-highlight` (for tree-sitter–aware file operations, from Phase 3) but not on `rho-tui` or `rho-ext`.

### `rho-highlight` — Tree-Sitter Syntax Awareness

Syntax highlighting and structural code awareness, powered by tree-sitter. **Introduced in Phase 3**, when the first real consumer (mapping diagnostic spans to AST nodes; node-splitting validation in `EditFile`) appears.

This is **not** just a display concern. Tree-sitter sits between `rho-core` and the tools/TUI because it serves two roles:

1. **Structural understanding** — Tools use tree-sitter to reason about code: find function boundaries, identify node types at a position, extract symbol names, determine edit-safe regions. The `EditFile` tool can validate that a replacement doesn't split a syntax node. The `CargoCheck` tool can map diagnostic spans to AST nodes.

2. **Rendering** — The TUI uses tree-sitter to syntax-highlight code blocks, diffs, and diagnostic context. This produces highlighted output (ANSI escape sequences or styled spans) that `rho-tui` renders directly.

- **Grammar management** — Compile and ship tree-sitter grammars for Rust (primary in Phase 3), with PowerShell, TOML, Markdown, and JSON evaluated for Phase 4.
- **Highlight queries** — Tree-sitter highlighting queries (`.scm` files) for each grammar, defining color classes.
- **Theme mapping** — Map tree-sitter highlight classes to concrete colors (ANSI, crossterm, or ratatui style). Phase 4.
- **Structural queries** — API for tools to query a syntax tree: "what node is at line X, column Y?", "what's the enclosing function?", "list all `fn` items in this file". Phase 3.
- **Incremental parsing** — Re-parse only changed regions for live editing scenarios. Phase 4 if useful.
- **Build dependency** — Tree-sitter grammars require a C compiler at build time. Document this in the project README so contributors know what to install.

`rho-highlight` depends on `rho-core` (for `FilePath` and error types) and on the `tree-sitter` + grammar crates.

### `rho-tui` — Terminal UI

The interactive terminal experience. Replaces the bare REPL.

- **Input** — Multi-line editor with history, completion, and paste support
- **Output rendering** — Markdown rendering, syntax-highlighted code blocks (via `rho-highlight`), diff views
- **Tool approval** — Display proposed tool calls and ask for user confirmation before execution
- **Streaming** — Render model output token-by-token as it arrives (SSE/streaming API). Tool output streaming uses the `ToolOutcome::Streamed` variant declared in Phase 1a.
- **Status display** — Show agent state (thinking, executing tool, waiting for input)
- **Diagnostic panel** — Render structured Rust diagnostics with clickable spans and syntax-highlighted context
- **Session navigation** — `/tree`, `/fork`, `/clone` slash commands for navigating the session tree

`rho-tui` depends on `rho-core` (for types and the agent loop interface) and `rho-highlight` (for syntax rendering). It does not depend on `rho-tools`.

### `rho-ext` — Extension API

Allows users to add custom tools and hooks without modifying the core.

- **Tool plugins** — Users define tools in a config file (TOML in Phase 5; Lua/WASM are deferred). Extension command templates use **structured argument substitution** — each argument is passed as a separate parameter to the command, never shell-interpolated. This is the difference between `Command::arg()` (safe) and `Command::new("/bin/sh -c ...")` (unsafe). Extension authors are responsible for their tools' safety; the framework prevents the most common injection vector.
- **Hooks** — Pre/post execution callbacks (e.g., log every tool call, block certain commands)
- **Configuration** — Per-project `.rho/` config: model, system prompt extensions, enabled tools, approval policies
- **Prompt templates** — User-defined system prompt fragments that get composed at startup

`rho-ext` depends on `rho-core` (for the `Tool` trait and registry). It does not depend on `rho-tui`.

### `rho-test-helpers` — Shared Test Utilities

A dev-only crate containing reusable test infrastructure shared across the workspace.

- **Mock `ChatClient`** — A `ChatClient` trait implementation that returns canned responses, used by integration tests in `rho-core` and beyond
- **Fixture loaders** — Helpers for loading JSON fixtures from `tests/fixtures/` directories
- **Tempdir helpers** — Create/verify/cleanup temporary directories for file-system tests
- **Trust-store helpers** — Per-test override for `~/.rho/trusted_projects.toml` so trust-flow tests are deterministic and isolated
- **PowerShell detection** — Shared helper for `pwsh` vs `powershell` availability (used by `rho-tools` tests). Returns `None` when neither is available, allowing tests to skip gracefully on systems without PowerShell.
- **Session helpers** — `in_memory_session()`, `path_messages_of()` for constructing and inspecting sessions in tests.

`rho-test-helpers` depends on `rho-core` (for the `ChatClient` trait and domain types). It is only included as a `dev-dependency` and never published.

### `rho-eval` — Behavioural Benchmark Suite

A standalone evaluation tool that runs the agent against fixed coding tasks and tracks success rate.

- **Task definitions** — 5 canonical coding tasks (fix type mismatch, unused imports, explain-and-fix, fix-and-test, multi-error fix) with known correct outcomes
- **Automated scoring** — Run each task, compare the agent's result against the expected outcome, produce a pass/fail report
- **Multi-model comparison** — `rho-bench` drives eval tasks against any OpenAI-compatible endpoint (local or remote) with timing and token metrics

`rho-eval` depends on `rho-core` (for the agent loop and types). It is a dev-only tool, not published.

### `rho` — Binary (Top-Level Assembly)

The entry point. Its only job is to wire the layers together:

1. Parse CLI arguments (model, system prompt, config path, `--session`, `--ephemeral`)
2. Select and instantiate the `ChatClient` provider based on config (default: `LocalChatClient`)
3. Scan sandbox root for project context files, verify trust, and compose the system prompt
4. Create the tool registry and register built-in tools from `rho-tools`
5. Load extensions from `rho-ext`
6. Construct a `Session` (persisted, resumed, or ephemeral based on CLI flags)
7. Start the agent loop from `rho-core`
8. Connect it to the TUI from `rho-tui`

---

## Development Phases

Each phase produces a runnable agent. No phase requires a rewrite of the previous one.

| Phase | Directory | Goal |
|---|---|---|
| 1a: The Agent Loop | [`phases/phase-1a-COMPLETE/`](phases/phase-1a-COMPLETE/) | Model invokes tools, agent loop runs autonomously. ✅ **Complete** |
| 1b: Security Surface | [`phases/phase-1b-COMPLETE/`](phases/phase-1b-COMPLETE/) | Approval gate, sandbox, context-file trust, redaction, untrusted-data framing. ✅ **Complete** |
| 2: PowerShell, File Tools, and Cross-Platform Support | [`phases/phase-2-COMPLETE/`](phases/phase-2-COMPLETE/) | PowerShell-native assistant, file system navigation, config loader, denylist, cross-platform (Windows/macOS/Linux). ✅ **Complete** |
| 2.5: Adaptive-Resolution Context | [`phases/phase-2.5-COMPLETE/`](phases/phase-2.5-COMPLETE/) | Session tree, resolution levels, calibrated budget, tool-result bounding, amnesia fix, JSONL persistence, extension entries, compaction strategy. ✅ **Complete** |
| 3: Rust Tooling and Tree-Sitter | [`phases/phase-3-COMPLETE/`](phases/phase-3-COMPLETE/) | `rho-highlight` crate, structured diagnostics, all Cargo tools, AST span mapping, `EditFile` validation, `rho-eval`, test suite audit, 5 prompt scenarios. ✅ **Complete** |
| 4: Terminal UI | [`phases/phase-4/`](phases/phase-4/) | Rich TUI with approval prompts, streaming, session navigation. 🔜 **Next** |
| 5: Extensions and Polish | [`phases/phase-5/`](phases/phase-5/) | Custom tools, config, prompt composition |
| 6: LSP (Future) | [`phases/phase-6/`](phases/phase-6/) | rust-analyzer LSP integration (deferred) |

Phase 1 was originally a single phase. It was split because the original scope packed the agent-loop machinery and the security surface (sandbox, approval, trust, redaction) into one milestone. Each deserved focused implementation and test coverage rather than being rushed alongside the other. Both Phase 1a and Phase 1b are now complete. Phase 2 is also complete, adding cross-platform support alongside the originally planned PowerShell and file tools. Phase 2.5 replaces the flat `Conversation` model with a tree-shaped `Session` that supports adaptive resolution, calibrated token budgets, bounded tool results (fixing the shipping amnesia bug), JSONL persistence, extension entries, and mechanical compaction. The agent loop now uses `Session` by default; `Conversation` is retained for backward compatibility.

Each phase directory contains:
- **`phase.md`** — goal, milestone, dependencies, decisions, and exit criteria
- **`tasks.md`** — ordered task list with details

### Testing Approach

All development follows test-driven development:

1. **Write the failing test first.** Before implementing a feature, write a test that defines the expected behavior. The test fails because the code doesn't exist yet.
2. **Make it pass.** Write the minimum implementation to satisfy the test.
3. **Refactor.** Clean up the implementation while keeping the test green.

At the end of each phase, the test suite is audited and refactored:
- Promote shared helpers out of individual test modules into reusable locations
- Extract inline fixtures (JSON blobs, file contents) into `tests/fixtures/` files
- Adopt consistent naming conventions
- Remove redundant tests
- Ensure the suite is fast, deterministic, and maintainable

This is not optional — the test suite is the safety net for every subsequent phase.

### Dependency Philosophy

Every external crate is a liability. Before adding a dependency:

1. **Can we write it ourselves?** If it's a few hundred lines of straightforward code, write it. We control it, we understand it, and it won't break on a minor version bump.
2. **Is it a foundation?** Async runtime, serialization, HTTP — these are non-negotiable infrastructure. Accept them.
3. **Is it a standard?** Tree-sitter grammars, LSP protocol types — these are domain standards with complex specs. Use the ecosystem crate.
4. **Is it a user interface?** Terminal rendering, input handling — these are deep specialties with edge cases across platforms. Use the ecosystem crate, but wrap it behind our own traits so we're not coupled to its API.

The dependency list for each phase reflects this analysis. Crates marked **(foundation)** are accepted without question. Crates marked **(evaluated)** were considered against the write-it-ourselves bar. Crates marked **(wrapped)** are used but abstracted behind our own traits.

**Accepted foundations:**

| Crate | Why it's non-negotiable |
|---|---|
| `tokio` | Async runtime — everything is async |
| `serde` + `serde_json` | Serialization — every API boundary speaks JSON |
| `reqwest` | HTTP client — the agent talks to model APIs over HTTP |
| `thiserror` | Error type derivation — boilerplate elimination with zero runtime cost |
| `anyhow` | Error propagation in application code — ergonomics for non-recoverable paths |
| `clap` | CLI parsing — the binary needs argument handling |
| `async-trait` | Required for `Box<dyn Tool>` and `Box<dyn ChatClient>`. Native AFIT (stable since 1.75) is not dyn-compatible; `trait-variant` solves the `Send` bound problem but does not provide dyn compatibility. The hand-rolled alternative is what `async-trait` already does, just without the macro |

**Phase 2.5 additions:**

| Crate | Why it's accepted |
|---|---|
| `uuid` | Entry IDs need to be stable across sessions and unique across processes. 8-char hex from a UUID is what pi uses; we do the same. |
| `tiktoken-rs` | (optional, feature-gated) Real tokenization for OpenAI-format models. Feature-gated so users on local-only models can opt out. |

---

## Security Model

A coding agent takes untrusted input (LLM output), interprets it as instructions, and executes those instructions with the full privileges of the user. The attack surface is real: a compromised or confused model can delete source code, exfiltrate secrets, or pivot to the network. The security model is defense-in-depth — no single layer is sufficient, but each layer raises the bar.

### Threat Model

| Threat | Vector | Primary defense | Secondary defense |
|---|---|---|---|
| Destructive command execution | Model generates dangerous shell commands | Command denylist | Approval gate |
| Data exfiltration via shell | Model runs `Invoke-WebRequest` with file contents | Approval gate | Command denylist (PowerShell cmdlets + .NET types + LOLBINs) + Egress allowlist |
| Data exfiltration via provider | Conversation (including file contents) sent to external API | Provider switch warning + consent | Egress allowlist |
| Path traversal | Model reads/writes files outside project | File sandbox (canonicalised paths, including not-yet-existing) | Approval gate |
| Secret exposure | Tool results contain API keys/tokens | Best-effort secret redaction | Approval gate |
| Prompt injection via file contents | File contains "ignore all instructions" | Untrusted-data framing (`<context>` wrapper) | Approval gate |
| Supply-chain prompt attack | Malicious `AGENTS.md` / `.rho/prompt.md` in cloned repo | Project context file trust (hash verification) | User confirmation on first load |
| Extension command injection | Model arguments escape command template | Structured argument substitution (`Command::arg()`) | Approval gate |
| Runaway tool-call loop | Model generates infinite tool calls | Max iteration guard | Retry budget |
| Credential at rest | API keys in plaintext config | Env var references (`api_key_env`) | Optional credential store integration |

### Security Principles

1. **Approval is an agent-loop concern, not a UI concern.** `rho-core` decides *whether* to execute; the UI decides *how* to ask.
2. **Default deny.** The agent starts locked down: sandbox on, denylist active, redaction enabled. Users opt out explicitly.
3. **Defense in depth.** Each threat has at least two layers of defense. The approval gate is the final backstop — even if every other defense fails, the user must confirm destructive actions.
4. **Transparency.** Tool call previews, provider switch warnings, and extension audit views ensure the user can see what the agent is about to do before it does it.
5. **Best-effort redaction, honest documentation.** Redaction reduces accidental secret exposure but does not guarantee its absence. The approval gate is the primary defense.

---

## Workspace Crate Summary

| Crate | Phase | Depends On | Purpose |
|---|---|---|---|
| `rho-core` | 1a | External only (`tokio`, `serde`, `reqwest`, `thiserror`, `anyhow`, `clap`, `async-trait`, `toml`, `sha2`, `regex`, `tracing`, `uuid`, `tokio-util` from Phase 1b/2/2.5) | Agent kernel: data model, types, traits, loop, registry, config, session tree |
| `rho-tools` | 1a–3 | `rho-core`, `rho-highlight` (from Phase 3) | Built-in tools: files, shell, Rust tooling |
| `rho-test-helpers` | 1a | `rho-core` | Shared test utilities: mock `ChatClient`, fixture loaders, tempdir helpers, trust-store overrides, session helpers |
| `rho-highlight` | 3 | `rho-core`, `tree-sitter`, `tree-sitter-rust` | Tree-sitter parsing, highlighting, and structural queries |
| `rho-eval` | 3 | `rho-core` | Behavioural benchmark suite |
| `rho-tui` | 4 | `rho-core`, `rho-highlight`, `crossterm` **(foundation)**, `ratatui` **(wrapped)**, `pulldown-cmark` **(wrapped)** | Terminal UI: rendering, input, approval |
| `rho-ext` | 5 | `rho-core` | Extension API and runtime |
| `rho` (binary) | 1a+ | All above | Top-level assembly and CLI |
| `xtask` | existing | External only | Dev task runner (unchanged) |

---

## Design Decisions to Revisit

These are choices that seem right now but may need adjustment as we build:

1. **Tool trait shape** — Resolved for now: async, dyn-compatible (`async-trait`), takes `CancellationToken`, returns `ToolOutcome` with immediate and streaming variants. The streaming variant is declared but not exercised until Phase 4. If real-world tools need a fundamentally different shape (e.g., long-running daemon tools), revisit.

2. **Approval model** — Phase 1b introduces `ApprovalPolicy` with a default that requires approval for destructive operations. Phase 4 enhances the UX (rich preview, single-keypress, batch approval). Per-tool config-driven policy lands in Phase 2. A trust-on-first-use model could be added later.

3. **Streaming** — ✅ Implemented (Pre-Work 1, `improvement-chat-streaming` branch): `ChatClient::chat_stream` returns `Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>>` with a default impl that wraps `chat`. `LocalChatClient` implements real SSE parsing with line buffering. `run_loop` always uses the streaming path. `StreamChunk` enum carries `TextDelta`, `ReasoningDelta`, `ToolCallDelta`, `Done`. `StreamChunk::accumulate()` reconstructs `AssistantResponse`. All 716 tests pass. `ToolOutcome::Streamed` remains for tool-side streaming in a future phase.

4. **Extension format** — TOML-defined command tools are the simplest starting point (Phase 5). WASM or Lua would allow more sophisticated extensions but add significant complexity.

5. **rust-analyzer integration** — LSP is deferred to Phase 6+. The protocol is complex (800–1200 lines, not the ~500 initially estimated). If pursued, accept `lsp-types` rather than hand-rolling.

6. **Multi-turn tool composition** — Resolved: `AssistantResponse` carries `Vec<ModelToolCall>` from Phase 1a. Phase 2 implements sequential execution of all tool calls. Parallel execution remains a future optimisation. Phase 2.5's session tree now supports parallel exploration via branching.

7. **Newtype vs. string** — Resolved: newtypes (`FilePath`, `ToolName`, `ToolCallId`, `EntryId`, etc.) implement `Deref<Target = _>` so they're ergonomic at API boundaries while preserving type safety internally.

8. **Tree-sitter grammar scope** — Starting with just Rust (Phase 3) is safe. Adding PowerShell, TOML, and Markdown grammars increases binary size and compile time. Feature-gating is defined from the start (`rust` as default, others opt-in).

9. **Data model evolution** — The data model will evolve as new tools and capabilities are added. Each phase should include an audit: are the existing types still ergonomic? Do any need to be split, merged, or promoted to newtypes?

10. **Test fixture management** — Resolved: fixtures use `<crate>/tests/fixtures/<category>/<name>.json` from Phase 1a. Generated from real API responses where possible. Shared helpers live in `rho-test-helpers`.

11. **When to write it ourselves vs. depend on a crate** — The bar for adding a dependency stays high. Each phase documents its dependency decisions. Revisit at the phase boundary: did we end up needing a crate we initially wrote ourselves? Did a crate we added turn out to be a thin wrapper?

12. **Grammar crate maturity** — Tree-sitter grammar crates vary widely in quality. `tree-sitter-rust` is mature. PowerShell, TOML, and Markdown grammars may be less so. Evaluate each grammar before committing in Phase 4.

13. **MCP (Model Context Protocol) compatibility** — The current `Tool` trait is custom. MCP has become the dominant standard for tool interoperability. The `Tool` trait schema format is JSON Schema–compatible (via `serde_json::Value`), which leaves the door open for an `rho-ext` MCP adapter layer. Revisit when the extension API is designed in Phase 5.

14. **Error recovery model** — The `AgentState` state machine (Phase 1a) treats errors and retries as transition outcomes rather than states. Tune the retry budget and backoff strategy based on real-world usage.

15. **Context window management strategy** — Resolved (Phase 2.5): replaced the flat `Vec<ChatMessage>` buffer with a tree-shaped `Session` using adaptive resolution. The `ContextManager` trait now exposes `fit_path`, which walks the leaf-to-root path, filters by resolution, renders compaction summaries, subtracts tool-schema and system-message overhead from the budget, and delegates to `fit`. `SlidingWindowContextManager` pins the system message *and the first user turn* and evicts by turn. Token budgets split into `context_window` and `completion_reserve` (default 4096). The `HeuristicEstimator` calibrates per-model chars-per-token ratios via exponential moving average against API ground truth. **Model-aware sizing** (querying the API for `max_context_length` and auto-sizing the budget) is a Phase 4 concern — it requires the TUI to display the resolved budget and the provider abstraction to expose model metadata.

16. **Cross-platform shell support** — Resolved (Phase 2, Task 14): `PowerShellExecutor` works cross-platform. PowerShell 7+ (`pwsh`) is the default on all platforms; `powershell` (Windows PowerShell 5.1) is the fallback on Windows only. Path normalization is platform-aware (Windows-only slash conversion). Process killing uses `taskkill` on Windows and `kill -9` on Unix. The `ShellExecutor` trait remains the seam for adding platform-specific shells (e.g., `BashExecutor` for native Unix workflows) without rewriting the tool layer. The system prompt instructs the model to use PowerShell on all platforms.

17. **Provider extensibility and crate boundary** — The `ChatClient` trait is the seam for plugging in model providers. Local models are the primary target and ship as the default `LocalChatClient`. External providers can be added by implementing the trait. **Open question:** providers may want to live in their own crates (e.g., `rho-providers-openai`) or behind feature flags so `rho-core` users (extension authors, embedders) don't pay for `reqwest` features they don't use. Revisit when the second provider is added — the current shape doesn't paint us into a corner either way.

18. **File sandbox scope** — The sandbox root defaults to the project directory. The boundary is enforced by canonicalising paths and checking they're within the root, including the not-yet-existing-path case (walk to the nearest existing ancestor, canonicalise, re-append, verify). Edge cases to revisit: system include paths (Rust stdlib sources for diagnostics), shared dependencies in a monorepo. A configurable `sandbox_paths` allowlist may be needed.

19. **Secret redaction completeness** — Best-effort defense, documented as such. Catches recognisable prefix-shaped secrets, misses everything else. The approval gate is the primary defense.

20. **Prompt injection defense** — Untrusted-data framing (`<context>` wrapper inside `User` messages) is a defense-in-depth measure, not a guarantee. Models vary in their ability to distinguish instructions from data inside framing. The approval gate is the primary defense — even if the model is tricked into generating a destructive command, the user must approve it before execution.

21. **Egress control** — An egress allowlist was implemented in Phase 2 but removed in v0.33.3 after review determined it provided a false sense of security: the model could bypass it via shell commands regardless. The approval gate and command denylist remain the primary defenses. The provider consent warning (informing the user that data will leave the machine when connecting to an external endpoint) was retained.

22. **Project context file scope** — The default scan list covers the most common ecosystem conventions, but the landscape evolves. Configurable in `.rho/config.toml`. Subdirectory scanning (e.g., `.claude/rules/`) is not done initially — each additional file is another supply-chain vector.

23. **Prompt composition precedence** — The base identity prompt is always first in the system prompt and cannot be overridden by project context files. Project context files extend the prompt but cannot rewrite it.

24. **Base prompt accessor shape** — `base_prompt()` is a function rather than a `const` so that runtime substitution (e.g., injecting the current OS, project name, or date) can be added later without an API break. v1 ships with no substitution. If `rho-eval` shows model-dependent regressions, per-model prompt variants can be added via a `base_prompt_for(model: &str) -> &'static str` overload. The base prompt is intentionally short: long prompts push relevant context out of the model's attention window, are harder to revise, and tempt the author to encode behaviours that belong in tool schemas or runtime checks. **Budget-aware prompt composition** (Phase 5, Task 6) measures the token cost of each prompt layer and warns when the system prompt consumes more than a configured fraction of the budget, ensuring sufficient room for conversation.

25. **Tool-call message persistence** — Resolved (Phase 1a, task 7): the assistant message containing `tool_calls` must be persisted into history before the matching `tool` result message is appended. The API rejects requests where a `tool` message is not preceded by an assistant message containing the matching `tool_call_id`. The fix has a regression test in Phase 1a. The invariant carries forward to `Session` — the agent loop appends assistant entries before tool-result entries.

26. **Tree-shaped session model** — Resolved (Phase 2.5): pi-inspired, simpler than git. Each entry has one parent, forming a tree. A single mutable leaf pointer identifies the current position. Context sent to the model is the leaf-to-root path, filtered by resolution. JSONL persistence is append-only at `~/.rho/sessions/<project-hash>/`. Entry IDs are 8-char hex from UUID v4 (explicit, not content-addressed). The leaf pointer is explicit, not implicit — persisted as a `LeafMoved` event for auditability.

27. **Adaptive resolution** — Resolved (Phase 2.5): entries carry explicit resolution (`Full`, `Compacted`, `Attached`). Compaction is a refinement operation, not a deletion — compacted entries stay in the tree at lower resolution, bypassed by `fit_path`. Tool results are bounded at append time (truncated to half the prompt budget, full content preserved in `ToolResultDetails::FullOutput`). This is the core fix for the shipping amnesia bug. Resolution transitions are one-way in Phase 2.5 (`Full → Compacted` or `Full → Attached`); reverse transitions may be supported later for branching scenarios.

28. **Calibrated token budget** — Resolved (Phase 2.5): per-model token estimation self-corrects against API ground truth via exponential moving average. Bootstrap defaults for known model families (Gemma 3.5, Qwen 2.5, Claude 3.7, GPT-4 4.0); unknown models use conservative 2.5. Tool-schema and system-message overhead subtracted from budget before path-fitting. Convergence within 10% by third calibration call. Calibrator persistence across sessions (`~/.rho/calibration.json`) is deferred but flagged for a future point release.

29. **Extension entry type safety** — Resolved (Phase 2.5): the `ExtensionEntry` and `ExtensionMessageEntry` traits provide typed, versioned read/write access to extension state. Kind strings follow the `"<author>.<feature>.v<n>"` convention. Schema version skew produces a clean `None` on read rather than panics or data corruption.
