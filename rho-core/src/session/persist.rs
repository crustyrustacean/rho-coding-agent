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

// ── Session discovery ──────────────────────────────────────────────────────────

/// Lightweight metadata about a saved session, extracted from its JSONL file.
///
/// Used by `list_sessions` and `find_latest_session` to enumerate sessions
/// without loading the full entry tree into memory.
#[derive(Clone, Debug)]
pub struct SessionMetadata {
    /// Session ID (8-char hex).
    pub id: String,
    /// When the session was created (from the JSONL header).
    pub created_at: std::time::SystemTime,
    /// The working directory the session was started in.
    pub cwd: PathBuf,
    /// Total number of entries in the file (line count minus header).
    pub entry_count: usize,
    /// Filesystem modification time — used for recency sorting.
    pub mtime: std::time::SystemTime,
    /// Full path to the JSONL file.
    pub path: PathBuf,
}

/// List all saved sessions for the given project directory, sorted by
/// modification time (most recent first).
///
/// Reads only the header line of each JSONL file — does not parse entry
/// lines. Files with corrupt or missing headers are skipped with a warning.
///
/// Returns an empty vector if the session directory doesn't exist.
pub fn list_sessions(cwd: &Path) -> Vec<SessionMetadata> {
    let hash = project_hash(cwd);
    let session_dir = dirs_home().join(".rho").join("sessions").join(hash);

    let Ok(entries) = std::fs::read_dir(&session_dir) else {
        return Vec::new();
    };

    let mut results: Vec<SessionMetadata> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            // Only consider .jsonl files.
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return None;
            }

            read_session_metadata(&path)
        })
        .collect();

    // Sort by filesystem mtime, most recent first.
    results.sort_by_key(|b| std::cmp::Reverse(b.mtime));
    results
}

/// Return the path to the most recent session for the given project directory.
///
/// Scans `~/.rho/sessions/<project-hash>/` and returns the JSONL file with
/// the most recent filesystem modification time. Returns `None` if the
/// directory doesn't exist or is empty.
pub fn find_latest_session(cwd: &Path) -> Option<PathBuf> {
    list_sessions(cwd).first().map(|m| m.path.clone())
}

/// Extract lightweight metadata from a single JSONL session file.
///
/// Reads only the header line and counts remaining non-empty lines.
/// Returns `None` if the file cannot be read or the header is corrupt.
fn read_session_metadata(path: &Path) -> Option<SessionMetadata> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);

    // Read and parse the header line.
    let mut header_line = String::new();
    let bytes = reader.read_line(&mut header_line).ok()?;
    if bytes == 0 {
        warn!(path = %path.display(), "empty session file, skipping");
        return None;
    }

    let header_jsonl: JsonlLine = serde_json::from_str(header_line.trim()).ok()?;
    let JsonlLine::Header {
        id,
        version: _,
        created_at_secs,
        cwd,
        parent_session: _,
    } = header_jsonl
    else {
        warn!(path = %path.display(), "first line is not a header, skipping");
        return None;
    };

    // Count remaining non-empty lines (entries).
    let mut entry_count: usize = 0;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if !line.trim().is_empty() {
                    entry_count += 1;
                }
            }
        }
    }

    // Get filesystem modification time.
    let mtime = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    Some(SessionMetadata {
        id,
        created_at: std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(created_at_secs),
        cwd: PathBuf::from(cwd),
        entry_count,
        mtime,
        path: path.to_path_buf(),
    })
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
/// Returns [`crate::error::RhoError`] if:
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
/// Returns [`crate::error::RhoError`] if:
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

#[cfg(test)]
#[allow(clippy::duration_suboptimal_units)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a fake session JSONL file in the given directory.
    ///
    /// Returns the path to the created file.
    fn create_test_session(dir: &Path, id: &str, created_at_secs: u64, entries: usize) -> PathBuf {
        let filename = format!("{created_at_secs}_{id}.jsonl");
        let path = dir.join(filename);
        let mut file = std::fs::File::create(&path).unwrap();
        let header = serde_json::json!({
            "type": "Header",
            "id": id,
            "version": 1,
            "created_at_secs": created_at_secs,
            "cwd": "/tmp/test",
            "parent_session": null
        });
        writeln!(file, "{header}").unwrap();
        for i in 0..entries {
            let entry_id = format!("{i:08x}");
            let entry = serde_json::json!({
                "type": "Entry",
                "id": entry_id,
                "parent_id": null,
                "timestamp": {"secs_since_epoch": created_at_secs, "nanos_since_epoch": 0},
                "resolution": "Full",
                "payload": {"type": "Message"}
            });
            writeln!(file, "{entry}").unwrap();
        }
        path
    }

    #[test]
    fn list_sessions_returns_empty_for_nonexistent_directory() {
        let cwd = Path::new("/tmp/nonexistent_rho_test_12345");
        let result = list_sessions(cwd);
        assert!(result.is_empty());
    }

    #[test]
    fn list_sessions_returns_empty_for_empty_directory() {
        let dir = std::env::temp_dir().join("rho_test_list_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let result = list_sessions(&dir);
        assert!(result.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_returns_sessions_sorted_by_mtime() {
        let base = std::env::temp_dir().join("rho_test_list_sorted");
        let _ = std::fs::remove_dir_all(&base);

        // Create a fake session directory with the right project hash.
        let hash = project_hash(&base);
        let session_dir = dirs_home().join(".rho").join("sessions").join(&hash);
        let _ = std::fs::remove_dir_all(&session_dir);
        std::fs::create_dir_all(&session_dir).unwrap();

        // Create two sessions with different timestamps.
        let _older = create_test_session(&session_dir, "aaa11111", 1000, 5);
        let _newer = create_test_session(&session_dir, "bbb22222", 2000, 10);

        // Touch the older file so it has a more recent mtime.
        let older_path = session_dir.join("1000_aaa11111.jsonl");
        let newer_path = session_dir.join("2000_bbb22222.jsonl");

        // On some systems, file creation order determines mtime.
        // Explicitly set mtimes to guarantee ordering.
        let older_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(50 * 60);
        let newer_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(67 * 60);
        std::fs::File::open(&older_path)
            .and_then(|f| f.set_modified(older_time))
            .ok();
        std::fs::File::open(&newer_path)
            .and_then(|f| f.set_modified(newer_time))
            .ok();

        let result = list_sessions(&base);
        assert_eq!(result.len(), 2);
        // Most recent mtime first.
        assert_eq!(result[0].id, "bbb22222");
        assert_eq!(result[1].id, "aaa11111");

        // Verify metadata is populated.
        assert_eq!(result[0].entry_count, 10);
        assert_eq!(result[1].entry_count, 5);
        assert_eq!(result[0].cwd, PathBuf::from("/tmp/test"));

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[test]
    fn find_latest_returns_none_when_no_sessions() {
        let cwd = Path::new("/tmp/nonexistent_rho_test_99999");
        assert!(find_latest_session(cwd).is_none());
    }

    #[test]
    fn find_latest_returns_most_recent_session() {
        let base = std::env::temp_dir().join("rho_test_find_latest");
        let _ = std::fs::remove_dir_all(&base);

        let hash = project_hash(&base);
        let session_dir = dirs_home().join(".rho").join("sessions").join(&hash);
        let _ = std::fs::remove_dir_all(&session_dir);
        std::fs::create_dir_all(&session_dir).unwrap();

        let _old = create_test_session(&session_dir, "ccc33333", 5000, 3);
        let _new = create_test_session(&session_dir, "ddd44444", 6000, 7);

        // Set mtimes so the second is definitively newer.
        let old_path = session_dir.join("5000_ccc33333.jsonl");
        let new_path = session_dir.join("6000_ddd44444.jsonl");
        let older_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(50 * 60);
        let newer_time =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(167 * 60);
        std::fs::File::open(&old_path)
            .and_then(|f| f.set_modified(older_time))
            .ok();
        std::fs::File::open(&new_path)
            .and_then(|f| f.set_modified(newer_time))
            .ok();

        let result = find_latest_session(&base);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("ddd44444"));

        let _ = std::fs::remove_dir_all(&session_dir);
    }

    #[test]
    fn list_sessions_skips_non_jsonl_files() {
        let base = std::env::temp_dir().join("rho_test_skip_files");
        let _ = std::fs::remove_dir_all(&base);

        let hash = project_hash(&base);
        let session_dir = dirs_home().join(".rho").join("sessions").join(&hash);
        let _ = std::fs::remove_dir_all(&session_dir);
        std::fs::create_dir_all(&session_dir).unwrap();

        // Create a valid session.
        let _ = create_test_session(&session_dir, "eee55555", 7000, 2);
        // Create a non-JSONL file.
        std::fs::write(session_dir.join("readme.txt"), "not a session").unwrap();
        // Create an empty file.
        std::fs::File::create(session_dir.join("empty.jsonl")).unwrap();

        let result = list_sessions(&base);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "eee55555");

        let _ = std::fs::remove_dir_all(&session_dir);
    }
}
