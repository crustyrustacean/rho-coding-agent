# Testing

rho uses a layered test strategy: unit tests within each source file, integration tests at the crate level, and end-to-end scenario tests that exercise the full agent loop.

## Running tests

```sh
cargo xtask ci        # Full CI pipeline: fmt → lint → build → test
ctest                 # All tests via cargo-nextest (falls back to cargo test)
cargo xtask test -p rho-core -- --nocapture  # Single crate
```

## Test structure

| Layer | Location | What it tests |
|---|---|---|
| Unit tests | `#[cfg(test)] mod tests` inside each source file | Individual functions, types, edge cases |
| Integration tests | `rho-core/tests/integration_tests.rs` | Agent loop, approval flow, context management |
| Security integration tests | `rho-core/tests/security_tests.rs` | Sandbox enforcement, denylist, trust store |
| Session integration tests | `rho-core/tests/session_integration_tests.rs` | Session tree operations, persistence, branching |
| Agent builder integration tests | `rho-core/tests/agent_builder.rs` | Public-API characterization for the embeddable `Agent`/`AgentBuilder` (the embedder's outside view) |
| RPC integration tests | `rho/src/rpc.rs` (`#[cfg(test)] mod tests`) | Full JSON-RPC 2.0 protocol: command dispatch, event sequencing, approval round-trips, tool calls, errors, multi-turn sessions, mid-turn steering and redirect approval flow |
| Tool integration tests | `rho-tools/tests/tool_tests.rs` | Tool execution, sandbox enforcement, denylist |
| Shell executor tests | `rho-tools/tests/shell_executor_tests.rs` | Shell command execution, path normalization |
| Extension unit tests | `rho-ext/src/*.rs` (`#[cfg(test)] mod tests`) | Runtime spawning, manifest parsing, discovery, transpilation, DenoTool, DenoObserver, host ops, module loader |

RPC integration tests live in `rho/src/rpc.rs` (not `rho/tests/`) because `App`'s fields are `pub(crate)`. They use `TestProvider` to wrap a `MockChatClient` as a `Provider` and construct `App` directly, bypassing the full CLI startup sequence. The RPC dispatch loop is decoupled from I/O via the `Transport` trait (`rho/src/transport.rs`); tests inject either a `StdioTransport` over `Cursor` readers (all lines readable up front) or a `ChannelTransport` (mpsc-backed, controls when each message becomes readable) together with `SyncTool` (a tool that blocks until released) to inject messages deterministically mid-turn.

## Test helpers (`rho-test-helpers`)

`rho-test-helpers` provides the shared infrastructure that all test layers use:

### Mocks

| Helper | Purpose |
|---|---|
| `MockChatClient` | Pre-programmed model responses for deterministic agent loop tests |
| `MockShellExecutor` | Pre-programmed command output for tool tests |
| `TestProvider` | Wraps `MockChatClient` as a `Provider` for tests needing `ProviderRegistry` |
| `AutoApproveGate` | Approves all tool calls automatically |
| `AutoDenyGate` | Denies all tool calls automatically |

### Builders

| Helper | Purpose |
|---|---|
| `text_events(text)` | Create a streaming text response followed by `Done` |
| `tool_call_events(id, name, args)` | Create a single tool call response |
| `multi_tool_call_events(calls)` | Create a response with multiple tool calls |
| `FixedResponseTool` | A tool that always returns the same output |
| `FailingTool` | A tool that always returns an error |

| Fixtures and utilities | Purpose |
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
| Scenarios | Low | End-to-end validation against real model servers |
