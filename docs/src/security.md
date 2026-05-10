# Security

rho takes untrusted input (LLM output), interprets it as instructions, and executes those instructions with the full privileges of the user. The security model is defense-in-depth — no single layer is sufficient, but each layer raises the bar.

See also:

- [Threat Model](./security/threat-model.md)
- [File Sandbox](./security/file-sandbox.md)
- [Secret Redaction](./security/secret-redaction.md)
- [Prompt Injection Defense](./security/prompt-injection-defense.md)
- [Egress Control](./security/egress-control.md) (historical)
