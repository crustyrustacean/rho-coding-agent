# Approval Policy

The approval policy sits between the model's tool call and its execution. It decides whether a tool call needs human confirmation, and the approval gate presents that decision to the user.

## Two-part system

| Component | Role |
|---|---|
| `ApprovalPolicy` | Decides *whether* approval is needed (configurable logic) |
| `ApprovalGate` | Asks the user for confirmation at runtime (UI integration point) |

## Policy: `ConfigApprovalPolicy`

The default policy uses per-tool overrides from config, falling back to risk-based defaults:

```toml
[approval.per_tool]
read_file = "auto"    # never ask
write_file = "ask"    # always ask
run_command = "ask"   # always ask
```

| Action | Behaviour |
|---|---|
| `Auto` | Execute without asking |
| `Ask` | Require human confirmation |
| `Deny` | Refuse to execute (returns a denial error to the model) |

When a tool is not listed in config, the risk-based default applies:

| ToolRisk | Default |
|---|---|
| `Read` | Auto |
| `Write` | Ask |
| `Destructive` | Ask |

## Gate: `ReplApprovalGate`

The REPL implementation presents a preview and reads `y/N` from stdin:

```text
  Tool     : write_file
  Risk     : write
  Arguments: {"path":"src/main.rs","content":"..."}
  Execute? [y/n] 
```

On denial, a synthetic error result is fed back to the model as if the tool had failed. The model can then adjust its approach — it does not crash or get stuck.

## Custom gates

The `ApprovalGate` trait is async, so any UI can implement it:

```rust
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request_approval(&self, call: &ModelToolCall, risk: ToolRisk) -> bool;
}
```

The TUI (Phase 4) will implement this with an in-terminal prompt that shows rich previews (syntax-highlighted diffs, command inspection) rather than the plain-text REPL format.
