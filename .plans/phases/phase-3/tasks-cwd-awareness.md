# Fix: Working Directory Awareness

## Problem

The model is never told its working directory. When it runs `cd src/lib` in a
`run_command` call, the command succeeds in that process but the next
`run_command` starts fresh in the sandbox root. The model believes it's in
`src/lib` and generates wrong relative paths — it has "forgotten" where it was.

Pi solves this by appending `Current working directory: /path` at the end of
every system prompt. This plan follows the same approach.

## Scope

| Crate | Change | Risk |
|---|---|---|
| `rho` (binary) | Inject cwd into system prompt, detect stale cwd on resume | Low — only `main.rs` |
| `rho-tools` | Expand cd-warning heuristic | Low — one function |
| `rho-core` | No public API changes | None |

## Tasks

### Task 1: Inject CWD into the system prompt

**File:** `rho/src/main.rs`, function `load_system_prompt`

After `compose_system_prompt` returns, append an `# Environment` section:

```
# Environment

- Working directory (project root): /Users/jeff/dev/crustyrustacean/rho-coding-agent
- All relative file paths and shell commands resolve from this directory.
- Each `run_command` invocation starts a fresh process in this directory.
  `cd` and `Set-Location` do not persist between commands — include the full
  relative path from the project root in every command.
```

The path comes from `sandbox.path().display()`. This is the canonical sandbox
root detected by `find_project_root()` (or `--root`).

**Cases:**
- `--system "custom prompt"` → still injected (user overrode the base prompt,
  not the environment section)
- `--compact` → still injected (compact prompt has the same problem)
- `--session resume` → injected with the *current* sandbox root, not the
  session header's stale cwd (Task 3 handles the mismatch)
- `--ephemeral` → no difference

**Tests:** Update the `load_system_prompt` unit tests (they need `sandbox` and
`cli` fixtures — currently none exist). Alternatively, since this is a binary
function with side effects, test via the public `compose_system_prompt` and a
new thin wrapper test that verifies the environment section is present.

### Task 2: Expand `cd`-in-command warning

**File:** `rho-tools/src/shell.rs`, function `command_attempts_directory_escape`

Currently only detects `cd ..` and `Set-Location ..`. Expand to detect *any*
`cd` or `Set-Location` (into a subdirectory, absolute path, etc.) and emit a
more informative warning:

```
[NOTE: each run_command starts a fresh process in the project root.
 cd and Set-Location do not persist between commands.
 Include the full relative path from the project root in every command.]
```

The function return type changes from `bool` to `Option<String>` (the warning
text, or `None`).

**Tests:**
- `cd src` → returns Some(warning)
- `Set-Location rho-core/src` → returns Some(warning)
- `cd ..` → returns Some(warning) (still detected, same as before)
- `cd C:\Users` → returns Some(warning) (absolute paths are also ephemeral)
- `Get-ChildItem` → returns None
- `cargo check` → returns None
- `Push-Location src` → returns Some(warning) (also ephemeral)

### Task 3: Detect stale CWD on session resume

**File:** `rho/src/main.rs`, session resume block

When `--session <path>` is used, compare the session header's `cwd` against the
current sandbox root. If they differ, emit a warning:

```
warning: session was created in a different directory
  session: /Users/jeff/dev/old-location/project
  current:  /Users/jeff/dev/new-location/project
  continuing with current directory
```

This is a warning only, not an error. The tools already use the current sandbox
root, so there's no functional breakage — the model just needs accurate context.

If the session's `cwd` no longer exists at all, emit a stronger warning:

```
warning: session's working directory no longer exists
  session: /Users/jeff/dev/deleted-project
  current:  /Users/jeff/dev/crustyrustacean/rho-coding-agent
  continuing with current directory
```

**Tests:**
- Session cwd matches current cwd → no warning
- Session cwd differs from current cwd → warning emitted
- Session cwd does not exist → warning emitted

## Non-goals

- **Dynamic cwd tracking.** Pi doesn't track `cd` between commands either — it
  tells the model the startup cwd and relies on the model to use full paths.
  We do the same.
- **A `--working-directory` parameter on `run_command`.** This would let the
  model explicitly set CWD per invocation, but it's a larger API surface change
  and pi doesn't need it. Defer if the model still struggles after this fix.
- **Changing `compose_system_prompt`'s public API.** The cwd injection is a
  binary-level concern, not a library concern.
