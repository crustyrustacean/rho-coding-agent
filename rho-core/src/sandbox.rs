//! File sandbox: [`SandboxRoot`] and validated [`FilePath`] construction.
//!
//! All file tools operate within a sandbox root so the model cannot read or
//! write files outside the project directory.
//!
//! # Validation strategy
//!
//! - **Existing paths** — [`std::fs::canonicalize`] resolves `..`, symlinks,
//!   and Windows junctions, then the canonical path is checked against the
//!   canonical root.
//! - **Not-yet-existing paths** (new files for `WriteFile`) — `canonicalize`
//!   fails on Windows if the path does not exist. [`canonicalize_for_write`]
//!   walks up to the nearest existing ancestor, canonicalises that, re-appends
//!   the remaining components, and verifies containment. Any `..` component
//!   after the existing-ancestor boundary is rejected immediately.

use crate::error::{Result, RhoError};
use crate::newtypes::FilePath;
use std::io;
use std::path::{Component, Path, PathBuf};

// ── SandboxRoot ───────────────────────────────────────────────────────────────

/// The canonical root of the file sandbox.
///
/// All validated [`FilePath`] values produced by this root are guaranteed to
/// reside within it. Tools that operate on files should hold a `SandboxRoot`
/// and call [`validate`] / [`validate_for_write`] before doing I/O.
///
/// [`validate`]: SandboxRoot::validate
/// [`validate_for_write`]: SandboxRoot::validate_for_write
#[derive(Clone, Debug)]
pub struct SandboxRoot(PathBuf);

impl SandboxRoot {
    /// Create a sandbox root from an existing directory.
    ///
    /// The path is canonicalised immediately so all subsequent comparisons are
    /// against the resolved form.
    ///
    /// # Errors
    ///
    /// Returns an error if the path does not exist or cannot be canonicalised.
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self(root.as_ref().canonicalize()?))
    }

    /// The canonical root path.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Validate that `input` points to an *existing* file within the sandbox.
    ///
    /// Resolves symlinks and `..` components via `canonicalize`, then verifies
    /// the resolved path starts with the canonical root.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError::Unexpected`] if the path is outside the sandbox or
    /// if `canonicalize` fails (e.g. file does not exist).
    pub fn validate(&self, input: impl AsRef<Path>) -> Result<FilePath> {
        let input = input.as_ref();
        let canonical = input.canonicalize().map_err(|e| {
            RhoError::Unexpected(anyhow::anyhow!(
                "sandbox: cannot resolve `{}`: {e}",
                input.display()
            ))
        })?;
        self.assert_within(&canonical)?;
        Ok(FilePath::from(canonical))
    }

    /// Validate a path that may **not yet exist** (e.g. a new file for `WriteFile`).
    ///
    /// Walks up from `input` until an existing ancestor is found, canonicalises
    /// that ancestor, re-appends the remaining components, and verifies the
    /// result is within the sandbox root. Any `..` component in the not-yet-
    /// existing suffix is rejected immediately.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError::Unexpected`] if any of the following are true:
    /// - A `..` component appears in the not-yet-existing suffix.
    /// - The resolved path would be outside the sandbox root.
    /// - No existing ancestor can be found.
    ///
    /// # TOCTOU assumption
    ///
    /// This validation walks up to the nearest existing ancestor,
    /// canonicalizes it, and verifies the would-be path stays within the
    /// sandbox root. There is a theoretical TOCTOU race: between this check
    /// and the subsequent `create_dir_all` + `write`, an attacker with
    /// concurrent filesystem access could plant a symlink at an intermediate
    /// component and redirect the write outside the sandbox.
    ///
    /// This is accepted because the threat model does not include concurrent
    /// adversarial filesystem modification. If the model has shell access and
    /// can plant symlinks, it can write directly via the shell — the sandbox
    /// only constrains the *tool* interface. The approval gate is the primary
    /// defense against model-initiated writes regardless of path.
    pub fn validate_for_write(&self, input: impl AsRef<Path>) -> Result<FilePath> {
        let canonical = canonicalize_for_write(input.as_ref())?;
        self.assert_within(&canonical)?;
        Ok(FilePath::from(canonical))
    }

    /// Returns `true` if `path` is within this sandbox root.
    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.0)
    }

    /// Assert that `canonical` is within the sandbox root, returning an error otherwise.
    fn assert_within(&self, canonical: &Path) -> Result<()> {
        if !canonical.starts_with(&self.0) {
            return Err(RhoError::Unexpected(anyhow::anyhow!(
                "sandbox violation: `{}` is outside sandbox root `{}`",
                canonical.display(),
                self.0.display()
            )));
        }
        Ok(())
    }
}

// ── find_project_root ─────────────────────────────────────────────────────────

/// Well-known project root markers, searched in priority order.
const PROJECT_MARKERS: &[&str] = &[
    ".rho/config.toml",
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
];

/// Auto-detect the project root by walking up from the current directory.
///
/// Searches for well-known project markers (see [`PROJECT_MARKERS`]).
/// Returns the first directory (from CWD upward) that contains any marker.
/// If no marker is found, returns the current directory.
///
/// # Errors
///
/// Returns an I/O error if the current directory cannot be determined.
pub fn find_project_root() -> io::Result<SandboxRoot> {
    let cwd = std::env::current_dir()?;
    let mut dir = cwd.as_path();

    loop {
        if PROJECT_MARKERS.iter().any(|m| dir.join(m).exists()) {
            return SandboxRoot::new(dir);
        }

        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            // Reached filesystem root without finding a marker.
            _ => return SandboxRoot::new(&cwd),
        }
    }
}

// ── canonicalize_for_write ────────────────────────────────────────────────────

/// Canonicalise a path that may not yet exist.
///
/// Walks up the path until an existing ancestor is found, canonicalises that
/// ancestor, then re-appends the remaining components (rejecting any `..`).
pub(crate) fn canonicalize_for_write(input: &Path) -> Result<PathBuf> {
    // Collect components, splitting at the first non-existent part.
    let mut existing = PathBuf::new();
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut found_existing = false;

    // If the full path already exists, just canonicalize normally.
    if input.exists() {
        return input.canonicalize().map_err(|e| {
            RhoError::Unexpected(anyhow::anyhow!(
                "sandbox: cannot canonicalize `{}`: {e}",
                input.display()
            ))
        });
    }

    // Walk from root down to find the deepest existing ancestor.
    for component in input.components() {
        match component {
            Component::ParentDir => {
                // `..` in the not-yet-existing suffix is rejected.
                if found_existing {
                    return Err(RhoError::Unexpected(anyhow::anyhow!(
                        "sandbox: `..` component after non-existent path segment in `{}`",
                        input.display()
                    )));
                }
                // Still in the existing portion — let canonicalize handle it.
                existing.push("..");
            }
            other => {
                if found_existing {
                    tail.push(other.as_os_str());
                } else {
                    let candidate = existing.join(other);
                    if candidate.exists() {
                        existing = candidate;
                    } else {
                        // First non-existent component — switch to tail mode.
                        found_existing = true;
                        tail.push(other.as_os_str());
                    }
                }
            }
        }
    }

    // If existing is empty (e.g. relative path with no existing ancestor),
    // try to canonicalize the current directory and use that as the base.
    if existing.as_os_str().is_empty() {
        existing = std::env::current_dir().map_err(|e| {
            RhoError::Unexpected(anyhow::anyhow!("sandbox: cannot get current dir: {e}"))
        })?;
    }

    let canonical_base = existing.canonicalize().map_err(|e| {
        RhoError::Unexpected(anyhow::anyhow!(
            "sandbox: cannot canonicalize ancestor `{}`: {e}",
            existing.display()
        ))
    })?;

    let mut result = canonical_base;
    for part in tail {
        result.push(part);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tmpdir() -> TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    #[test]
    fn validates_existing_file_inside_root() {
        let dir = tmpdir();
        let file = dir.path().join("foo.txt");
        fs::write(&file, "hello").unwrap();
        let root = SandboxRoot::new(dir.path()).unwrap();
        assert!(root.validate(&file).is_ok());
    }

    #[test]
    fn rejects_existing_file_outside_root() {
        let inside = tmpdir();
        let outside = tmpdir();
        let file = outside.path().join("evil.txt");
        fs::write(&file, "evil").unwrap();
        let root = SandboxRoot::new(inside.path()).unwrap();
        assert!(root.validate(&file).is_err());
    }

    #[test]
    fn validates_new_file_inside_root() {
        let dir = tmpdir();
        let new_file = dir.path().join("new.txt");
        let root = SandboxRoot::new(dir.path()).unwrap();
        assert!(root.validate_for_write(&new_file).is_ok());
    }

    #[test]
    fn rejects_new_file_outside_root_via_dotdot() {
        let dir = tmpdir();
        // dir/subdir/../../../etc/passwd  — escapes via ..
        let sneaky = dir
            .path()
            .join("sub")
            .join("..")
            .join("..")
            .join("evil.txt");
        let root = SandboxRoot::new(dir.path()).unwrap();
        // This either resolves inside (if sub exists) or hits the .. guard
        // Either way it must not escape the sandbox.
        let result = root.validate_for_write(&sneaky);
        // If it succeeds it must be within the root.
        if let Ok(fp) = result {
            assert!(root.contains(&fp));
        }
    }

    #[test]
    fn rejects_dotdot_in_non_existent_suffix() {
        let dir = tmpdir();
        // dir/nonexistent/../../../evil — the .. after nonexistent must fail.
        let sneaky = dir
            .path()
            .join("nonexistent_dir")
            .join("..")
            .join("evil.txt");
        let root = SandboxRoot::new(dir.path()).unwrap();
        assert!(root.validate_for_write(&sneaky).is_err());
    }
}
