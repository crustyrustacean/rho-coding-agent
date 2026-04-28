# Phase 6: LSP and Advanced Integrations (Future)

**Goal:** The agent can communicate with language servers for deeper code understanding.

**Milestone:** `rust-analyzer` provides hover type info and go-to-definition within the agent.

## Status

This phase is intentionally deferred. LSP integration adds significant complexity (long-lived background process, JSON-RPC protocol, capability negotiation) and the agent is already valuable without it. Pursue this only when the core agent loop, tools, TUI, and extensions are stable.

## Potential Approach

Accept `lsp-types` **(standard)** as a dependency. The protocol surface area is too large to reimplement safely. Write a minimal JSON-RPC transport over stdio (~200 lines) and use `lsp-types` for message serialization. Support only the messages we need initially: initialize, hover, goto-definition, find-references.
