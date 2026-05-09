# Secret Redaction

The `Redactor` scans tool output for known secret patterns and replaces matches with `[REDACTED]`. This prevents the model from seeing (and potentially leaking) secrets that appear in file contents, command output, or configuration files.

## How it works

Redaction is applied to tool results before they enter the conversation history. The model never sees the original secret — only `[REDACTED]`.

## Built-in patterns

| Pattern | Example matches |
|---|---|
| AWS access key IDs | `AKIA...` |
| AWS secret access keys | 40-char base64 strings following `AKIA` keys |
| GitHub personal access tokens | `ghp_...`, `github_pat_...` |
| OpenAI API keys | `sk-...` |
| Slack tokens | `xoxb-...`, `xoxp-...` |
| Bearer tokens | `Bearer <opaque-string>` |
| Generic password assignments | `password = "..."`, `password: '...'` |

## Custom patterns

Additional regex patterns can be added in config:

```toml
[redaction]
enabled = true
custom_patterns = [
    "my-service-key-[a-zA-Z0-9]{32}",
    "token: \\S+",
]
```

Custom patterns are applied after the built-in patterns. Invalid regex patterns are silently ignored (a warning is printed to stderr at startup).

## Vec replacement semantics

Custom patterns follow the same replacement rule as all Vec fields: the project-level list **replaces** the user-level list (not appended). This avoids surprising composition effects.

## Configuration

Redaction is enabled by default. It can be disabled in config:

```toml
[redaction]
enabled = false
```

## Limitations

- **Best-effort:** The redactor matches known patterns. It cannot detect secrets that don't match any pattern (e.g., a novel token format).
- **No reconstruction:** Redaction is one-way — the original value is discarded. Tool result details (`ToolResultDetails::FullOutput`) preserve the un-redacted content for tools that need it, but the model-facing text is always redacted.
- **Pattern overlap:** A string matching multiple patterns is replaced once. The `[REDACTED]` sentinel is not itself matched by subsequent patterns.
