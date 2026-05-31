//! Session header — identity, version, and origin metadata.
//!
//! [`SessionHeader`] carries the session's unique identifier, on-disk format
//! version, creation time, working directory, and optional parent-session link
//! (for forked sessions, Phase 5+).

use std::path::PathBuf;
use std::time::SystemTime;

use crate::newtypes::SessionId;

/// Metadata about a session's identity and origin.
///
/// `version` starts at 1 and will be incremented if the on-disk format changes
/// in a way that requires migration. `parent_session` links to a prior session
/// file if this session was forked from one (Phase 5+).
#[derive(Clone, Debug)]
pub struct SessionHeader {
    /// Unique identifier for this session.
    pub id: SessionId,
    /// On-disk format version (starts at 1).
    pub version: u32,
    /// When this session was created.
    pub created_at: SystemTime,
    /// The working directory the session was started in.
    pub cwd: PathBuf,
    /// Path to a parent session file, if this session was forked.
    pub parent_session: Option<PathBuf>,
}
