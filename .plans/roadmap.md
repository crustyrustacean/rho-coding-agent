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
```

**Dependency rule:** a crate may only depend on crates below it in the stack. `rho-core` depends on nothing but external libraries. `rho-tools` depends on `rho-core`. And so on.

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
- **Tool registry** — Maps tool names to `dyn Tool` implementations
- **Agent loop** — The core cycle: send prompt → receive response → if tool call, execute and feed back → repeat until `Stop`
- **Conversation** — Message history management (already exists, will gain tool result handling)
- **HTTP client** — Abstracted behind a trait so the agent loop is testable without a live server
- **Error types** — `RhoError` and `Result` (already exists)

`rho-core` does **not** know about:
- PowerShell, shells, or any specific command execution
- File systems
- Terminal rendering
- Extensions or plugins

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

- **Tool plugins** — Users define tools in a config file (Lua, TOML, or WASM — TBD) that get registered in the tool registry at startup
- **Hooks** — Pre/post execution callbacks (e.g., log every tool call, block certain commands)
- **Configuration** — Per-project `.rho/` config: model, system prompt extensions, enabled tools, approval policies
- **Prompt templates** — User-defined system prompt fragments that get composed at startup

`rho-ext` depends on `rho-core` (for the `Tool` trait and registry). It does not depend on `rho-tui`.

### `rho` — Binary (Top-Level Assembly)

The entry point. Its only job is to wire the layers together:

1. Parse CLI arguments (model, system prompt, config path)
2. Create the tool registry and register built-in tools from `rho-tools`
3. Load extensions from `rho-ext`
4. Start the agent loop from `rho-core`
5. Connect it to the TUI from `rho-tui`

---

## Development Phases

Each phase produces a runnable agent. No phase requires a rewrite of the previous one.

### Phase 1: The Agent Loop

**Goal:** The model can invoke tools and receive structured results. The agent runs autonomously until it's done or needs user input.

**Milestone:** `rho` reads a file when asked, instead of just saying "I would read the file."

**Tasks:**

1. Refine the data model in `rho-core`:
   - Audit existing types for ergonomics — can they be constructed without boilerplate? Are they composable?
   - Introduce newtypes where they prevent misuse: `FilePath`, `ToolName`, `DiagnosticCode`
   - Ensure every type round-trips through serde (JSON for API, TOML for future config)
   - Add builder-style or `From` impls for common construction patterns
   - Document the design philosophy in the crate-level doc comment

2. Define the `Tool` trait in `rho-core`:
   ```rust
   pub trait Tool: Send + Sync {
       fn name(&self) -> ToolName;
       fn description(&self) -> &str;
       fn parameters_schema(&self) -> serde_json::Value;
       fn execute(&self, arguments: serde_json::Value) -> Result<ToolResult>;
   }
   ```

3. Define `ToolResult` and `ToolRegistry` in `rho-core`.

4. Implement the agent loop in `rho-core`:
   - `Conversation::send` returns `AssistantResponse::ToolCall`
   - The loop looks up the tool in the registry, executes it, appends a `Role::Tool` message with the result, and sends again
   - Configurable max iterations to prevent infinite loops
   - Returns when `FinishReason::Stop` or user input is needed

5. Abstract the HTTP client behind a `ChatClient` trait for testability.

6. Create the `rho-tools` crate. Implement three minimal tools:
   - `ReadFile` — read a file's text content
   - `WriteFile` — write content to a file
   - `RunCommand` — execute a PowerShell command

7. Create the `rho-highlight` crate with initial scaffolding:
   - Add `tree-sitter` and `tree-sitter-rust` as dependencies
   - Implement a `parse` function that takes source text and returns a tree-sitter `Tree`
   - Implement a `highlight` function that produces ANSI-highlighted output for Rust source
   - This is minimal — just enough to validate the integration path. Richer grammars and queries come in Phase 4.

8. Update the binary to register tools and run the agent loop.

9. Add integration tests: mock `ChatClient` returns tool calls, verify the loop executes and feeds back.

**Exit criteria:** The agent can read, write, and execute commands when the model requests it. The loop terminates correctly on `Stop`. `rho-highlight` can parse and highlight a Rust source file.

---

### Phase 2: PowerShell and File Tools

**Goal:** The agent is a credible PowerShell-native assistant that can navigate and manipulate the file system.

**Milestone:** The agent can be asked "list the Rust source files in this project and tell me what each does" and it works.

**Tasks:**

1. Expand `RunCommand`:
   - Detect `pwsh` vs `powershell` availability
   - Set appropriate execution policy flags
   - Normalize path separators in arguments
   - Capture and return structured output (stdout, stderr, exit code)
   - Timeout support

2. Add `ListDir` tool (recursive directory listing with `.gitignore` awareness).

3. Add `EditFile` tool:
   - Exact-match replacement (old text → new text)
   - Non-overlapping edits in a single call
   - Validation: refuse if old text is not found or is ambiguous
   - Tree-sitter validation (via `rho-highlight`): warn if a replacement would split a syntax node (e.g., replacing half a string literal)

4. Compose a PowerShell-aware system prompt:
   - "You are running on Windows. Use PowerShell commands."
   - Common PowerShell idioms for file operations, process management, etc.
   - Few-shot examples of correct PowerShell usage

5. Add the `ChatRequest.tools` serialization so tool definitions are sent to the model API.

6. Add deserialization tests for tool-call responses (JSON fixtures with `finish_reason: "tool_calls"`).

**Exit criteria:** The agent reliably uses PowerShell commands, reads and edits files, and the model generates syntactically valid PowerShell.

---

### Phase 3: Rust Tooling

**Goal:** The agent understands Rust compilation errors and Clippy lints as structured data, not text. It can fix code using the compiler's own suggestions.

**Milestone:** The agent runs `cargo check`, parses the JSON diagnostics, applies the machine-applicable fix, and verifies the fix compiles.

**Tasks:**

1. Add `CargoCheck` tool:
   - Run `cargo check --message-format=json`
   - Parse the NDJSON stream into structured `Diagnostic` types
   - Return: error code, message, file, line, column, suggested replacements
   - Filter to the relevant crate/project (not dependency noise)

2. Add `CargoClippy` tool:
   - Same as `CargoCheck` but with `cargo clippy --message-format=json`
   - Include lint name and severity

3. Add `RustcExplain` tool:
   - Run `rustc --explain E0XXX`
   - Return the formatted explanation text

4. Add `CargoTest` tool:
   - Run `cargo test --message-format=json`
   - Parse test results: which passed, which failed, failure output

5. Add `CargoFix` tool:
   - Run `cargo fix --allow-dirty` for machine-applicable suggestions
   - Or: apply individual `MachineApplicable` suggestions from check/clippy output directly (more surgical)

6. Define `rho-tools::rust` types — these are part of the data model and should be designed with the same care as `rho-core` types:
   - `Diagnostic` — a structured compiler diagnostic
   - `DiagnosticSpan` — file, line range, column range
   - `DiagnosticSuggestion` — suggested replacement text for a span
   - `TestResult` — pass/fail with output
   - All types round-trip through serde, use newtypes where appropriate

7. Use `rho-highlight` to map diagnostic spans to AST nodes:
   - When a diagnostic points to a span, query the tree-sitter tree for the enclosing syntax node
   - Include the enclosing node type in the tool result (e.g., "this error is inside a `fn` item")
   - This gives the model richer context than raw line/column numbers

8. Compose a Rust-aware system prompt extension:
   - "You have access to structured Rust compiler diagnostics."
   - "When code fails to compile, use CargoCheck before attempting fixes."
   - "Trust machine-applicable suggestions from the compiler."

9. Add a `CargoCheck` → `EditFile` → `CargoCheck` integration test loop.

**Exit criteria:** The agent can diagnose and fix compilation errors using structured compiler output. It prefers compiler suggestions over its own guesses.

---

### Phase 4: Terminal UI

**Goal:** Replace the bare REPL with a rich, interactive terminal experience.

**Milestone:** The agent displays in a split-pane TUI with syntax-highlighted output, tool call previews, and approval prompts.

**Tasks:**

1. Create the `rho-tui` crate with `ratatui` + `crossterm`.

2. Implement the input pane:
   - Multi-line editor (Shift+Enter for newline, Enter to submit)
   - Command history (up/down arrows)
   - Autocomplete for tool names and file paths

3. Implement the output pane:
   - Markdown rendering (headers, bold, code blocks)
   - Syntax-highlighted code blocks via `rho-highlight` (supports Rust, PowerShell, TOML, JSON, Markdown)
   - Diff view for file edits (show old → new, both syntax-highlighted)
   - Inline diagnostic context with syntax-highlighted source lines
   - Incremental re-rendering: as streaming tokens arrive, only re-parse the changed region

4. Implement tool call display:
   - Show tool name and arguments before execution
   - Show tool result after execution (collapsible)
   - Ask for user approval on destructive operations (WriteFile, EditFile, RunCommand)
   - Allow/deny/skip approval with a single keypress

5. Implement streaming output:
   - Switch from `POST and wait for full response` to SSE/streaming API
   - Render tokens as they arrive
   - Show "thinking" indicator while waiting

6. Implement a diagnostic panel:
   - Render structured `Diagnostic` objects with file/line context
   - Color-code severity (error = red, warning = yellow)
   - Show suggested fix inline

7. Status bar:
   - Current model, conversation turn count, agent state (idle / thinking / executing)

**Exit criteria:** The agent is usable as a daily terminal tool. The REPL feels responsive, informative, and safe (approval on destructive actions).

---

### Phase 5: Extensions and Polish

**Goal:** The agent is configurable and extensible. Users can add tools, customize prompts, and integrate their own workflows.

**Milestone:** A user can add a custom tool via config, restart the agent, and the model can use it.

**Tasks:**

1. Create the `rho-ext` crate.

2. Define the extension API:
   - `ExtTool` trait — user-defined tools with name, description, schema, and execute
   - Initial implementation: tools defined in TOML config files (command + args template)
   - Example: a `DockerRun` tool that runs `docker run` with the provided arguments

3. Project-level configuration (`.rho/config.toml`):
   - Model selection and system prompt extensions
   - Enabled/disabled tools
   - Approval policies (auto-approve reads, require approval for writes)
   - Custom tool definitions

4. Global configuration (`~/.rho/config.toml`):
   - Default model
   - API endpoint
   - Theme preferences
   - Keybindings

5. Prompt composition:
   - Base identity prompt
   + PowerShell guidance
   + Rust tooling guidance
   + Project-specific `.rho/prompt.md` (if present)
   + Tool schemas (auto-generated from the registry)

6. `rust-analyzer` LSP integration (stretch goal for this phase):
   - `RustAnalyzer` tool that communicates via LSP protocol
   - Hover (type info), go-to-definition, find-references
   - Requires running `rust-analyzer` as a background process

7. Polish:
   - Comprehensive error messages (no panics, all errors surfaced)
   - Logging (file-based, for debugging)
   - Performance profiling (tool execution times, token usage tracking)

**Exit criteria:** The agent is configurable, extensible, and production-ready for daily Rust development on Windows.

---

## Workspace Crate Summary

| Crate | Phase | Depends On | Purpose |
|---|---|---|---|
| `rho-core` | 1 | External only | Agent kernel: data model, types, traits, loop, registry |
| `rho-highlight` | 1 | `rho-core`, `tree-sitter` | Tree-sitter parsing, highlighting, and structural queries |
| `rho-tools` | 1–3 | `rho-core`, `rho-highlight` | Built-in tools: files, shell, Rust tooling |
| `rho-tui` | 4 | `rho-core`, `rho-highlight` | Terminal UI: rendering, input, approval |
| `rho-ext` | 5 | `rho-core` | Extension API and runtime |
| `rho` (binary) | 1+ | All above | Top-level assembly and CLI |
| `xtask` | existing | External only | Dev task runner (unchanged) |

---

## Design Decisions to Revisit

These are choices that seem right now but may need adjustment as we build:

1. **Tool trait shape** — The initial `Tool` trait is async and takes/returns `serde_json::Value`. This may need to support streaming results (e.g., for long-running commands) or typed parameters.

2. **Approval model** — Phase 4 assumes a blocking "approve/deny" prompt. A more sophisticated model might allow auto-approval for reads, batch approval, or a trust-on-first-use model.

3. **Streaming** — The current API does a full request/response. Phase 4 introduces streaming for the TUI, but the `ChatClient` trait in `rho-core` needs to support both modes.

4. **Extension format** — TOML-defined command tools are the simplest starting point. WASM or Lua would allow more sophisticated extensions but add significant complexity.

5. **rust-analyzer integration** — LSP is powerful but adds a long-lived process and complex message protocol. May be better as a Phase 6 or later, once the tool trait supports persistent backends.

6. **Multi-turn tool composition** — The agent loop in Phase 1 handles one tool call per model turn. Some models return multiple tool calls in a single response. The loop should handle `Vec<ModelToolCall>`, executing them in sequence or parallel.

7. **Newtype vs. string** — Introducing newtypes like `FilePath` and `ToolName` improves type safety but adds conversion overhead at API boundaries (the model sends raw strings). Decide how much newtyping is worth it — probably: newtype for things the *agent* constructs and passes around, raw strings at the API deserialization boundary with `From` impls to convert.

8. **Tree-sitter grammar scope** — Starting with just Rust is safe. Adding PowerShell, TOML, and Markdown grammars increases binary size and compile time. Consider feature-gating grammars so users only compile what they need.

9. **Data model evolution** — The data model will evolve as new tools and capabilities are added. Each phase should include an audit: are the existing types still ergonomic? Do any need to be split, merged, or promoted to newtypes? This is not a one-time design — it's an ongoing practice.
