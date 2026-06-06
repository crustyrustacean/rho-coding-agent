# Plan: Fix Extension Sandbox — cwd = Project Root

## Problem

User-level extensions (in `~/.rho/extensions/`) are sandboxed to their own directory,
not the project root. They cannot read or write project files, making them useless.
Project-level extensions (in `<project>/.rho/extensions/`) have `cwd` set to
`<project>/.rho/extensions/<name>/`, so `rho.readFile("Cargo.toml")` fails for them
too (it resolves to `<project>/.rho/extensions/<name>/Cargo.toml`).

## Root Cause

In `discover.rs`, `DiscoveredExtension.root_dir` is set to the extension's parent
directory (the extensions directory itself). This flows through to
`HostState::new(root_dir, extra_allowed)` where it becomes `cwd`. Since
`HostState::resolve_and_check()` does `self.cwd.join(requested)`, all relative
paths resolve against the extension directory, not the project root.

## Solution

Three coordinated changes:

### A) Set extension `cwd` to the project root
### B) Add the extension's own directory as an extra `allowed_path`
### C) Add `rho.getProjectRoot()` host function

## Detailed Steps (TDD)

### Step 1: Add `project_root` field to `HostState` — RED phase

**File:** `rho-ext/src/host.rs`

Add a `project_root: PathBuf` field to `HostState`. Update `HostState::new()` to
accept a `project_root` parameter. Add `op_rho_get_project_root()` op that returns
the project root string. Register the op in the `rho_host` extension definition.

**Changes to `HostState`:**

```rust
pub struct HostState {
    pub cwd: PathBuf,              // NOW: project root (was: extension dir)
    pub project_root: PathBuf,     // NEW: always the project root
    pub allowed_paths: Vec<PathBuf>,
    pub allow_commands: bool,
    pub allow_network: bool,
    pub model: Arc<Mutex<String>>,
}
```

**New constructor signature:**

```rust
impl HostState {
    pub fn new(
        project_root: PathBuf,     // The project sandbox root
        ext_root_dir: PathBuf,     // The extension's own directory (extra allowed)
        extra_allowed: Vec<PathBuf>,
    ) -> Self {
        let mut allowed = Vec::new();

        // Always include the project root (canonicalized).
        if let Ok(canonical) = project_root.canonicalize() {
            allowed.push(canonical);
        } else {
            allowed.push(project_root.clone());
        }

        // Add the extension's own directory (canonicalized).
        if let Ok(canonical) = ext_root_dir.canonicalize() {
            if !allowed.contains(&canonical) {
                allowed.push(canonical);
            }
        } else {
            if !allowed.contains(&ext_root_dir) {
                allowed.push(ext_root_dir.clone());
            }
        }

        // Add any extra allowed paths (resolved relative to project root).
        for p in extra_allowed {
            let resolved = project_root.join(p);
            if let Ok(canonical) = resolved.canonicalize() {
                if !allowed.contains(&canonical) {
                    allowed.push(canonical);
                }
            }
        }

        Self {
            cwd: project_root.clone(),       // ← project root, not extension dir
            project_root,                     // ← stored for getProjectRoot()
            allowed_paths: allowed,
            allow_commands: false,
            allow_network: false,
            model: Arc::new(Mutex::new(String::new())),
        }
    }
}
```

**Key behavioral changes:**
- `cwd` is now the project root, not the extension directory
- `resolve_and_check()` does `self.cwd.join(requested)` → resolves against project root ✅
- `resolve_and_check_write()` does `self.cwd.join(requested)` → same ✅
- `allowed_paths` includes both project root AND extension directory
- Extension's own data files still pass containment check (they're in `allowed_paths`)
- But relative paths resolve against project root, so `rho.readFile("my-ext-data.json")`
  would need to be `rho.readFile(".rho/extensions/my-ext/my-ext-data.json")` or use
  an absolute path via `rho.getProjectRoot()` + extension-relative path

**New op:**

```rust
#[op2]
#[string]
pub fn op_rho_get_project_root(state: &mut OpState) -> String {
    state
        .try_borrow::<HostState>()
        .map_or_else(
            || std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .to_string_lossy()
                .to_string(),
            |s| s.project_root.to_string_lossy().to_string(),
        )
}
```

**Register in extension definition:**

```rust
deno_core::extension!(
    rho_host,
    ops = [
        // ... existing ops ...
        op_rho_get_project_root,   // ← NEW
    ],
    // ...
);
```

**Tests (in `host.rs` `#[cfg(test)]`):**
- `host_state_cwd_is_project_root` — construct with project_root="/project", ext_dir="/ext", assert `cwd == "/project"`
- `host_state_allowed_paths_includes_both` — assert `allowed_paths` contains canonical of both project root and ext dir
- `rho_get_project_root_returns_project_root` — eval `rho.getProjectRoot()`, assert equals project root

**Backward compatibility note:** `HostState::new()` signature changes from
`(cwd, extra_allowed)` to `(project_root, ext_root_dir, extra_allowed)`. All callers
must be updated. There are exactly two callers:
1. `runtime.rs::spawn_from_file_with_perms()` (line 212)
2. `host.rs::runtime_with_state()` test helper (and similar test helpers)

### Step 2: Update `spawn_from_file_with_perms()` to accept project root — GREEN phase

**File:** `rho-ext/src/runtime.rs`

Add a `project_root` parameter to `spawn_from_file_with_perms()`:

```rust
pub fn spawn_from_file_with_perms(
    entry_path: &Path,
    root_dir: &Path,          // Extension's own directory (for module loader)
    project_root: &Path,      // NEW: the project sandbox root
    permissions: &ExtensionPermissions,
    model: &str,
) -> Result<Self, ExtensionError> {
```

Inside, construct `HostState` with the new signature:

```rust
// Build HostState: cwd = project root, extra allowed = extension dir
let host_state = HostState::new(
    project_root.to_path_buf(),    // cwd = project root
    root_dir_buf.clone(),          // extension dir as extra allowed
    extra_allowed,
)
.with_commands(allow_commands)
.with_network(allow_network)
.with_model(model);
```

The module loader still uses `root_dir_buf` (the extension's own directory) for V8
import sandboxing — that's passed via `spawn_inner(specifier, js, Some(root_dir_buf), ...)`.

**Update the simpler `spawn_from_file()` too:**

```rust
pub fn spawn_from_file(entry_path: &Path, root_dir: &Path, project_root: &Path) -> Result<Self, ExtensionError> {
    Self::spawn_from_file_with_perms(entry_path, root_dir, project_root, &ExtensionPermissions::default(), "")
}
```

**Tests to update:** Every test that calls `spawn_from_file()` or `spawn_from_file_with_perms()`
needs the new `project_root` parameter. For most tests, the tempdir serves as both
the extension directory and the project root — just pass `dir.path()` twice.
Specifically affected tests in `runtime.rs`:
- ~15 call sites need the additional argument
- Integration tests in `rho-ext/tests/integration_test.rs` (~8 call sites)

**New tests:**
- `extension_can_read_project_file` — spawn extension in `~/.rho/extensions/`,
  create `Cargo.toml` in project root, assert `rho.readFile("Cargo.toml")` succeeds
- `extension_cwd_is_project_root` — eval `rho.getCwd()`, assert it returns project root
- `extension_get_project_root` — eval `rho.getProjectRoot()`, assert it returns project root
- `extension_cannot_read_outside_sandbox` — create a file outside both project and ext dir,
  assert `rho.readFile()` returns error

### Step 3: Thread project root through `ExtensionLoader` — WIRE phase

**File:** `rho-ext/src/loader.rs`

Add `project_root: PathBuf` field to `ExtensionLoader`:

```rust
pub struct ExtensionLoader {
    config: ExtensionConfig,
    project_root: PathBuf,     // NEW
    loaded: HashMap<String, LoadedState>,
}

impl ExtensionLoader {
    pub fn new(config: ExtensionConfig, project_root: PathBuf) -> Self {
        Self {
            config,
            project_root,
            loaded: HashMap::new(),
        }
    }
}
```

Update `spawn_one()`:

```rust
fn spawn_one(&mut self, name: &str, disc: &DiscoveredExtension, mtime: SystemTime) -> Result<(), ExtensionError> {
    let perms = self.config.permissions_for(name);
    let rt = ExtensionRuntime::spawn_from_file_with_perms(
        &disc.entry_path,
        &disc.root_dir,
        &self.project_root,    // ← NEW
        &perms,
        "",
    )?;
    // ... rest unchanged ...
}
```

Update `spawn_extensions()` — same change in the match arm.

**Tests to update:** Any test that constructs `ExtensionLoader::new(config)` needs the
additional `project_root` argument.

### Step 4: Wire sandbox root into `ExtensionLoader` from `App::build()`

**File:** `rho/src/app.rs`

Change the loader construction:

```rust
// Before:
let mut ext_loader = ExtensionLoader::new(config.extensions.clone());

// After:
let mut ext_loader = ExtensionLoader::new(config.extensions.clone(), sandbox.path().to_path_buf());
```

### Step 5: Add `rho.getProjectRoot()` to the JS shim

**File:** `rho-ext/src/host_shim.js`

Add the method to the `globalThis.rho` object:

```javascript
/**
 * Get the project root directory (the sandbox root).
 *
 * This is the same directory that all relative file paths resolve from.
 * Use this to construct absolute paths when needed.
 *
 * @returns {string} The project root path.
 */
getProjectRoot() {
    return ops.op_rho_get_project_root();
},
```

### Step 6: Update documentation

**File:** `rho-ext/types/rho.d.ts`

Add `getProjectRoot()` to the `RhoGlobal` interface.

**File:** `rho-ext/src/host.rs` doc comments

Update `rho.getCwd()` JSDoc to note it returns the project root (not the extension directory).
Update `rho.readFile()` and `rho.writeFile()` JSDoc to note paths resolve from the project root.

## What We Are NOT Changing

| Thing | Why not |
|---|---|
| `RhoModuleLoader` import sandboxing | V8 imports are a separate concern. Module imports resolve relative to `root_dir` (the extension directory), which is correct — extensions import their own helpers, not project files. This is handled in `module_loader.rs` and `spawn_inner()`. |
| `allowed_paths` config resolution | Currently `allow_paths` in extension config are resolved relative to `project_root` (via `root_dir.join(p)`). Since we're changing the constructor so `root_dir` is now explicitly the extension dir and project_root is separate, config-based `allow_paths` should be resolved relative to `project_root` (the new first param). This is already handled in the new constructor — `extra_allowed` entries are joined to `project_root`. |
| Built-in tool sandboxing | Built-in tools already work correctly — they use `SandboxRoot` directly and resolve paths against the project root. This change only affects extensions. |
| Extension `rho.runCommand()` sandboxing | That's Fix 3 (separate plan). We're not touching denylist or cwd for extension shell commands here. |

## Verification

After each step:
1. `cargo check` — compilation
2. `cargo clippy -- -D warnings` — lint compliance
3. `cargo test -p rho-ext` — all extension tests pass
4. `cargo test` — all workspace tests pass

After all steps:
5. `cargo xtask ci` — full CI pipeline

## Affected Files

| File | Change |
|---|---|
| `rho-ext/src/host.rs` | Add `project_root` to `HostState`, new constructor, `op_rho_get_project_root`, update extension definition |
| `rho-ext/src/host_shim.js` | Add `getProjectRoot()` to JS global |
| `rho-ext/src/runtime.rs` | Add `project_root` param to `spawn_from_file` and `spawn_from_file_with_perms`, update HostState construction |
| `rho-ext/src/loader.rs` | Add `project_root` field to `ExtensionLoader`, thread through `spawn_one` and `spawn_extensions` |
| `rho/src/app.rs` | Pass `sandbox.path()` to `ExtensionLoader::new()` |
| `rho-ext/types/rho.d.ts` | Add `getProjectRoot()` to type definitions |
| `rho-ext/src/runtime.rs` tests | ~15 call sites updated with new parameter |
| `rho-ext/tests/integration_test.rs` | ~8 call sites updated with new parameter |

## Order of Operations

```
Step 1: RED   — HostState project_root field + new constructor + op + failing compile
Step 2: GREEN — Update spawn_from_file_with_perms + fix all tests
Step 3:       — ExtensionLoader stores and threads project_root
Step 4:       — App passes sandbox root to loader
Step 5:       — JS shim getProjectRoot()
Step 6:       — Type definitions + doc comments
```
