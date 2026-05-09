# Testing

rho uses a layered test strategy: unit tests within each source file, integration tests at the crate level, and end-to-end scenario tests that exercise the full agent loop.

## Running tests

```sh
cargo xtask ci        # Full CI pipeline: fmt → lint → build → test
cargo xtask test      # All tests with stdout visible
cargo xtask test -p rho-core -- --nocapture  # Single crate
```

## Test structure

| Layer | Location | What it tests |
|---|---|---|
| Unit tests | `#[cfg(test)] mod tests` inside each source file | Individual functions, types, edge cases |
| Integration tests | `rho-core/tests/integration_tests.rs` | Agent loop, approval flow, context management |
| Tool integration tests | `rho-tools/tests/tool_tests.rs` | Tool execution, sandbox enforcement, denylist |
| Scenario tests | `rho-eval/scenarios/` | Full agent loop with real model |

## Test helpers (`rho-test-helpers`)

`rho-test-helpers` provides the shared infrastructure that all test layers use:

### Mocks

| Helper | Purpose |
|---|---|
| `MockChatClient` | Pre-programmed model responses for deterministic agent loop tests |
| `MockShellExecutor` | Pre-programmed command output for tool tests |
| `AutoApproveGate` | Approves all tool calls automatically |
| `AutoDenyGate` | Denies all tool calls automatically |

### Builders

| Helper | Purpose |
|---|---|
| `text_response(text)` | Create a simple text model response |
| `tool_call_response(name, args)` | Create a tool call model response |
| `multi_tool_call_response(calls)` | Create a response with multiple tool calls |
| `FixedResponseTool` | A tool that always returns the same output |

### Fixtures and utilities

| Helper | Purpose |
|---|---|
| `FileTestEnv` | Temporary directory with file-system operations for sandbox tests |
| `in_memory_session(prompt)` | Create a session without disk persistence |
| `empty_trust_store()` | Trust store with no trusted files |
| `detect_shell()` | Find the available PowerShell on the test system |
| `tempdir_with_sandbox()` | Create a temp directory with a `SandboxRoot` |
| `assert_no_orphan_tool_results()` | Verify every tool call has a matching result in the session |

## Naming conventions

- Test functions: `descriptive_snake_case` — e.g., `system_message_is_always_retained`
- Test modules: match the function under test — e.g., `tests::tool_result_success_has_no_details`
- Fixtures: `tests/fixtures/*.json` — externalised test data for deserialization tests

## Coverage priorities

| Area | Coverage | Notes |
|---|---|---|
| Error handling | High | Every `RhoError` variant has a test |
| Deserialization | High | Every JSON fixture has a round-trip test |
| Edge cases | High | Empty inputs, oversized inputs, invalid inputs |
| Sandbox | High | Path traversal, symlinks, non-existent paths |
| Denylist | High | Each denied command and substring pattern |
| Context management | High | Eviction, pinning, compaction rendering |
| Integration | Medium | Agent loop with realistic message sequences |
| Scenarios | Low (5) | End-to-end validation against real model |
