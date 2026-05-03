//! Domain newtypes that prevent misuse at API boundaries.
//!
//! # Deref policy
//!
//! - [`FilePath`] → `Deref<Target = Path>`
//! - [`ToolName`], [`ToolCallId`], [`DiagnosticCode`] → `Deref<Target = str>`
//!
//! This avoids `.0` access at call sites while preserving type-level distinction.
//! `From` impls cover the common construction patterns.

use serde::{Deserialize, Serialize};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A file path within the project sandbox.
///
/// Phase 1a: pure type-safety wrapper. Sandbox enforcement (canonicalisation +
/// containment) is added in Phase 1b.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilePath(PathBuf);

impl FilePath {
    /// Create a validated `FilePath` that is guaranteed to lie within `root`.
    ///
    /// Delegates to [`SandboxRoot::validate`] for existing paths and
    /// [`SandboxRoot::validate_for_write`] for paths that may not yet exist.
    /// Use this form in all production code that touches the file system.
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`] if the path cannot be resolved or lies outside `root`.
    ///
    /// [`SandboxRoot::validate`]: crate::sandbox::SandboxRoot::validate
    /// [`SandboxRoot::validate_for_write`]: crate::sandbox::SandboxRoot::validate_for_write
    /// [`RhoError`]: crate::error::RhoError
    pub fn new(
        input: impl AsRef<std::path::Path>,
        root: &crate::sandbox::SandboxRoot,
    ) -> crate::error::Result<Self> {
        root.validate(input)
    }

    /// Create a validated `FilePath` for a file that may not yet exist.
    ///
    /// Delegates to [`SandboxRoot::validate_for_write`].
    ///
    /// # Errors
    ///
    /// Returns [`RhoError`] if the path would escape the sandbox.
    ///
    /// [`SandboxRoot::validate_for_write`]: crate::sandbox::SandboxRoot::validate_for_write
    /// [`RhoError`]: crate::error::RhoError
    pub fn new_for_write(
        input: impl AsRef<std::path::Path>,
        root: &crate::sandbox::SandboxRoot,
    ) -> crate::error::Result<Self> {
        root.validate_for_write(input)
    }

    /// Create an unchecked `FilePath` from any path-like value.
    ///
    /// No sandbox validation is performed. Use the `From` impls or this
    /// constructor only in tests or in code that has already validated the path
    /// by other means.
    pub fn unchecked(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}

impl Deref for FilePath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl From<PathBuf> for FilePath {
    fn from(p: PathBuf) -> Self {
        Self(p)
    }
}

impl From<&Path> for FilePath {
    fn from(p: &Path) -> Self {
        Self(p.to_path_buf())
    }
}

impl From<&str> for FilePath {
    fn from(s: &str) -> Self {
        Self(PathBuf::from(s))
    }
}

impl From<String> for FilePath {
    fn from(s: String) -> Self {
        Self(PathBuf::from(s))
    }
}

impl std::fmt::Display for FilePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.display().fmt(f)
    }
}

/// A tool's registered name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ToolName(String);

impl ToolName {
    /// Create a new `ToolName`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

impl Deref for ToolName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ToolName {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ToolName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A unique identifier for a model-issued tool call.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Create a new `ToolCallId`.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl Deref for ToolCallId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ToolCallId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ToolCallId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct EntryId(String);

impl EntryId {
    /// Generates a brand new `EntryId` using the first 8 chars of a UUID v4
    pub fn new() -> Self {
        // Instead of duplicating logic, we generate a Uuid and convert it
        Self::from(Uuid::new_v4())
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}

/// The idiomatic way to allow: let id: `EntryId` = `some_uuid.into()`;
impl From<Uuid> for EntryId {
    fn from(uuid: Uuid) -> Self {
        let bytes = uuid.as_bytes();
        // Efficiently extract the first 4 bytes as an 8-char hex string
        let prefix = format!(
            "{:08x}",
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        );
        Self(prefix)
    }
}

impl Deref for EntryId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for EntryId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for EntryId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A unique identifier for a session.
///
/// Like [`EntryId`], this is an 8-char hex prefix of a UUID v4, matching
/// pi's format. Stable across sessions and unique across processes.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct SessionId(String);

impl SessionId {
    /// Generates a brand new `SessionId` using the first 8 chars of a UUID v4.
    pub fn new() -> Self {
        Self::from(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for SessionId {
    fn from(uuid: Uuid) -> Self {
        let bytes = uuid.as_bytes();
        let prefix = format!(
            "{:08x}",
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        );
        Self(prefix)
    }
}

impl Deref for SessionId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<String> for SessionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A Rust compiler diagnostic code (e.g. `E0308`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    /// Create a new `DiagnosticCode`.
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }
}

impl Deref for DiagnosticCode {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for DiagnosticCode {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for DiagnosticCode {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn file_path_deref_to_path() {
        let fp = FilePath::from("/tmp/foo.rs");
        assert_eq!(fp.extension().unwrap(), "rs");
    }

    #[test]
    fn tool_name_deref_to_str() {
        let name = ToolName::from("read_file");
        assert_eq!(&*name, "read_file");
        assert_eq!(name.len(), 9);
    }

    #[test]
    fn tool_call_id_round_trips_serde() {
        let id = ToolCallId::from("call_abc123");
        let json = serde_json::to_string(&id).unwrap();
        let back: ToolCallId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn test_collision_sanity_check() {
        let mut seen_ids = HashSet::new();
        let iterations = 10_000;

        for _ in 0..iterations {
            let id = EntryId::new();

            assert!(
                seen_ids.insert(id),
                "Collision detected! This suggests the random generator or slicing logic is flawed."
            );
        }

        assert_eq!(seen_ids.len(), iterations);
    }

    #[test]
    fn entry_id_round_trips_serde() {
        let id = EntryId::from(Uuid::new_v4());
        let json = serde_json::to_string(&id).unwrap();
        let back = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn session_id_round_trips_serde() {
        let id = SessionId::from(Uuid::new_v4());
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn diagnostic_code_round_trips_serde() {
        let code = DiagnosticCode::from("E0308");
        let json = serde_json::to_string(&code).unwrap();
        let back: DiagnosticCode = serde_json::from_str(&json).unwrap();
        assert_eq!(code, back);
    }

    #[test]
    fn file_path_new_validated_accepts_file_inside_root() {
        use crate::sandbox::SandboxRoot;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("foo.txt");
        std::fs::write(&file, "hello").unwrap();
        let root = SandboxRoot::new(dir.path()).unwrap();

        let fp = FilePath::new(&file, &root);
        assert!(fp.is_ok(), "file inside root must be accepted");
    }

    #[test]
    fn file_path_new_validated_rejects_file_outside_root() {
        use crate::sandbox::SandboxRoot;

        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("evil.txt");
        std::fs::write(&file, "evil").unwrap();
        let root = SandboxRoot::new(inside.path()).unwrap();

        let fp = FilePath::new(&file, &root);
        assert!(fp.is_err(), "file outside root must be rejected");
    }

    #[test]
    fn file_path_unchecked_accepts_any_path() {
        let fp = FilePath::unchecked("/tmp/anything");
        assert_eq!(fp.to_str().unwrap(), "/tmp/anything");
    }
}
