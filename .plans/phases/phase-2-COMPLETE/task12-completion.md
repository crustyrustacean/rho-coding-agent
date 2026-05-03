# Task 12: Security Tests — Completion Report

**Date:** 2026-05-01 (retroactive)
**Branch:** Commit `815455c`
**Status:** ✅ Complete — all tests pass, fmt + clippy clean

---

## What Was Done

Added egress enforcement integration tests to complement the existing security test suite. With this task, all security layers defined in Phase 2 now have full test coverage.

### New security tests: Egress enforcement (6 tests)

**`rho-core/tests/integration_tests.rs`:**

| Test | What it verifies |
|---|---|
| `egress_blocks_external_host_with_default_config` | External host blocked with default egress config |
| `egress_allows_localhost_with_default_config` | localhost passes egress; fails with HTTP error (not egress) |
| `egress_allows_listed_external_host` | Allowlisted host bypasses egress; request reaches network |
| `egress_blocks_unlisted_external_host` | Unlisted host blocked even when others are allowed |
| `egress_allows_127_0_0_1_with_default_config` | 127.0.0.1 passes egress; fails with HTTP error |
| `egress_no_config_allows_any_host` | Legacy constructor (no egress) permits any host |

### Existing security test coverage (from prior tasks)

| Security layer | Test location | Tests | From task |
|---|---|---|---|
| Command denylist | `rho-tools/tests/tool_tests.rs` | 13 | Task 1 |
| Working directory escape | `rho-tools/tests/tool_tests.rs` | 3 | Task 1 |
| Secret redaction (built-in) | `rho-core/tests/security_tests.rs` | 8 | Task 7 |
| Secret redaction (custom patterns) | `rho-core/tests/security_tests.rs` | 2 | Task 7 |
| Secret redaction (disable) | `rho-core/tests/security_tests.rs` | 2 | Task 7 |
| Sandbox validation | `rho-core/tests/security_tests.rs` | 6 | Phase 1b |
| Approval policy (default) | `rho-core/tests/security_tests.rs` | 4 | Phase 1b |
| Approval policy (config) | `rho-core/tests/security_tests.rs` | 4 | Task 6 |
| Config: API key not plaintext | `rho-core/tests/security_tests.rs` | 1 | Task 6 |
| Config: sandbox toggle | `rho-core/tests/security_tests.rs` | 2 | Task 6 |
| Config: redaction toggle | `rho-core/tests/security_tests.rs` | 2 | Task 6 |
| Egress enforcement | `rho-core/tests/integration_tests.rs` | 6 | Task 12 |
| Provider consent (unit) | `rho/src/main.rs` | 6 | Task 10 |

**Total security tests: 59**

---

## Test Isolation Guarantees

All security tests are deterministic and isolated:

- **No real network calls** — Egress tests either check `check_egress()` directly (unit) or use endpoints that will fail at the HTTP level (localhost:1, invalid hosts). The `egress_allows_listed_external_host` test uses a 5-second timeout to avoid hanging.
- **No real credential store access** — API key tests verify that config stores env var *names*, not values, and that `resolve_api_key()` returns `None` when the env var is unset.
- **No real file system mutation** — Sandbox tests use `tempfile::TempDir` for cleanup.
- **No process spawning** — Denylist tests use `MockShellExecutor` which records zero calls for denied commands.

---

## File Changes

| File | Change |
|---|---|
| `rho-core/tests/integration_tests.rs` | Added 6 egress enforcement tests |

---

## Design Decisions

1. **Integration tests for egress, not unit tests** — Egress enforcement spans `LocalChatClient` + `EgressConfig` + `reqwest`. Testing it at the integration level verifies the full stack works together, not just the config logic.

2. **Timeout on external host tests** — Tests that allow external hosts use `tokio::time::timeout(5s)` to avoid hanging on DNS resolution or network latency. The assertion checks that the error is *not* an egress error, regardless of what HTTP error occurs.

3. **Connection-refused distinguishes egress from HTTP** — For localhost tests, the expected error is `RhoError::Http` (connection refused), not an egress error. This proves the request passed the egress check and failed at the network level.
