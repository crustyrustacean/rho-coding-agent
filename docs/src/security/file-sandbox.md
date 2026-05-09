# File Sandbox

All file tools (`ReadFile`, `WriteFile`, `ListDir`, `EditFile`) operate within a sandbox root — a canonical directory that no tool operation can escape.

## SandboxRoot

```rust
let root = SandboxRoot::new("/home/user/project")?;
root.validate("src/main.rs")?;       // Ok — within sandbox
root.validate("../../etc/passwd")?;  // Err — escapes sandbox
```

The sandbox root is the project root by default, auto-detected by walking up from the current directory looking for markers (`.rho/config.toml`, `.git/`, `Cargo.toml`, `package.json`, etc.). It can be overridden with `--root <path>`.

## Validation strategies

### Existing paths

`validate()` resolves symlinks and `..` components via `std::fs::canonicalize`, then checks the canonical path starts with the canonical root. This prevents path traversal through symlinks or directory climbing.

### Not-yet-existing paths

`validate_for_write()` handles new files (where `canonicalize` would fail). It:

1. Walks up from the path until it finds an existing ancestor.
2. Canonicalises that ancestor.
3. Re-appends the remaining components.
4. Rejects any `..` component in the not-yet-existing suffix immediately.
5. Verifies the resolved path stays within the sandbox root.

This allows `WriteFile` to create new files within the project while still preventing escapes like `new_dir/../../../etc/passwd`.

## Opt-out

The sandbox can be disabled in config (not recommended):

```toml
[sandbox]
enabled = false
```

When disabled, all file tools still resolve paths but skip the containment check. This is intended only for environments where the user explicitly wants to operate outside the project directory.

## TOCTOU note

There is a theoretical time-of-check-time-of-use race between `validate_for_write` and the subsequent `create_dir_all` + `write`. This is accepted because:

- The threat model does not include concurrent adversarial filesystem modification.
- If the model has shell access, it can write directly via the shell — the sandbox only constrains the *tool* interface.
- The approval gate is the primary defense against model-initiated writes.
