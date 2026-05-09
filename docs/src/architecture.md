# Architecture

rho is organized as a layered workspace where dependencies flow downward only.

```text
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │
├─────────────────────────────────────────────────┤
│                   rho-ext                        │
├─────────────────────────────────────────────────┤
│                   rho-tui                        │
├─────────────────────────────────────────────────┤
│                   rho-tools                      │
├─────────────────────────────────────────────────┤
│                   rho-highlight                  │
├─────────────────────────────────────────────────┤
│                   rho-core                       │
└─────────────────────────────────────────────────┘
```

The rule is simple: a crate may only depend on crates below it in the stack. `rho-core` and `rho-highlight` depend on nothing but external libraries.

See also:

- [Workspace Layout](./architecture/workspace-layout.md)
- [Crate Responsibilities](./architecture/crate-responsibilities.md)
- [Dependency Flow](./architecture/dependency-flow.md)
