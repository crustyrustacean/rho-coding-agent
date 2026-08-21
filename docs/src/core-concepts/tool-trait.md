# Tool Trait

The `Tool` trait is the interface every tool implements. It defines a name, description, JSON Schema for parameters, risk classification, and an async `execute` method.

## Definition

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's registered name (used by the model to invoke it).
    fn name(&self) -> ToolName;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// JSON Schema object describing this tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Risk classification, used by the approval policy.
    fn risk(&self) -> ToolRisk;

    /// Execute the tool. Check `cancel.is_cancelled()` at I/O boundaries.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome>;
}
```

## Risk levels

Every tool has a `ToolRisk` that determines whether it needs human approval:

| Risk | Meaning | Approval | Examples |
|---|---|---|---|
| `Read` | Cannot modify state | Auto-approved | `ReadFile`, `ListDir`, `CargoCheck` |
| `Write` | May create or modify files | Requires approval | `WriteFile`, `EditFile`, `CargoFix` |
| `Destructive` | May cause irreversible effects | Requires approval | `RunCommand` |
| `Network` | Makes network requests | Requires approval | Extension tools that call `fetch()` |

The approval policy can override these defaults per-tool via config.

## Tool result

`execute` returns `ToolOutcome::Immediate(ToolResult)` containing:

- `output: String` — the text result (sent to the model and session)
- `is_error: bool` — whether the tool reported an error (e.g., non-zero exit code)
- `details: ToolResultDetails` — structured data *not* sent to the model

The `details` field carries richer information for tools and extensions:

| Variant | Purpose |
|---|---|
| `None` | No structured detail (default) |
| `FullOutput` | Preserves un-truncated output when the model-facing text is truncated |
| `Diagnostics` | Structured compiler diagnostics from `CargoCheck` / `CargoClippy` |

## Tool registry

`ToolRegistry` maps tool names to `Box<dyn Tool>` implementations:

```rust
let mut registry = ToolRegistry::new();
registry.register(Box::new(ReadFile { root: root.clone() }));
registry.register(Box::new(WriteFile { root: root.clone() }));

// Execute a model-issued tool call
let result = registry.execute(&call, cancel).await?;
```

Duplicate tool names panic at registration (fast-fail, programming error). Execution returns `RhoError::ToolNotFound` if the model requests an unregistered tool.

## Built-in tools

rho ships with 16 built-in tools, all registered in `rho-tools::register_all()` (plus `memory`, registered only when `[memory] enabled = true`):

| Tool | Risk | Description |
|---|---|---|
| `read_file` | Read | Read file contents, wrapped in `<context>` framing |
| `batch_read` | Read | Read multiple files at once (up to 20 paths) |
| `search_files` | Read | Regex content search (ripgrep engine) returning `path:line:` matches; `.gitignore`-aware, sandbox-confined |
| `find_files` | Read | Find files by name glob (e.g. `*.rs`, `Cargo.toml`); `.gitignore`-aware, sandbox-confined |
| `write_file` | Write | Create or overwrite files within the sandbox |
| `list_dir` | Read | `.gitignore`-aware directory listing |
| `edit_file` | Write | Hashline-anchor replacements with node-splitting validation |
| `run_command` | Destructive | Execute shell commands with denylist enforcement |
| `cargo_check` | Read | Structured compiler diagnostics |
| `cargo_clippy` | Read | Structured lint diagnostics |
| `cargo_test` | Read | Structured test pass/fail results |
| `cargo_fix` | Write | Apply machine-applicable compiler/clippy suggestions |
| `rustc_explain` | Read | Look up detailed explanations for error codes |
| `rustdoc_lookup` | Read | Look up Rust standard library documentation (local rustdoc) |
| `crates_io_lookup` | Read | Search and inspect crate metadata on crates.io |
| `session_summary` | Read | Compressed numbered history of user requests and outcomes |
| `memory` | Read | Store, search, and manage project-local knowledge (via `rho-memory`) |

## Extension tools

TypeScript extensions can register additional tools via `rho-ext`. Each extension tool is wrapped in a `DenoTool` that implements the same `Tool` trait. Extension tools appear alongside built-in tools in the `ToolRegistry` and are visible to the model via the tool schema.

Extension tools are discovered at startup from `~/.rho/extensions/` and `.rho/extensions/`. They can be hot-reloaded at runtime via the `reloadExtensions` RPC method.

See [Extensions](../extensions.md) for details.