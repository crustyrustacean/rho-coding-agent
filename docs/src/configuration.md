# Configuration

rho reads configuration from two TOML files, merged with project-level overrides taking precedence over user-level defaults.

## File locations

| Tier | Path | Purpose |
|---|---|---|
| User-level | `~/.rho/config.toml` | Global defaults: model, API endpoint, provider, egress |
| Project-level | `.rho/config.toml` (relative to sandbox root) | Per-project: model, approval policies, command denylist, sandbox, context files |

Both files are optional. Missing files are silently skipped; all fields have sensible defaults.

## Merging strategy

Project-level config overrides user-level config on a per-field basis. For struct fields: if the project sets a field, it wins; if not, the user-level value applies; if neither sets it, the hardcoded default applies.

For `Vec` fields (denylist commands, egress hosts, context scan list, custom redaction patterns): the project-level list **replaces** the user-level list. It does not append. This avoids surprising composition effects.

## Full configuration reference

```toml
[agent]
# Model identifier (overrides auto-detection)
model = "qwen3-8b"

# Maximum model-tool-model round trips before the loop fails (default: 32)
max_iterations = 32

# Maximum retry attempts on transient errors (default: 4)
retry_budget = 4

# Base backoff in milliseconds (default: 500)
initial_backoff_ms = 500

# Context window token budget (default: 32768)
token_budget = 32768

# Stuck-loop detection: consecutive identical outputs before nudge (default: 3, 0 = off)
stuck_loop_threshold = 3

# Show full chain-of-thought reasoning in output (default: false)
show_reasoning = false

[provider]
# Provider type: "local" (default), "openai", "anthropic", etc.
type = "local"

# API endpoint URL (default: http://localhost:1234/v1/chat/completions)
endpoint = "http://localhost:1234/v1/chat/completions"

# Environment variable holding the API key (never stored in plaintext)
api_key_env = "OPENAI_API_KEY"

[approval.per_tool]
# Per-tool approval overrides: "auto", "ask", "deny"
read_file = "auto"
write_file = "ask"
edit_file = "ask"
run_command = "ask"

[shell]
# Additional commands to deny (appended to the built-in denylist)
denied_commands = ["Remove-Item", "Invoke-WebRequest"]

# Additional denied flag combinations (all flags must be present to deny)
denied_flag_combos = [["-Quiet", "-Force"]]

[sandbox]
# File sandbox toggle (default: true)
enabled = true

[context]
# Override the default context file scan list
scan_list = ["AGENTS.md", "CLAUDE.md"]

[egress]
# Hostnames allowed in addition to localhost (default: empty)
allowed_hosts = ["api.openai.com"]

[redaction]
# Secret redaction toggle (default: true)
enabled = true

# Additional regex patterns to redact
custom_patterns = ["my-key-[a-zA-Z0-9]{32}"]

[system_prompt]
# Additional prompt fragments appended after the base prompt
extensions = ["Always use Rust idioms."]
```

## API key handling

API keys are **never** stored in plaintext in config files. Instead, config references environment variable names:

```toml
[provider]
api_key_env = "OPENAI_API_KEY"
```

The provider reads the key from the environment variable at runtime. If the variable is not set, the provider reports an error at connection time.

## CLI overrides

All config values can be overridden by CLI flags. CLI flags take highest priority:

| Config field | CLI flag |
|---|---|
| `agent.model` | `--model` |
| `agent.token_budget` | `--token-budget` |
| `provider.endpoint` | `--root` (indirectly) |
| System prompt | `--system` |
| Compact prompt | `--compact` |
| Provider consent | `--accept-external-provider` |
