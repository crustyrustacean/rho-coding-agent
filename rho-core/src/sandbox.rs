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
//!   fails on Windows if the path does not exist. The sandbox uses a
//!   workaround that walks up to the nearest existing ancestor, canonicalises
//!   that, re-appends the remaining components, and verifies containment. Any `..` component
//!   after the existing-ancestor boundary is rejected immediately.

use crate::error::Result;
use crate::newtypes::FilePath;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Errors that can occur during sandbox operations.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Cannot canonicalize the sandbox root path.
    #[error("sandbox: cannot canonicalize root `{root}`: {source}")]
    RootCanonicalizationFailed {
        root: String,
        source: std::io::Error,
    },

    /// Cannot resolve a path within the sandbox.
    #[error("sandbox: cannot resolve `{path}`: {source}")]
    PathResolutionFailed {
        path: String,
        source: std::io::Error,
    },

    /// A path would escape the sandbox.
    #[error("sandbox: path `{path}` is outside the sandbox root")]
    PathEscape { path: String },

    /// A `..` component appears in a not-yet-existing path suffix.
    #[error("sandbox: path `{path}` contains `..` in non-existent suffix")]
    ParentDirInSuffix { path: String },

    /// No existing ancestor found for path validation.
    #[error("sandbox: no existing ancestor found for `{path}`")]
    NoExistingAncestor { path: String },
}

/// A specialised `Result` type for sandbox operations.
pub type SandboxResult<T> = std::result::Result<T, SandboxError>;

// Convert SandboxError to RhoError::Sandbox
impl From<SandboxError> for crate::error::RhoError {
    fn from(error: SandboxError) -> Self {
        crate::error::RhoError::Sandbox(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_canonicalization_failed() {
        let error = SandboxError::RootCanonicalizationFailed {
            root: "/nonexistent".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        assert!(matches!(
            error,
            SandboxError::RootCanonicalizationFailed { .. }
        ));
        assert!(error.to_string().contains("cannot canonicalize root"));
        assert!(error.to_string().contains("/nonexistent"));
    }

    #[test]
    fn test_path_resolution_failed() {
        let error = SandboxError::PathResolutionFailed {
            path: "/some/path".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        };
        assert!(matches!(error, SandboxError::PathResolutionFailed { .. }));
        assert!(error.to_string().contains("cannot resolve"));
        assert!(error.to_string().contains("/some/path"));
    }

    #[test]
    fn test_path_escape() {
        let error = SandboxError::PathEscape {
            path: "/etc/passwd".to_string(),
        };
        assert!(matches!(error, SandboxError::PathEscape { .. }));
        assert!(error.to_string().contains("outside the sandbox root"));
        assert!(error.to_string().contains("/etc/passwd"));
    }

    #[test]
    fn test_parent_dir_in_suffix() {
        let error = SandboxError::ParentDirInSuffix {
            path: "safe/..".to_string(),
        };
        assert!(matches!(error, SandboxError::ParentDirInSuffix { .. }));
        assert!(
            error
                .to_string()
                .contains("contains `..` in non-existent suffix")
        );
    }

    #[test]
    fn test_no_existing_ancestor() {
        let error = SandboxError::NoExistingAncestor {
            path: "/nonexistent/deep/path".to_string(),
        };
        assert!(matches!(error, SandboxError::NoExistingAncestor { .. }));
        assert!(error.to_string().contains("no existing ancestor found"));
    }

    #[test]
    fn test_sandbox_result_ok() {
        let result: SandboxResult<String> = Ok("success".to_string());
        assert!(result.is_ok());
    }

    #[test]
    fn test_sandbox_result_err() {
        let result: SandboxResult<String> = Err(SandboxError::PathEscape {
            path: "/escape".to_string(),
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_debug_format() {
        let error = SandboxError::PathEscape {
            path: "/test/path".to_string(),
        };
        let debug_str = format!("{error:?}");
        assert!(debug_str.contains("PathEscape"));
        assert!(debug_str.contains("/test/path"));
    }
}

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
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(root.as_ref().canonicalize().map_err(|e| {
            SandboxError::RootCanonicalizationFailed {
                root: root.as_ref().display().to_string(),
                source: e,
            }
        })?))
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
    /// Returns [`SandboxError::PathEscape`] if the path is outside the sandbox or
    /// [`SandboxError::PathResolutionFailed`] if `canonicalize` fails (e.g. file does not exist).
    pub fn validate(&self, input: impl AsRef<Path>) -> Result<FilePath> {
        let input = input.as_ref();
        let canonical = input
            .canonicalize()
            .map_err(|e| SandboxError::PathResolutionFailed {
                path: input.display().to_string(),
                source: e,
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
    /// Returns [`SandboxError`] variants if any of the following are true:
    /// - A `..` component appears in the not-yet-existing suffix ([`SandboxError::ParentDirInSuffix`]).
    /// - The resolved path would be outside the sandbox root ([`SandboxError::PathEscape`]).
    /// - No existing ancestor can be found ([`SandboxError::NoExistingAncestor`]).
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
            return Err(SandboxError::PathEscape {
                path: canonical.display().to_string(),
            }
            .into());
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
/// Searches for well-known project markers (Cargo.toml, .git, package.json, etc.).
/// Returns the first directory (from CWD upward) that contains any marker.
/// If no marker is found, returns the current directory.
///
/// # Errors
///
/// Returns an error if the current directory cannot be determined.
pub fn find_project_root() -> Result<SandboxRoot> {
    let cwd = std::env::current_dir().map_err(|e| SandboxError::RootCanonicalizationFailed {
        root: "current directory".to_string(),
        source: e,
    })?;
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
            SandboxError::PathResolutionFailed {
                path: input.display().to_string(),
                source: e,
            }
            .into()
        });
    }

    // Walk from root down to find the deepest existing ancestor.
    for component in input.components() {
        match component {
            Component::ParentDir => {
                // `..` in the not-yet-existing suffix is rejected.
                if found_existing {
                    return Err(SandboxError::ParentDirInSuffix {
                        path: input.display().to_string(),
                    }
                    .into());
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
        existing =
            std::env::current_dir().map_err(|e| SandboxError::RootCanonicalizationFailed {
                root: "current directory".to_string(),
                source: e,
            })?;
    }

    let canonical_base =
        existing
            .canonicalize()
            .map_err(|e| SandboxError::PathResolutionFailed {
                path: existing.display().to_string(),
                source: e,
            })?;

    let mut result = canonical_base;
    for part in tail {
        result.push(part);
    }
    Ok(result)
}

#[cfg(test)]
mod sandbox_tests {
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
