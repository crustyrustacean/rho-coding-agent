# Fix 3: Add Denylist and CWD to Extension `rho.runCommand()`

**Bug:** Extension `rho.runCommand()` runs arbitrary commands with no denylist
check and no working directory restriction. This is a privilege escalation path
vs the built-in `RunCommand` tool, which checks `CommandDenylist` and validates
the working directory against the sandbox.

**Current behavior:**
```rust
// op_rho_run_command in rho-ext/src/host.rs
match std::process::Command::new(cmd).args(&args).output() { ... }
// — no current_dir() set, no denylist check
```

**Desired behavior:** Extension commands get the same denylist and cwd
enforcement as the built-in `RunCommand` tool.

## Constraint: Dependency Direction

`CommandDenylist` lives in `rho-tools`. `rho-ext` depends on `rho-core`, not
`rho-tools`. Adding `rho-ext → rho-tools` would create a circular dependency:
`rho → rho-tools → rho-core ← rho-ext` (siblings, fine), but `rho-ext → rho-tools`
would need `rho-tools` to also depend on `rho-ext` for nothing. Actually the
dependency is: `rho-ext` and `rho-tools` are both leaves off `rho-core`. Adding
`rho-ext → rho-tools` is technically allowed (it's a DAG), but `CommandDenylist`
depends on `rho_core::RhoConfig` for its `from_config()` constructor.

**Decision:** Re-export `CommandDenylist` from `rho-core`. The denylist is
configuration-driven — it belongs in `rho-core` alongside `RhoConfig`. The type
currently lives in `rho-tools` because that's where `RunCommand` is, but it's
a pure data structure with no tool-specific logic. Moving it keeps `rho-ext`
from needing to pull in the entire `rho-tools` crate (which includes `deno`,
`tree-sitter`, etc.).

Alternative considered and rejected: duplicating the denylist logic in
`rho-ext`. This would drift and be a maintenance burden.

Alternative considered and rejected: adding `rho-ext → rho-tools` dependency.
`rho-tools` is a heavy crate (deno, tree-sitter, reqwest). Extensions don't
need that weight just for a denylist check.

## Steps

### Step 1: Move `CommandDenylist` to `rho-core`

Move the `CommandDenylist` struct, its `Default` impl, `default_powershell()`,
`from_config()`, and `check()` method from `rho-tools/src/shell.rs` to a new
file `rho-core/src/denylist.rs`.

- `CommandDenylist::from_config()` already depends only on `rho_core::RhoConfig`,
  so no new dependencies are introduced.
- `rho-tools/src/shell.rs` re-exports it from `rho-core` for backward compat.
- `rho-tools/src/shell.rs` `RunCommand` continues to use it unchanged.

**Files:**
- `rho-core/src/denylist.rs` — new file with `CommandDenylist`
- `rho-core/src/lib.rs` — add `pub mod denylist;`
- `rho-tools/src/shell.rs` — remove `CommandDenylist` definition, import from `rho-core`

### Step 2 (RED): Add Denylist to `HostState`

Add a `denylist: CommandDenylist` field to `HostState`. This will cause
compile errors at every `HostState::new()` call site that doesn't provide one.

**Files:**
- `rho-ext/src/host.rs` — add `denylist: CommandDenylist` field, update `new()`,
  update all ~10 test call sites (expect compile failure)

### Step 3 (GREEN): Apply Denylist in `op_rho_run_command`

In `op_rho_run_command`, after the permission check, construct the full command
string (cmd + args joined with spaces) and call `denylist.check(&command)`.
If denied, return an error JSON.

Also set `current_dir()` to `self.cwd` (the project root) on the spawned
process, matching the built-in `RunCommand` behavior.

**Files:**
- `rho-ext/src/host.rs` — update `op_rho_run_command`

### Step 4: Thread Denylist from Config

The denylist needs to reach `HostState::new()`. The call chain is:

```
app.rs → ExtensionLoader::new(config, project_root)
       → spawn_one() / spawn_extensions()
       → ExtensionRuntime::spawn_from_file_with_perms(..., perms, model)
       → HostState::new(project_root, ext_root_dir, extra_allowed, denylist)
```

The `denylist` is built from `RhoConfig`, which is already available in `app.rs`.
Thread it through:

- `ExtensionLoader::new(config, project_root, denylist)` — store `denylist`
- `spawn_one()` / `spawn_extensions()` — pass to `spawn_from_file_with_perms`
- `spawn_from_file_with_perms()` — accept `denylist` and pass to `HostState::new()`
- `app.rs` — build denylist from config, pass to `ExtensionLoader::new()`

**Files:**
- `rho-ext/src/loader.rs` — add `denylist` field, thread through spawns
- `rho-ext/src/runtime.rs` — add `denylist` parameter to spawn functions
- `rho/src/app.rs` — build and pass denylist

### Step 5: Update Tests

- Existing tests that create `HostState` directly get a default denylist
  (`CommandDenylist::default_powershell()`) — this is what `from_config()` uses
  as its base, so tests using "echo" and "bash" will still pass.
- Add new test: `run_command_blocked_by_denylist` — verify that a denylisted
  command (e.g. `curl`) is rejected with an error.
- Add new test: `run_command_uses_project_root_as_cwd` — verify the command's
  working directory is the project root.
- Update `runtime_with_commands()` helper to accept denylist parameter.
- Update all `HostState::new()` call sites in test code (~10 in host.rs,
  ~13 in loader.rs, ~13 in runtime.rs, 1 in rpc.rs, ~8 in integration_test.rs).

**Files:**
- `rho-ext/src/host.rs` — test updates
- `rho-ext/src/runtime.rs` — test updates
- `rho-ext/src/loader.rs` — test updates
- `rho-ext/tests/integration_test.rs` — test updates
- `rho/src/rpc.rs` — test update

### Step 6: Docs

Update JSDoc on `rho.runCommand()` in `host_shim.js` and `rho.d.ts` to mention
the denylist and cwd behavior.

**Files:**
- `rho-ext/src/host_shim.js`
- `rho-ext/types/rho.d.ts`

## Risk Assessment

**Low risk.** The denylist is applied before execution, so existing extensions
that use non-denied commands (like `git`, `echo`, `cargo`) will continue to
work unchanged. The cwd change is also safe — extensions already have `cwd`
set to the project root (from Fix 1), so setting `current_dir()` on the spawned
process is consistent with existing behavior.

The only behavioral change for existing extensions is that denied commands
will now be rejected instead of executed. This is a tightening, not a breaking
change — extensions that relied on running denied commands were already
operating against the design intent.

## Call Site Tally

~45 call sites to update (from Fix 1 experience — same files, similar pattern):
- `rho-ext/src/host.rs`: ~10 (test helpers + manual constructions)
- `rho-ext/src/loader.rs`: ~13 (test constructors)
- `rho-ext/src/runtime.rs`: ~13 (test constructors)
- `rho-ext/tests/integration_test.rs`: ~8 (test constructors)
- `rho/src/rpc.rs`: 1 (test constructor)
