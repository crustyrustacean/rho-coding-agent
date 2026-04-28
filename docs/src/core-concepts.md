# Core Concepts

The agent kernel in `rho-core` is built on a small set of interlocking concepts.

- [Agent Loop](./core-concepts/agent-loop.md) — the state machine that drives the conversation
- [Tool Trait](./core-concepts/tool-trait.md) — the interface all tools implement
- [ChatClient Provider Trait](./core-concepts/chatclient-provider-trait.md) — provider-agnostic model access
- [Approval Policy](./core-concepts/approval-policy.md) — the gate between tool calls and execution
- [Context Management](./core-concepts/context-management.md) — keeping the conversation within the context window
- [Project Context Files](./core-concepts/project-context-files.md) — loading and trusting project-level instructions
