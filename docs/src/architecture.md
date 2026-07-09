# Architecture

rho is organized as a layered workspace where dependencies flow downward only.

```text
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │  ← Headless JSON-RPC 2.0 agent
├─────────────────────────────────────────────────┤
│  rho-ext          rho-tools                       │  ← Extensions   /  Built-in tools
├─────────────────────────────────────────────────┤
│            rho-memory      rho-highlight           │  ← Siblings of rho-tools
├─────────────────────────────────────────────────┤
│                   rho-core                       │  ← Agent kernel (loop, types, traits, data model)
├─────────────────────────────────────────────────┤
│                   rho-ai                         │  ← Unified LLM provider abstraction
└─────────────────────────────────────────────────┘
```

`rho-ai` is the lowest layer; it depends only on external libraries. `rho-core` depends on `rho-ai` for the `LlmService` trait. Every other crate eventually depends on `rho-core`.

`rho-ext`, `rho-tools`, `rho-memory`, and `rho-highlight` are all siblings sitting directly on `rho-core`. `rho-tools` additionally depends on `rho-memory` and `rho-highlight`; the others each depend only on `rho-core`.

See also:

- [Workspace Layout](./architecture/workspace-layout.md)
- [Crate Responsibilities](./architecture/crate-responsibilities.md)
- [Dependency Flow](./architecture/dependency-flow.md)
