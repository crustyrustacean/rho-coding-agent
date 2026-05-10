# Threat Model

rho takes untrusted input (LLM output), interprets it as instructions, and executes those instructions with the full privileges of the user. The security model is defense-in-depth — no single layer is sufficient, but each layer raises the bar.

## Threats and defenses

| Threat | Primary Defense | Secondary |
|---|---|---|
| Destructive command execution | Command denylist | Approval gate |
| Data exfiltration via shell | Approval gate | Denylist |
| Data exfiltration via provider | Provider consent warning | Command denylist |
| Path traversal via file tools | File sandbox (canonicalised) | Approval gate |
| Secret exposure in tool output | Best-effort redaction | Approval gate |
| Prompt injection via file content | Untrusted-data framing (`<context>` / `<context:end>`) | Approval gate |
| Supply-chain prompt attack | Project context file trust (SHA-256 hash) | User confirmation on first load |
| Extension command injection | Structured argument substitution | Approval gate |
| Runaway tool-call loop | Max iteration guard (default: 32) | Stuck-loop detection |
| Credential at rest | Env var references (no plaintext in config) | Optional credential store (future) |

## Scope and limitations

- **Trusted user:** The human at the keyboard is trusted. Approval gates protect against *model*-initiated actions, not user-initiated ones.
- **No OS-level isolation:** Tools execute as the same user/process as rho. The sandbox validates paths but cannot prevent a determined model from exploiting a zero-day in a dependency.
- **Best-effort redaction:** The `Redactor` matches known secret patterns (API keys, tokens, passwords). It cannot detect secrets that don't match its patterns.
- **Command denylist coverage:** The denylist catches common exfiltration vectors (PowerShell networking cmdlets, `curl`, `wget`, .NET HTTP). A creative model can construct network requests using .NET APIs that aren't in the substring list. The approval gate is the primary defense.

See the individual pages for detailed design of each defense layer:

- [File Sandbox](./file-sandbox.md)
- [Secret Redaction](./secret-redaction.md)
- [Prompt Injection Defense](./prompt-injection-defense.md)
- [Egress Control](./egress-control.md) (historical — removed in v0.33.3)
