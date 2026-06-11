//! File operation tools: [`ReadFile`], [`WriteFile`], [`ListDir`], [`EditFile`], [`BatchRead`].
//!
//! This module re-exports file tools from `file_ops` and `edit` submodules.

pub use crate::edit::EditFile;
pub use crate::file_ops::{BatchRead, ListDir, ReadFile, WriteFile};
pub use crate::hashline::{
    AnchorResolution, HashlineAnchor, HashlineEdit, HashlineOp, compute_line_hash,
    detect_regex_patterns, truncate_for_error,
};
