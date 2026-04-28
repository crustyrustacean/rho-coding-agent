# rho-coding-agent — Roadmap

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

**Platform scope:** rho is a Windows-first, PowerShell-native agent. The shell, paths, and tooling assumptions target Windows. Unix support is welcome but not a priority. Shell execution is abstracted behind a `ShellExecutor` trait so cross-platform support can be added later without rewriting the tool layer.

Two auxiliary crates sit outside the main stack:
- **`rho-test-helpers`** — shared test utilities (mock `ChatClient`, fixture loaders, tempdir helpers). Available as a dev-dependency to every crate.
- **`rho-eval`** — behavioural benchmark suite for evaluating agent success rate on canonical coding tasks.

## Crate Responsibilities

### `rho-core` — Agent Kernel

The foundation. Defines the contract everything else implements.

- **Data model** — The domain types that everything else builds on. These are the most important types in the project: if they're easy to work with, the whole system flows; if they're awkward, every layer pays the price. Design goals:
  - **Constructible** — types should be easy to create in code (tests, tools, extensions) without boilerplate
  - **Composable** — conversations, tool results, and diagnostic spans should combine naturally (e.g., appending a tool result to a conversation should be one method call)
  - **Serializable** — every domain type round-trips through serde (JSON for the API, TOML for config)
  - **Documented** — every public type, field, and variant has a doc comment explaining its purpose and constraints
  - **Newtype where it matters** — `FilePath`, `ToolName`, `DiagnosticCode` etc. as distinct types rather than raw strings, so the type system prevents misuse
- **Domain types** — `ChatMessage`, `Role`, `ChatRequest`, `ModelResponse`, `FinishReason`, etc. (already exists, will be refined)
- **Tool trait** — `Tool`: the interface all tools implement (`name`, `description`, `parameters`, `execute`)
- **Tool registry** — Maps tool names to `dyn Tool` implementations. Tools are registered with a risk level (`Read`, `Write`, `Destructive`) that feeds into the approval policy.
- **Approval policy** — Every tool call passes through an `ApprovalPolicy` before execution. The default policy requires human confirmation for destructive operations (`WriteFile`, `EditFile`, `RunCommand`). The approval gate lives in `rho-core`, not the UI layer — the TUI just renders the prompt and collects the response. Even the bare REPL enforces this with a simple `y/n` prompt.
- **File sandbox** — File tools operate within a sandbox root (the project directory, or an explicit `--root` argument). `FilePath` canonicalises the path (resolving `..`, symlinks, junctions) and validates it's within the root. Users can opt out in config (`sandbox = false`), but the default is safe.
- **Secret redaction** — Tool results pass through a redaction layer before entering conversation history. Patterns for API keys (`sk-...`, `ghp_...`), tokens, and environment variable values are replaced with `[REDACTED]`. This prevents secrets from being sent to model APIs or persisted in conversation logs.
- **Command surface** — The set of operations the user can invoke beyond sending a message to the model. These are not model interactions — they are agent control actions (clear history, switch provider, list tools, etc.). `rho-core` defines the capabilities; the UI layer (bare REPL or TUI) handles parsing and dispatch. See the command table below for the full surface.
- **Agent loop** — The core cycle: send prompt → receive response → if tool call, execute and feed back → repeat until `Stop`. The loop is modelled as an `AgentLoopState` state machine with explicit states for `Idle`, `Thinking`, `AwaitingApproval`, `ExecutingTool`, `Error(RhoError)`, and `RetryPending`. Every tool call passes through the `ApprovalPolicy` before execution. Retry semantics are built in: retryable errors (rate limits, transient HTTP failures) trigger exponential backoff up to a configurable budget; fatal errors (auth failure, malformed schemas) terminate the loop immediately. A configurable max-iteration guard prevents infinite loops.
- **Conversation** — Message history management (already exists, will gain tool result handling). File contents enter the conversation under a `Role::Context` role, distinct from `Role::User`, signalling to the model that this is data, not instructions. This is a defense-in-depth measure against prompt injection via file contents.
- **Context window management** — A `ContextManager` that uses a sliding window algorithm to keep the conversation within the model's context limit. The system message is always pinned (never evicted). The window slides over the remaining message history, evicting the oldest messages first when capacity is exceeded. This preserves the agent's core instructions while gracefully handling long sessions.
- **Project context files** — The agent detects and loads project-level instruction files from the sandbox root. These are files like `AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, and `.rho/prompt.md` that contain project-specific instructions the user wants incorporated into the system prompt. The default scan list covers the common ecosystem conventions; the list is configurable in `.rho/config.toml`. All project context files go through the same trust model: hash verification on first load, user confirmation required, re-confirmation if the file changes. This is both an ergonomics feature (the agent respects existing project conventions) and a security concern (these files are project-local and carry supply-chain risk identical to `.rho/prompt.md`).
- **Provider abstraction** — A `ChatClient` trait that decouples the agent loop from any specific model provider. The trait defines the contract for sending chat completions: request in, response out. Local models are the primary target (LM Studio, Ollama, etc. over OpenAI-compatible endpoints), but the trait is designed so external providers (OpenAI, Anthropic, Google) can implement it without modifying `rho-core`. Each provider handles its own authentication, endpoint construction, and any request/response translation. The concrete `LocalChatClient` (talking to `localhost`) ships as the default. Tests use a mock implementation from `rho-test-helpers`.
- **Network egress** — An egress allowlist controls which hosts the agent is permitted to contact. `LocalChatClient` defaults to `localhost` only. External providers add their API hostname to the allowlist. When switching from local to an external provider, the user is warned that conversation contents (including file contents) will be sent to that provider.
- **Error types** — `RhoError` and `Result` (already exists)

**Command surface** — These operations must be supported by `rho-core` APIs. The UI layer decides how to expose them (slash commands in the REPL/TUI, keybindings, etc.), but `rho-core` provides the methods.

| Command | Agent operation | Required API |
|---|---|---|
| `/help` | List available commands | N/A (UI-only) |
| `/clear` | Reset conversation history (keep system message) | `Conversation::clear()` |
| `/history` | Show conversation history | `Conversation::messages()` |
| `/system` | Show or replace the system prompt | `Conversation::system_prompt()` getter/setter |
| `/model` | Show or switch the current model | `Conversation::set_model()`, config access |
| `/provider` | Show or switch the provider | `ChatClient` swap, config access |
| `/tools` | List registered tools | `ToolRegistry::list()` |
| `/context` | Show loaded project context files and their trust status | Context file scanner API |
| `/config` | Show current configuration | Config access |
| `/quit` | Exit the agent | N/A (UI-only) |

Phase 1 implements the APIs that `rho-core` owns (`Conversation::clear()`, `ToolRegistry::list()`, etc.). The bare REPL handles `/quit` and `/clear` minimally. Phase 4 (TUI) builds out full slash-command parsing, autocomplete, and rendering. Phase 5 (extensions) may allow custom commands via `rho-ext`.

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
| `ReadFile` | Read a file's contents (text or image) |
| `WriteFile` | Create or overwrite a file |
| `EditFile` | Apply targeted replacements to a file (exact-match, like pi's edit tool) |
| `ListDir` | List directory contents |

**Shell execution:**
| Tool | Description |
|---|---|
| `RunCommand` | Execute a PowerShell command, capture stdout/stderr and exit code |

This tool is PowerShell-first. The system prompt instructs the model to generate PowerShell commands. On Windows, `pwsh` is the default; `powershell` is the fallback. The tool normalizes path separators and handles execution policies.

**Security controls:**
- **Command denylist** — `RunCommand` refuses to execute a configurable list of dangerous commands by default: `Remove-Item`, `Invoke-WebRequest`, `Invoke-RestMethod`, `Start-Process`, `New-Service`, `Set-ExecutionPolicy`, and any command with `-Recurse -Force`. Users can extend the denylist in `.rho/config.toml`.
- **Working directory** — `RunCommand` executes within the sandbox root. Commands that attempt to `cd` outside the project directory are flagged.
- **Structured output** — stdout/stderr are captured and returned as structured data, never shell-interpolated back into command strings.

**Rust tooling:**
| Tool | Description |
|---|---|
| `CargoCheck` | Run `cargo check --message-format=json`, return parsed diagnostics |
| `CargoClippy` | Run `cargo clippy --message-format=json`, return parsed lints |
| `CargoTest` | Run `cargo test`, parse and return structured results |
| `CargoFix` | Apply machine-applicable compiler/clippy suggestions |
| `RustcExplain` | Run `rustc --explain <CODE>`, return the error explanation |
| `RustAnalyzer` | LSP client for hover, definitions, references, and completions |

The Rust tools parse structured JSON output rather than scraping text. This gives the model precise file, line, column, and suggested replacement — no guesswork.

`rho-tools` depends on `rho-core` and `rho-highlight` (for tree-sitter–aware file operations) but not on `rho-tui` or `rho-ext`.

### `rho-highlight` — Tree-Sitter Syntax Awareness

Syntax highlighting and structural code awareness, powered by tree-sitter.

This is **not** just a display concern. Tree-sitter sits between `rho-core` and the tools/TUI because it serves two roles:

1. **Structural understanding** — Tools use tree-sitter to reason about code: find function boundaries, identify node types at a position, extract symbol names, determine edit-safe regions. The `EditFile` tool can validate that a replacement doesn't split a syntax node. The `CargoCheck` tool can map diagnostic spans to AST nodes.

2. **Rendering** — The TUI uses tree-sitter to syntax-highlight code blocks, diffs, and diagnostic context. This produces highlighted output (ANSI escape sequences or styled spans) that `rho-tui` renders directly.

- **Grammar management** — Compile and ship tree-sitter grammars for Rust (primary), PowerShell, TOML, Markdown, and JSON
- **Highlight queries** — Tree-sitter highlighting queries (`.scm` files) for each grammar, defining color classes
- **Theme mapping** — Map tree-sitter highlight classes to concrete colors (ANSI, crossterm, or ratatui style)
- **Structural queries** — API for tools to query a syntax tree: "what node is at line X, column Y?", "what's the enclosing function?", "list all `fn` items in this file"
- **Incremental parsing** — Re-parse only changed regions for live editing scenarios (useful when the TUI shows a live preview)

`rho-highlight` depends on `rho-core` (for the `FilePath` type and error types) and on the `tree-sitter` + grammar crates.

### `rho-tui` — Terminal UI

The interactive terminal experience. Replaces the current bare REPL.

- **Input** — Multi-line editor with history, completion, and paste support
- **Output rendering** — Markdown rendering, syntax-highlighted code blocks (via `rho-highlight`), diff views
- **Tool approval** — Display proposed tool calls and ask for user confirmation before execution
- **Streaming** — Render model output token-by-token as it arrives (SSE/streaming API)
- **Status display** — Show agent state (thinking, executing tool, waiting for input)
- **Diagnostic panel** — Render structured Rust diagnostics with clickable spans and syntax-highlighted context

`rho-tui` depends on `rho-core` (for types and the agent loop interface) and `rho-highlight` (for syntax rendering). It does not depend on `rho-tools`.

### `rho-ext` — Extension API

Allows users to add custom tools and hooks without modifying the core.

- **Tool plugins** — Users define tools in a config file (Lua, TOML, or WASM — TBD) that get registered in the tool registry at startup. Extension command templates use **structured argument substitution** — each argument is passed as a separate parameter to the command, never shell-interpolated. This is the difference between `Command::arg()` (safe) and `Command::new("/bin/sh -c ...")` (unsafe). Extension authors are responsible for their tools' safety; the framework prevents the most common injection vector.
- **Hooks** — Pre/post execution callbacks (e.g., log every tool call, block certain commands)
- **Configuration** — Per-project `.rho/` config: model, system prompt extensions, enabled tools, approval policies
- **Prompt templates** — User-defined system prompt fragments that get composed at startup
- **Project prompt trust** — `.rho/prompt.md` and other project context files (`AGENTS.md`, `.cursorrules`, etc.) require user confirmation on first load. The file's hash is stored in `~/.rho/trusted_projects.toml`. If any file changes, re-confirmation is required. This prevents a supply-chain attack where a cloned repository contains a malicious prompt that instructs the model to exfiltrate data or execute destructive commands.

`rho-ext` depends on `rho-core` (for the `Tool` trait and registry). It does not depend on `rho-tui`.

### `rho-test-helpers` — Shared Test Utilities

A dev-only crate containing reusable test infrastructure shared across the workspace.

- **Mock `ChatClient`** — A `ChatClient` trait implementation that returns canned responses, used by integration tests in `rho-core` and beyond
- **Fixture loaders** — Helpers for loading JSON fixtures from `tests/fixtures/` directories
- **Tempdir helpers** — Create/verify/cleanup temporary directories for file-system tests
- **PowerShell detection** — Shared helper for `pwsh` vs `powershell` availability (used by `rho-tools` tests)

`rho-test-helpers` depends on `rho-core` (for the `ChatClient` trait and domain types). It is only included as a `dev-dependency` and never published.

### `rho-eval` — Behavioural Benchmark Suite

A standalone evaluation tool that runs the agent against fixed coding tasks and tracks success rate.

- **Task definitions** — 10–20 canonical tasks (fix this compile error, refactor this function, add this test) with known correct outcomes
- **Automated scoring** — Run each task, compare the agent's result against the expected outcome, produce a pass/fail report
- **Regression tracking** — Compare success rates across phases to ensure new features don't regress existing behaviour

`rho-eval` depends on `rho-core` (for the agent loop and types). It is a dev-only tool, not published.

### `rho` — Binary (Top-Level Assembly)

The entry point. Its only job is to wire the layers together:

1. Parse CLI arguments (model, system prompt, config path)
2. Select and instantiate the `ChatClient` provider based on config (default: `LocalChatClient`)
3. Scan sandbox root for project context files (`AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`), verify trust, and compose the system prompt
4. Create the tool registry and register built-in tools from `rho-tools`
5. Load extensions from `rho-ext`
6. Start the agent loop from `rho-core`
7. Connect it to the TUI from `rho-tui`

---

## Development Phases

Each phase produces a runnable agent. No phase requires a rewrite of the previous one.

| Phase | Directory | Goal |
|---|---|---|
| 1: The Agent Loop | [`phases/phase-1/`](phases/phase-1/) | Model invokes tools, agent loop runs autonomously |
| 2: PowerShell and File Tools | [`phases/phase-2/`](phases/phase-2/) | PowerShell-native assistant, file system navigation |
| 3: Rust Tooling | [`phases/phase-3/`](phases/phase-3/) | Structured compiler diagnostics, Cargo integration |
| 4: Terminal UI | [`phases/phase-4/`](phases/phase-4/) | Rich TUI with approval prompts and streaming |
| 5: Extensions and Polish | [`phases/phase-5/`](phases/phase-5/) | Custom tools, config, prompt composition |
| 6: LSP (Future) | [`phases/phase-6/`](phases/phase-6/) | rust-analyzer LSP integration (deferred) |

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

**Accepted foundations (all phases):**

| Crate | Why it's non-negotiable |
|---|---|
| `tokio` | Async runtime — everything is async |
| `serde` + `serde_json` | Serialization — every API boundary speaks JSON |
| `reqwest` | HTTP client — the agent talks to model APIs over HTTP |
| `thiserror` | Error type derivation — boilerplate elimination with zero runtime cost |
| `anyhow` | Error propagation in application code — ergonomics for non-recoverable paths |
| `clap` | CLI parsing — the binary needs argument handling |

---

## Security Model

A coding agent takes untrusted input (LLM output), interprets it as instructions, and executes those instructions with the full privileges of the user. The attack surface is real: a compromised or confused model can delete source code, exfiltrate secrets, or pivot to the network. The security model is defense-in-depth — no single layer is sufficient, but each layer raises the bar.

### Threat Model

| Threat | Vector | Primary defense | Secondary defense |
|---|---|---|---|
| Destructive command execution | Model generates dangerous shell commands | Command denylist | Approval gate |
| Data exfiltration via shell | Model runs `Invoke-WebRequest` with file contents | Command denylist (network cmdlets) | Egress allowlist |
| Data exfiltration via provider | Conversation (including file contents) sent to external API | Provider switch warning + consent | Egress allowlist |
| Path traversal | Model reads/writes files outside project | File sandbox (canonicalised paths) | Approval gate |
| Secret exposure | Tool results contain API keys/tokens | Secret redaction layer | `Role::Context` separation |
| Prompt injection via file contents | File contains "ignore all instructions" | `Role::Context` role | Approval gate |
| Supply-chain prompt attack | Malicious `AGENTS.md` / `.rho/prompt.md` in cloned repo | Project prompt trust (hash verification) | User confirmation on first load |
| Extension command injection | Model arguments escape command template | Structured argument substitution (`Command::arg()`) | Approval gate |
| Runaway tool-call loop | Model generates infinite tool calls | Max iteration guard | Retry budget |
| Credential at rest | API keys in plaintext config | Env var references (`api_key_env`) | Optional credential store integration |

### Security Principles

1. **Approval is an agent-loop concern, not a UI concern.** `rho-core` decides *whether* to execute; the UI decides *how* to ask. Even the bare REPL enforces approval for destructive operations.
2. **Default deny.** The agent starts locked down: sandbox on, denylist active, redaction enabled, egress restricted. Users opt out explicitly.
3. **Defense in depth.** Each threat has at least two layers of defense. The approval gate is the final backstop — even if every other defense fails, the user must confirm destructive actions.
4. **Transparency.** Tool call previews, provider switch warnings, and extension audit views ensure the user can see what the agent is about to do before it does it.
5. **Secrets never leave.** Redaction prevents secrets from entering the conversation (and thus the model API or logs). Credential storage uses env vars, not plaintext config.

---

## Workspace Crate Summary

| Crate | Phase | Depends On | Purpose |
|---|---|---|---|
| `rho-core` | 1 | External only (`tokio`, `serde`, `reqwest`, `thiserror`, `anyhow`, `clap`, `toml`) | Agent kernel: data model, types, traits, loop, registry, config |
| `rho-highlight` | 1 | `rho-core`, `tree-sitter`, `tree-sitter-rust` | Tree-sitter parsing, highlighting, and structural queries |
| `rho-tools` | 1–3 | `rho-core`, `rho-highlight` | Built-in tools: files, shell, Rust tooling |
| `rho-tui` | 4 | `rho-core`, `rho-highlight`, `crossterm` **(foundation)**, `ratatui` **(wrapped)**, `pulldown-cmark` **(wrapped)** | Terminal UI: rendering, input, approval |
| `rho-ext` | 5 | `rho-core` | Extension API and runtime |
| `rho-test-helpers` | 1 | `rho-core` | Shared test utilities: mock `ChatClient`, fixture loaders, tempdir helpers |
| `rho-eval` | 3 | `rho-core` | Behavioural benchmark suite |
| `rho` (binary) | 1+ | All above | Top-level assembly and CLI |
| `xtask` | existing | External only | Dev task runner (unchanged) |

---

## Design Decisions to Revisit

These are choices that seem right now but may need adjustment as we build:

1. **Tool trait shape** — The initial `Tool` trait is async and takes/returns `serde_json::Value`. This may need to support streaming results (e.g., for long-running commands) or typed parameters. A `timeout: Option<Duration>` may be added to the registry invocation to cap long-running tools.

2. **Approval model** — Phase 1 introduces `ApprovalPolicy` with a default that requires approval for destructive operations even in the bare REPL. Phase 4 enhances the UX (rich preview, single-keypress, batch approval). The policy is per-tool, not just read/write — `RunCommand` is always high-risk, `ReadFile` is low-risk, `EditFile`/`WriteFile` are medium-risk. Policies are loaded from config (Phase 2) and can be tuned per-project. A trust-on-first-use model (auto-approve after N successful calls) could be added later.

3. **Streaming** — The current `ChatClient` trait does a full request/response via `chat()`. Phase 4 introduces streaming for the TUI, and the trait will likely gain a `chat_stream()` method. Providers that don't support streaming can emulate it by returning the full response as a single chunk. The trait design must not break existing providers when streaming is added.

4. **Extension format** — TOML-defined command tools are the simplest starting point. WASM or Lua would allow more sophisticated extensions but add significant complexity.

5. **rust-analyzer integration** — LSP is deferred to Phase 6+. The protocol is complex (800–1200 lines, not the ~500 initially estimated). If pursued, accept `lsp-types` rather than hand-rolling — the protocol surface area is too large to reimplement safely.

6. **Multi-turn tool composition** — Resolved: `AssistantResponse` carries `Vec<ModelToolCall>` from Phase 1. Phase 2 implements sequential execution of all tool calls. Parallel execution remains a future optimisation.

7. **Newtype vs. string** — Resolved: newtypes (`FilePath`, `ToolName`, etc.) implement `Deref<Target = _>` so they're ergonomic at API boundaries while preserving type safety internally. Raw strings at the API deserialization boundary, `From` impls to convert. Documented in the crate-level doc comment.

8. **Tree-sitter grammar scope** — Starting with just Rust is safe. Adding PowerShell, TOML, and Markdown grammars increases binary size and compile time. Feature-gating is defined from Phase 1 (`rust` as default, `powershell`, `toml`, `json`, `markdown` as opt-in) so the retrofit is painless.

9. **Data model evolution** — The data model will evolve as new tools and capabilities are added. Each phase should include an audit: are the existing types still ergonomic? Do any need to be split, merged, or promoted to newtypes? This is not a one-time design — it's an ongoing practice.

10. **Test fixture management** — Resolved: fixtures use `<crate>/tests/fixtures/<category>/<name>.json` from Phase 1. Generated from real API responses where possible. Shared helpers live in `rho-test-helpers`.

11. **When to write it ourselves vs. depend on a crate** — The bar for adding a dependency should stay high. Each phase documents its dependency decisions. Revisit these at the phase boundary: did we end up needing a crate we initially wrote ourselves? Did a crate we added turn out to be a thin wrapper we could replace? The audit is part of the phase retrospective.

12. **Grammar crate maturity** — Tree-sitter grammar crates vary widely in quality. `tree-sitter-rust` is mature and well-maintained. PowerShell, TOML, and Markdown grammars may be less so. Evaluate each grammar before committing: does it parse real-world files correctly? Is it actively maintained? If not, regex-based highlighting is an acceptable fallback — it's better than a broken grammar.

13. **MCP (Model Context Protocol) compatibility** — The current `Tool` trait is entirely custom. MCP has become the dominant standard for tool interoperability. The `Tool` trait schema format is JSON Schema–compatible (via `serde_json::Value`), which leaves the door open. If the ecosystem converges on MCP, `rho-ext` could provide an MCP adapter layer. This should be revisited when the extension API is designed in Phase 5 — at minimum, ensure the tool schema format doesn't paint us into a corner.

14. **Error recovery model** — The `AgentLoopState` state machine (Phase 1) distinguishes retryable from fatal errors. The retry budget and backoff strategy should be tuned based on real-world usage. If agents frequently get stuck in retry loops on particular error types, the model may need adjustment. The state machine makes this easy to iterate on.

15. **Context window management strategy** — The initial `ContextManager` (Phase 1) uses a sliding window with system message pinning. This is the baseline. More sophisticated strategies — summarisation (ask the model to compress earlier turns), importance scoring within the window, or retrieval-augmented context — should be evaluated as conversation lengths grow. The `ContextManager` trait boundary makes swapping strategies possible without changing the agent loop. The pinning guarantee (system message is never evicted) must be preserved by any future strategy — the agent's core instructions are not optional context.

16. **Cross-platform shell support** — Shell execution is abstracted behind `ShellExecutor` (Phase 2) with `PowerShellExecutor` as the first implementation. If Unix support becomes a priority, a `BashExecutor` can be added without rewriting the tool layer. The system prompt composition would also need per-platform shell guidance. The command denylist is shell-specific — each `ShellExecutor` implementation defines its own dangerous commands.

17. **Provider extensibility** — The `ChatClient` trait is the seam for plugging in model providers. Local models (LM Studio, Ollama) are the primary target and ship as the default `LocalChatClient`. External providers (OpenAI, Anthropic, Google) can be added by implementing the trait — each provider handles its own authentication, endpoint construction, and any request/response translation. Providers should live in their own crates or behind feature flags to avoid pulling in unnecessary dependencies (e.g., an `OpenAI` provider shouldn't require `reqwest` features that `LocalChatClient` doesn't need). The config system (Phase 2) selects the provider; the agent loop is oblivious to the choice.

18. **File sandbox scope** — The sandbox root defaults to the project directory. The boundary is enforced by canonicalising paths and checking they're within the root. Edge cases to revisit: what happens when the model needs to read from a system include path (e.g., Rust stdlib sources for diagnostics)? What about shared dependencies in a monorepo? A configurable `sandbox_paths` allowlist (in addition to the root) may be needed.

19. **Secret redaction completeness** — The redaction layer uses pattern matching on tool results. This is a best-effort defense — it catches known secret formats (OpenAI keys, GitHub tokens, Slack tokens) but cannot catch arbitrary secrets (internal API keys with unknown formats). Users can add custom patterns in config. The redaction is applied at the `rho-core` boundary before secrets enter the conversation, but secrets that are already in the model's training data or that the model generates independently cannot be redacted.

20. **Prompt injection defense** — `Role::Context` is a defense-in-depth measure, not a guarantee. Models vary in their ability to distinguish instructions from data in different roles. Local models may not respect role boundaries at all. The approval gate is the primary defense — even if the model is tricked into generating a destructive command, the user must approve it before execution. The system prompt should explicitly instruct the model to treat `Role::Context` as untrusted data, but this is a prompt-level defense, not a cryptographic one.

21. **Egress control granularity** — The egress allowlist controls which hosts the `ChatClient` can contact. `RunCommand` with PowerShell networking (`Invoke-WebRequest`) is a separate egress path. The command denylist blocks the most common networking cmdlets, but a determined model could construct network requests using .NET APIs directly (`[System.Net.WebClient]::new()`). Full egress control would require OS-level network filtering, which is out of scope for the agent itself. The denylist + approval gate is the pragmatic balance.

22. **Project context file scope** — The default scan list (`AGENTS.md`, `.agents.md`, `CLAUDE.md`, `.cursorrules`, `.rho/prompt.md`) covers the most common ecosystem conventions, but the landscape evolves. The list is fully configurable in `.rho/config.toml`. A future consideration: should the agent also scan subdirectories (e.g., `.claude/rules/`, `.cursor/rules/`)? This adds complexity and increases the attack surface — each additional file is another supply-chain vector. For now, only the sandbox root is scanned. Subdirectory scanning can be added if users request it, with the same trust model applied.

23. **Prompt composition precedence** — The base identity prompt is always first in the system prompt and cannot be overridden by project context files. This ensures the agent's core safety instructions (e.g., "treat `Role::Context` as data", "wait for approval on destructive operations") are never displaced. Project context files extend the prompt but cannot rewrite it. If a context file contains instructions that conflict with the base prompt, the base prompt wins — the model sees both, but the base prompt's positioning (first) gives it primacy.
