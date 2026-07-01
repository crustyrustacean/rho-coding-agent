# Architecture

rho is organized as a layered workspace where dependencies flow downward only.

```text
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │
├─────────────────────────────────────────────────┤
│                   rho-ext                        │
├─────────────────────────────────────────────────┤
│                   rho-tools                      │
├─────────────────────────────────────────────────┤
│                   rho-memory                     │
├─────────────────────────────────────────────────┤
│                   rho-highlight                  │
├─────────────────────────────────────────────────┤
│                   rho-core                       │
├─────────────────────────────────────────────────┤
│                   rho-ai                         │
└─────────────────────────────────────────────────┘
```

The rule is simple: a crate may only depend on crates below it in the stack. `rho-ai` depends on nothing but external libraries; every other crate eventually depends on `rho-core` (and `rho-core` depends on `rho-ai`).

See also:

- [Workspace Layout](./architecture/workspace-layout.md)
- [Crate Responsibilities](./architecture/crate-responsibilities.md)
- [Dependency Flow](./architecture/dependency-flow.md)
