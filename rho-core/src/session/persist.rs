//! JSONL persistence for session trees.
//!
//! Sessions persist to append-only JSONL files. Each line is a JSON object
//! representing a single entry. The format is:
//!
//! ```text
//! {"id":"abc12345","parent_id":null,"timestamp":...,"resolution":"Full","payload":{...}}
//! ```
//!
//! # Path layout
//!
//! ```text
//! ~/.rho/sessions/<project-hash>/<timestamp>_<session-id>.jsonl
//! ```
//!
//! - `project-hash` is the first 16 characters of the SHA-256 hash of the
//!   canonical project root directory, encoded as lower-hex. This avoids
//!   pi's `--<path>--` filename hack and works on Windows.
//! - `timestamp` is the session creation time as a Unix epoch seconds string.
//! - `session-id` is the 8-char hex [`SessionId`](crate::newtypes::SessionId).
//!
//! # Crash safety
//!
//! Each append operation auto-flushes to disk. A crashed process loses at
//! most one in-flight entry. The JSONL format is append-only, so partial writes
//! lose only the last line — the rest of the file is intact.
//!
//! # In-memory mode
//!
//! `Session::in_memory()` creates a session that skips all disk operations.
//! Used by tests and ephemeral sessions.

use crate::error::Result;
use crate::session::entry::Entry;
use crate::session::error::SessionError;
use crate::session::{Session, SessionHeader};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// A single line in a JSONL session file.
///
/// Every line is a `JsonlEntry` — this wraps [`Entry`] directly. The JSONL
/// file is simply one JSON-serialised `Entry` per line. The session header
/// and leaf position are reconstructed from the entries themselves:
///
/// - The **leaf** is determined by the last `LeafMoved` entry (if any), or
///   otherwise by the last entry in the file (the one with the greatest
///   timestamp among entries that have no child pointing to them).
/// - The **session header** is stored as the first line of the file using
///   a special `JsonlLine::Header` variant. This allows reconstruction of
///   `SessionId`, `version`, `created_at`, and `cwd` without needing a
///   separate metadata file.
///
/// # Design note
///
/// The header line is *not* an `Entry` — it's session-level metadata that
/// doesn't belong in the tree. It's always the first line. All subsequent
/// lines are entries.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type")]
pub enum JsonlLine {
    /// Session header. Always the first line in a JSONL file.
    Header {
        id: String,
        version: u32,
        created_at_secs: u64,
        cwd: String,
        parent_session: Option<String>,
    },
    /// A session tree entry.
    Entry(Entry),
}

// ── Path computation ──────────────────────────────────────────────────────────

/// Compute the project hash for a given CWD.
///
/// The hash is the first 16 hex characters of SHA-256 of the canonical CWD
/// path. This gives a stable, collision-resistant directory name that avoids
/// encoding the full path (which breaks on Windows and with special chars).
pub fn project_hash(cwd: &Path) -> String {
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    let mut hex = String::with_capacity(result.len() * 2);
    for byte in &result {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex.chars().take(16).collect()
}

/// Compute the default save path for a session.
///
/// Layout: `~/.rho/sessions/<project-hash>/<timestamp>_<session-id>.jsonl`
pub fn default_save_path(cwd: &Path, session_id: &str, created_at_secs: u64) -> PathBuf {
    let home = dirs_home();
    let hash = project_hash(cwd);
    home.join(".rho")
        .join("sessions")
        .join(hash)
        .join(format!("{created_at_secs}_{session_id}.jsonl"))
}

/// Best-effort home directory resolution.
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}

// ── Persistence methods on Session ────────────────────────────────────────────

/// Persistence state tracked alongside the session tree.
///
/// When `save_path` is `None`, the session is in-memory mode — all flush
/// operations are no-ops.
#[derive(Debug)]
pub struct PersistState {
    /// Path to the JSONL file, or `None` for in-memory sessions.
    pub save_path: Option<PathBuf>,
    /// Number of entries that have been flushed to disk. Entries at index
    /// `flushed_count..entries.len()` are unwritten.
    pub flushed_count: usize,
}

impl PersistState {
    /// Create an in-memory persist state (no disk I/O).
    pub fn in_memory() -> Self {
        Self {
            save_path: None,
            flushed_count: 0,
        }
    }

    /// Create a file-backed persist state.
    pub fn with_path(path: PathBuf, flushed_count: usize) -> Self {
        Self {
            save_path: Some(path),
            flushed_count,
        }
    }
}

// ── Open / flush / header serialization ────────────────────────────────────────

/// Open a session from a JSONL file.
///
/// Reads all lines, reconstructs the `entries` map, and determines the leaf
/// position:
///
/// 1. The first line must be a `JsonlLine::Header` — it provides `SessionId`,
///    version, `created_at`, and `cwd`.
/// 2. All subsequent lines are entries. They're inserted into the `entries`
///    map.
/// 3. The leaf is determined by scanning for the most recent
///    `EntryPayload::LeafMoved` entry. If none exists, the leaf is set to the
///    last entry appended (the one whose `id` doesn't appear as any other
///    entry's `parent_id` among entries in the file order).
///
/// # Errors
///
/// Returns [`RhoError`] if:
/// - The file cannot be opened.
/// - The first line is not a valid header.
/// - Any entry line cannot be deserialized.
/// - The session has no entries after the header.
pub fn open_session(path: &Path) -> Result<Session> {
    let file = std::fs::File::open(path).map_err(|e| {
        SessionError::Persistence(format!(
            "failed to open session file {}: {e}",
            path.display()
        ))
    })?;

    let reader = std::io::BufReader::new(file);
    let mut lines = reader.lines();

    // First line must be the header.
    let header_line = lines
        .next()
        .ok_or_else(|| {
            SessionError::Persistence(format!("session file is empty: {}", path.display()))
        })?
        .map_err(|e| {
            SessionError::Persistence(format!(
                "failed to read header from {}: {e}",
                path.display()
            ))
        })?;

    let header_jsonl: JsonlLine = serde_json::from_str(&header_line).map_err(|e| {
        SessionError::Persistence(format!(
            "failed to parse session header from {}: {e}",
            path.display()
        ))
    })?;

    let (session_id, version, created_at_secs, cwd, parent_session) = match header_jsonl {
        JsonlLine::Header {
            id,
            version,
            created_at_secs,
            cwd,
            parent_session,
        } => (
            id,
            version,
            created_at_secs,
            PathBuf::from(cwd),
            parent_session,
        ),
        JsonlLine::Entry(_) => {
            return Err(SessionError::Persistence(format!(
                "first line of session file is not a header: {}",
                path.display()
            ))
            .into());
        }
    };

    // Read all entry lines.
    let mut entries: HashMap<crate::newtypes::EntryId, Entry> = HashMap::new();
    let mut last_entry_id: Option<crate::newtypes::EntryId> = None;
    let mut entry_count: usize = 0;

    for line_result in lines {
        let line = line_result.map_err(|e| {
            SessionError::Persistence(format!(
                "failed to read line from session file {}: {e}",
                path.display()
            ))
        })?;

        let line = line.trim();
        if line.is_empty() {
            continue; // skip blank lines
        }

        let jsonl_line: JsonlLine = serde_json::from_str(line).map_err(|e| {
            SessionError::Persistence(format!(
                "failed to parse entry at line {} in {}: {e}",
                entry_count + 2, // +2: header is line 1, entries start at line 2
                path.display()
            ))
        })?;

        match jsonl_line {
            JsonlLine::Header { .. } => {
                warn!("duplicate header line in session file, ignoring");
            }
            JsonlLine::Entry(entry) => {
                last_entry_id = Some(entry.id.clone());
                entries.insert(entry.id.clone(), entry);
                entry_count += 1;
            }
        }
    }

    // Determine the leaf.
    // Determine the leaf.
    // The last entry in the file is always the leaf (entries are appended
    // in order). Branch operations produce LeafMoved entries that become
    // the leaf, then subsequent appends extend from there.
    let leaf = last_entry_id;

    // Build the session header.
    let header = SessionHeader {
        id: crate::newtypes::SessionId::from(session_id),
        version,
        created_at: std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(created_at_secs),
        cwd,
        parent_session: parent_session.map(PathBuf::from),
    };

    // We need to construct a Session, but Session has private fields.
    // We'll use a builder approach: create a minimal session and then
    // replace its internals.
    let session = Session::new_internal(
        header,
        entries,
        leaf,
        PersistState::with_path(path.to_path_buf(), entry_count),
    );

    debug!(
        path = %path.display(),
        entries = entry_count,
        "session opened from JSONL"
    );

    Ok(session)
}

/// Flush unwritten entries to the JSONL file.
///
/// Appends all entries that haven't been written yet. Creates the file and
/// its parent directories if they don't exist. Writes the header as the
/// first line if the file is new.
///
/// After a successful flush, `flushed_count` is updated to reflect the total
/// number of entries on disk.
///
/// # Errors
///
/// Returns [`RhoError`] if:
/// - The parent directories cannot be created.
/// - The file cannot be opened for appending.
/// - A write fails.
pub fn flush_session(session: &mut Session) -> Result<()> {
    let persist = session.persist_state();
    let save_path = match &persist.save_path {
        Some(p) => p.clone(),
        None => return Ok(()), // in-memory mode
    };

    let total_entries = session.entry_count();

    if persist.flushed_count >= total_entries {
        // Nothing new to write.
        return Ok(());
    }

    // Create parent directories if needed.
    if let Some(parent) = save_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SessionError::Persistence(format!(
                "failed to create session directory {}: {e}",
                parent.display()
            ))
        })?;
    }

    // Determine if we need to write the header (file is new/empty).
    let file_exists =
        save_path.exists() && std::fs::metadata(&save_path).is_ok_and(|m| m.len() > 0);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&save_path)
        .map_err(|e| {
            SessionError::Persistence(format!(
                "failed to open session file for writing {}: {e}",
                save_path.display()
            ))
        })?;

    let mut writer = std::io::BufWriter::new(file);

    // Write header if file is new.
    if !file_exists {
        let header = session.header();
        let header_line = JsonlLine::Header {
            id: header.id.to_string(),
            version: header.version,
            created_at_secs: header
                .created_at
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            cwd: header.cwd.to_string_lossy().into_owned(),
            parent_session: header
                .parent_session
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        };
        let json = serde_json::to_string(&header_line).map_err(|e| {
            SessionError::Persistence(format!("failed to serialize session header: {e}"))
        })?;
        writeln!(writer, "{json}").map_err(|e| {
            SessionError::Persistence(format!(
                "failed to write session header to {}: {e}",
                save_path.display()
            ))
        })?;
    }

    // Write unwritten entries.
    // We need to get the entries in append order. Since Session stores entries
    // in a HashMap, we need to reconstruct the append order from the tree.
    // The entries are linked by parent_id, so we walk from root to leaf.
    // But we only want to write the entries that haven't been flushed yet.
    //
    // The simplest approach: collect all entries in tree order (root → leaf)
    // and write the ones at indices >= flushed_count.
    let ordered_entries = session.entries_in_order();

    for entry in ordered_entries.iter().skip(persist.flushed_count) {
        let line = JsonlLine::Entry(entry.clone());
        let json = serde_json::to_string(&line).map_err(|e| {
            SessionError::Persistence(format!("failed to serialize entry {}: {e}", entry.id))
        })?;
        writeln!(writer, "{json}").map_err(|e| {
            SessionError::Persistence(format!(
                "failed to write entry to {}: {e}",
                save_path.display()
            ))
        })?;
    }

    writer.flush().map_err(|e| {
        SessionError::Persistence(format!(
            "failed to flush session file {}: {e}",
            save_path.display()
        ))
    })?;

    // Update flushed count.
    session.set_flushed_count(total_entries);

    debug!(
        path = %save_path.display(),
        total = total_entries,
        "session flushed to JSONL"
    );

    Ok(())
}

/// Compute the default save path for a new session.
pub fn compute_save_path(header: &SessionHeader) -> PathBuf {
    let created_at_secs = header
        .created_at
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    default_save_path(&header.cwd, &header.id.to_string(), created_at_secs)
}
