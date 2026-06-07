# Architecture

rho is organized as a layered workspace where dependencies flow downward only.

```text
┌─────────────────────────────────────────────────┐
│                   rho (binary)                   │
├─────────────────────────────────────────────────┤
│                   rho-repl                       │
├─────────────────────────────────────────────────┤
│                   rho-ext                        │
├─────────────────────────────────────────────────┤
│                   rho-tools                      │
├─────────────────────────────────────────────────┤
│                   rho-highlight                  │
├─────────────────────────────────────────────────┤
│                   rho-core                       │
├─────────────────────────────────────────────────┤
│                   rho-ai                         │
└─────────────────────────────────────────────────┘
```

The rule is simple: a crate may only depend on crates below it in the stack. `rho-ai` and `rho-highlight` depend on nothing but external libraries. `rho-repl` spawns `rho` as a subprocess and does not depend on any rho crate.

See also:

- [Workspace Layout](./architecture/workspace-layout.md)
- [Crate Responsibilities](./architecture/crate-responsibilities.md)
- [Dependency Flow](./architecture/dependency-flow.md)
