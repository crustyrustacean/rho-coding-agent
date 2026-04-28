# Phase 1b: Security Surface

**Goal:** The agent loop from Phase 1a is hardened. Destructive tool calls require approval, file paths are sandboxed, project context files are scanned and trusted, untrusted data is framed separately from user instructions, and tool results pass through a redaction layer before entering conversation history.

**Milestone:** A user can run `rho` against a local model on a real project directory without exposing themselves to the most common ways an agent can hurt them: arbitrary command execution, path traversal, prompt injection from file contents, or supply-chain attacks via malicious project context files.

## Scope

Phase 1b is the security surface that was originally bundled into Phase 1. It is split out so each defense gets focused implementation and test coverage rather than being rushed alongside the agent loop.

The Phase 1a agent loop is the prerequisite — every defense in Phase 1b plugs into seams established in 1a (`Tool::execute`, `ChatMessage`, `Conversation`, the registry).

## New Dependencies

| Crate | For | Decision |
|---|---|---|
| None | — | — |

Path canonicalisation uses `std::fs`. Hash storage for project context file trust uses a hand-rolled TOML reader/writer for `~/.rho/trusted_projects.toml` (the file's structure is trivial — list of `path = "hash"` entries). If the TOML format proves awkward to handle without a parser, accept `toml` here rather than wait for Phase 2; the choice is small either way.

## Exit Criteria

The agent enforces approval on destructive tools, sandboxes file operations, separates untrusted data from user instructions, scans and gates project context files behind first-load trust confirmation, and redacts known secret patterns from tool results before they enter conversation history. The default policy is **deny by default**: sandbox on, approval required for `Write` and `Destructive` tools, redaction enabled.

## Non-Goals (deferred to later phases)

- Per-tool approval policy from config (Phase 2, when config loading lands)
- Command denylist (Phase 2, with `RunCommand` expansion)
- Egress allowlist (Phase 2, with config + provider switching)
- Provider switch warning (Phase 2)
- API key handling via env vars (Phase 2)
- Trust-on-first-use auto-approval after N successful calls (future)
