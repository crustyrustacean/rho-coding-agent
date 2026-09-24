//! JSONL persistence for session trees.
//!
//! Sessions persist to append-only JSONL files. The first line is a `Header`;
//! every subsequent line is a tagged record — an `Entry`, or (format v2+) a
//! `Resolution` change against an already-written entry:
//!
//! ```text
//! {"type":"Header",...}
//! {"type":"Entry","id":"abc12345","parent_id":null,"timestamp":...,"resolution":"Full","payload":{...}}
//! {"type":"Resolution","entry":"abc12345","resolution":{"Outlined":{"outline":"..."}},"cursor":null}
//! ```
//!
//! `Entry` lines are append-only; resolution is changed by appending a
//! `Resolution` line rather than rewriting the entry. Replay applies those
//! changes in file order, last-write-wins.
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
//! # Format versions
//!
//! v1 carries resolution inside the `Entry` line. v2 (current,
//! [`SESSION_FORMAT_VERSION`]) adds the `Resolution` line so changes to
//! already-flushed entries survive a restart. v1 files remain readable;
//! files newer than this build understands are rejected rather than misread.
//!
//! # Crash safety
//!
//! Each append operation auto-flushes to disk. A crashed process loses at
//! most one in-flight entry. The JSONL format is append-only, so partial writes
//! lose only the last line — the rest of the file is intact. On open, an
//! unparsable *final* line is discarded as a torn write; an unparsable line
//! anywhere else is treated as corruption and fails the load.
//!
//! # In-memory mode
//!
//! `Session::in_memory()` creates a session that skips all disk operations.
//! Used by tests and ephemeral sessions.

use crate::error::Result;
use crate::session::entry::{Entry, EntryResolution};
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
//
// `Entry` is a session-tree node that intentionally carries rich inline
// payloads (a full `CompactionSummary`, message content blocks, etc.), so the
// `Entry` variant is much larger than `Header`. Boxing it (as clippy
// suggests) would add a heap indirection on every JSONL line read and write
// for little benefit; the size is bounded and these values are transient
// during (de)serialisation. Suppressed deliberately.
#[allow(clippy::large_enum_variant)]
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
    /// A resolution change applied to an existing entry.
    ///
    /// Resolution changes happen after an entry is flushed (the entry is
    /// already on disk), so they cannot be folded back into the `Entry` line.
    /// Each change is appended as its own line; replay applies them in file
    /// order with last-write-wins.
    ///
    /// `cursor` is reserved for the per-cursor overlay split (roadmap A3) and
    /// is always `None` in format v2. The field is present so stamping it
    /// later does not require a second format change.
    Resolution {
        /// The entry whose resolution changed.
        entry: crate::newtypes::EntryId,
        /// The new resolution for that entry.
        resolution: crate::session::entry::EntryResolution,
        /// Cursor this change applies to. Always `None` in format v2.
        #[serde(default)]
        cursor: Option<String>,
    },
}

/// Current on-disk session format version.
///
/// v2 adds the `JsonlLine::Resolution` variant so resolution changes to
/// already-flushed entries survive a restart. v1 files carry resolution
/// inside the `Entry` line and are still accepted.
pub const SESSION_FORMAT_VERSION: u32 = 2;

/// Highest on-disk format version this build can read.
pub const MAX_SUPPORTED_SESSION_VERSION: u32 = SESSION_FORMAT_VERSION;

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
    let filename = format!("{created_at_secs}_{session_id}.jsonl");
    session_dir_for(&dirs_home(), cwd).join(filename)
}

/// Best-effort home directory resolution.
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}

/// The sessions root directory under a given home: `<home>/.rho/sessions`.
fn sessions_root(home: &Path) -> PathBuf {
    home.join(".rho").join("sessions")
}

/// The per-project session directory for `cwd` under `home`:
/// `<home>/.rho/sessions/<project_hash(cwd)>`.
///
/// Taking `home` as a parameter (rather than always reading it from the
/// environment) lets tests isolate in a temp directory instead of the real
/// session store — see `list_sessions_in` / `find_latest_session_in`.
fn session_dir_for(home: &Path, cwd: &Path) -> PathBuf {
    sessions_root(home).join(project_hash(cwd))
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
    list_sessions_in(&dirs_home(), cwd)
}

/// Testable variant of [`list_sessions`] rooted at an explicit `home`, so
/// tests can isolate in a temp directory instead of the real session store
/// (which avoids global-env races and cross-test contention under the full
/// parallel suite, and keeps the real store clean).
fn list_sessions_in(home: &Path, cwd: &Path) -> Vec<SessionMetadata> {
    let session_dir = session_dir_for(home, cwd);

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
    find_latest_session_in(&dirs_home(), cwd)
}

/// Testable variant of [`find_latest_session`] rooted at an explicit `home`.
fn find_latest_session_in(home: &Path, cwd: &Path) -> Option<PathBuf> {
    list_sessions_in(home, cwd).first().map(|m| m.path.clone())
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
    /// Resolution changes not yet written to disk.
    ///
    /// Resolution changes apply to entries that are *already* on disk, so this
    /// queue is deliberately independent of `flushed_count`. `flush_session`
    /// drains it after writing any new entries — a `Resolution` line must
    /// never precede the `Entry` line it refers to.
    pub pending_resolution: Vec<(
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )>,
}

impl PersistState {
    /// Create an in-memory persist state (no disk I/O).
    pub fn in_memory() -> Self {
        Self {
            save_path: None,
            flushed_count: 0,
            pending_resolution: Vec::new(),
        }
    }

    /// Create a file-backed persist state.
    pub fn with_path(path: PathBuf, flushed_count: usize) -> Self {
        Self {
            save_path: Some(path),
            flushed_count,
            pending_resolution: Vec::new(),
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

    let (session_id, version, created_at_secs, cwd, parent_session) =
        parse_header_line(&header_line, path)?;

    // Reject formats newer than this build understands rather than silently
    // misreading them. v1 and v2 are both accepted.
    if version > MAX_SUPPORTED_SESSION_VERSION {
        return Err(SessionError::Persistence(format!(
            "session file {} has unsupported format version {version} (this build supports up to {MAX_SUPPORTED_SESSION_VERSION})",
            path.display()
        ))
        .into());
    }

    // Read all entry lines.
    let mut entries: HashMap<crate::newtypes::EntryId, Entry> = HashMap::new();
    let mut last_entry_id: Option<crate::newtypes::EntryId> = None;
    let mut entry_count: usize = 0;
    // Resolution changes in file order. Last write wins on replay.
    let mut resolution_changes: Vec<(
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )> = Vec::new();

    // Read the remaining lines eagerly so an unparsable *final* line can be
    // told apart from corruption in the middle of the file. A crash mid-append
    // leaves a partial last line; the JSONL contract treats the rest of the
    // file as intact.
    let raw_lines: Vec<String> =
        lines
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| {
                SessionError::Persistence(format!(
                    "failed to read line from session file {}: {e}",
                    path.display()
                ))
            })?;

    let mut legacy_resolutions: Vec<(crate::newtypes::EntryId, EntryResolution)> = Vec::new();
    parse_body_lines(
        path,
        &raw_lines,
        &mut entries,
        &mut last_entry_id,
        &mut entry_count,
        &mut resolution_changes,
        &mut legacy_resolutions,
    )?;

    let resolution_overlay =
        build_resolution_overlay(&entries, &resolution_changes, &legacy_resolutions, path);

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
    let session = Session::new_internal_with_overlay(
        header,
        entries,
        leaf,
        PersistState::with_path(path.to_path_buf(), entry_count),
        resolution_overlay,
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
    let pending_resolution = session.persist_state().pending_resolution.clone();

    if persist.flushed_count >= total_entries && pending_resolution.is_empty() {
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
        write_header_line(&mut writer, session, &save_path)?;
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

    // Write queued resolution changes *after* the entries, so a Resolution
    // line never precedes the Entry it refers to. Last write wins on replay,
    // so repeats for the same entry are safe and order-preserving.
    write_resolution_lines(&mut writer, &pending_resolution, &save_path)?;

    writer.flush().map_err(|e| {
        SessionError::Persistence(format!(
            "failed to flush session file {}: {e}",
            save_path.display()
        ))
    })?;

    // Update flushed count and clear the queue only after a successful write.
    session.set_flushed_count(total_entries);
    session.clear_pending_resolution();

    debug!(
        path = %save_path.display(),
        total = total_entries,
        "session flushed to JSONL"
    );

    Ok(())
}

/// Parse the header line of a session file.
///
/// Returns `(session_id, version, created_at_secs, cwd, parent_session)`.
///
/// # Errors
///
/// Returns [`crate::error::RhoError`] if the line does not deserialise or is
/// not a `Header` line.
fn parse_header_line(
    header_line: &str,
    path: &Path,
) -> Result<(String, u32, u64, PathBuf, Option<String>)> {
    let header_jsonl: JsonlLine = serde_json::from_str(header_line).map_err(|e| {
        SessionError::Persistence(format!(
            "failed to parse session header from {}: {e}",
            path.display()
        ))
    })?;

    match header_jsonl {
        JsonlLine::Header {
            id,
            version,
            created_at_secs,
            cwd,
            parent_session,
        } => Ok((
            id,
            version,
            created_at_secs,
            PathBuf::from(cwd),
            parent_session,
        )),
        JsonlLine::Entry(_) | JsonlLine::Resolution { .. } => {
            Err(SessionError::Persistence(format!(
                "first line of session file is not a header: {}",
                path.display()
            ))
            .into())
        }
    }
}

/// Serialise and write the session header line.
///
/// Used only when the target file is new or empty; an existing file already
/// carries its header from the first flush.
///
/// # Errors
///
/// Returns [`crate::error::RhoError`] if the header cannot be serialised or
/// written.
fn write_header_line<W: Write>(writer: &mut W, session: &Session, save_path: &Path) -> Result<()> {
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
    Ok(())
}

/// Serialise and write queued resolution changes, one line per change.
///
/// Order is preserved so replay's last-write-wins rule produces the same
/// result as the in-memory sequence of changes.
///
/// # Errors
///
/// Returns [`crate::error::RhoError`] if a change cannot be serialised or
/// written.
fn write_resolution_lines<W: Write>(
    writer: &mut W,
    changes: &[(
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )],
    save_path: &Path,
) -> Result<()> {
    for (entry_id, resolution) in changes {
        let line = JsonlLine::Resolution {
            entry: entry_id.clone(),
            resolution: resolution.clone(),
            cursor: None,
        };
        let json = serde_json::to_string(&line).map_err(|e| {
            SessionError::Persistence(format!(
                "failed to serialize resolution change for {entry_id}: {e}"
            ))
        })?;
        writeln!(writer, "{json}").map_err(|e| {
            SessionError::Persistence(format!(
                "failed to write resolution change to {}: {e}",
                save_path.display()
            ))
        })?;
    }
    Ok(())
}

/// Parse the body lines of a session file (everything after the header).
///
/// Populates `entries`, `last_entry_id`, and `entry_count` from `Entry`
/// lines, and appends `Resolution` changes to `resolution_changes` in file
/// order (the caller applies them last-write-wins).
///
/// # Torn-tail tolerance
///
/// A crash mid-append leaves a partial final line. That line is discarded with
/// a warning; every complete line before it is kept. An unparsable line
/// anywhere else is real corruption and fails the load.
///
/// # Errors
///
/// Returns [`crate::error::RhoError`] if a non-final line cannot be
/// deserialized.
fn parse_body_lines(
    path: &Path,
    raw_lines: &[String],
    entries: &mut HashMap<crate::newtypes::EntryId, Entry>,
    last_entry_id: &mut Option<crate::newtypes::EntryId>,
    entry_count: &mut usize,
    resolution_changes: &mut Vec<(
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )>,
    legacy_resolutions: &mut Vec<(
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )>,
) -> Result<()> {
    let last_line_index = raw_lines.len();

    for (line_index, raw_line) in raw_lines.iter().enumerate() {
        let physical_line_number = line_index + 2; // +2: header is line 1
        let line = raw_line.trim();
        if line.is_empty() {
            continue; // skip blank lines
        }

        let jsonl_line: JsonlLine = match serde_json::from_str(line) {
            Ok(parsed) => parsed,
            Err(e) => {
                // A crash mid-write leaves a partial final line. Tolerate it
                // and keep every complete line before it; an unparsable line
                // anywhere else is real corruption and fails the load.
                if line_index + 1 == last_line_index {
                    warn!(
                        path = %path.display(),
                        line = physical_line_number,
                        "discarding unparsable final line (torn write)"
                    );
                    break;
                }
                return Err(SessionError::Persistence(format!(
                    "failed to parse entry at line {physical_line_number} in {}: {e}",
                    path.display()
                ))
                .into());
            }
        };

        match jsonl_line {
            JsonlLine::Header { .. } => {
                warn!("duplicate header line in session file, ignoring");
            }
            JsonlLine::Entry(entry) => {
                // A v1 line carries `resolution` inline. The field is gone from
                // `Entry`, so pull it straight off the raw JSON to seed the
                // overlay. Unknown keys are ignored by serde, so a v2 line
                // without it simply yields `None`.
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(legacy) = raw.get("resolution")
                    && let Ok(resolution) = serde_json::from_value(legacy.clone())
                {
                    legacy_resolutions.push((entry.id.clone(), resolution));
                }
                *last_entry_id = Some(entry.id.clone());
                entries.insert(entry.id.clone(), entry);
                *entry_count += 1;
            }
            JsonlLine::Resolution {
                entry,
                resolution,
                cursor,
            } => {
                if cursor.is_some() {
                    // Per-cursor resolution is roadmap A3; this build has no
                    // cursor concept, so a stamped line cannot be applied.
                    warn!(
                        entry = %entry,
                        "resolution line carries a cursor id, which this build does not support; ignoring"
                    );
                    continue;
                }
                resolution_changes.push((entry, resolution));
            }
        }
    }
    Ok(())
}

/// Build the sparse resolution overlay for a session being opened.
///
/// Two sources feed the overlay:
///
/// 1. `Resolution` lines, applied in file order (last write wins). A line
///    naming an entry that is not in this file is skipped with a warning: the
///    entry may live in a session file this one was branched from.
/// 2. A v1 file's inline `resolution` key, read straight off the raw JSON
///    (the field no longer exists on `Entry`). It differs from the payload
///    default only in hand-migrated files: an organic v1 file carries each
///    entry's append-time default, because resolution changes were never
///    persisted before v2.
///
/// Entries whose effective resolution equals their payload default are left
/// out of the map, which is what keeps the overlay sparse.
fn build_resolution_overlay(
    entries: &HashMap<crate::newtypes::EntryId, Entry>,
    resolution_changes: &[(crate::newtypes::EntryId, EntryResolution)],
    legacy_resolutions: &[(crate::newtypes::EntryId, EntryResolution)],
    path: &Path,
) -> HashMap<crate::newtypes::EntryId, EntryResolution> {
    let mut overlay: HashMap<crate::newtypes::EntryId, EntryResolution> = HashMap::new();

    let mut apply = |entry_id: &crate::newtypes::EntryId, resolution: &EntryResolution| {
        let Some(entry) = entries.get(entry_id) else {
            warn!(
                entry = %entry_id,
                path = %path.display(),
                "resolution change references unknown entry; skipping"
            );
            return;
        };
        if *resolution == EntryResolution::default_for(&entry.payload) {
            // Back to the payload default — drop any earlier override.
            overlay.remove(entry_id);
        } else {
            overlay.insert(entry_id.clone(), resolution.clone());
        }
    };

    // `Resolution` lines win over an inline v1 value: they are the newer format
    // and a file may legitimately carry both.
    for (entry_id, resolution) in legacy_resolutions {
        apply(entry_id, resolution);
    }
    for (entry_id, resolution) in resolution_changes {
        apply(entry_id, resolution);
    }

    overlay
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
    use crate::message::ChatMessage;
    use crate::newtypes::EntryId;
    use crate::session::MechanicalCompactionStrategy;
    use crate::session::entry::EntryResolution;
    use std::io::Write;

    /// Build a file-backed [`Session`] rooted at `path`, using the internal
    /// constructor so tests can point persistence at a temp file.
    ///
    /// The session starts with a root system-message entry and no unwritten
    /// entries. Callers append via the normal public API to exercise the
    /// real flush path.
    fn file_backed_session(path: PathBuf) -> Session {
        let header = SessionHeader {
            id: crate::newtypes::SessionId::from("test0001"),
            version: SESSION_FORMAT_VERSION,
            created_at: std::time::SystemTime::UNIX_EPOCH,
            cwd: std::env::temp_dir(),
            parent_session: None,
        };
        Session::new_internal_with_overlay(
            header,
            HashMap::new(),
            None,
            PersistState::with_path(path, 0),
            HashMap::new(),
        )
    }

    /// Append a root system message plus a user turn, returning their IDs.
    ///
    /// Exercises the real append/flush path so the file exists on disk before
    /// a resolution change is applied.
    fn seed_two_entries(session: &mut Session) -> (EntryId, EntryId) {
        let root = session.append_user_message("system");
        let user = session.append_user_message("hello");
        (root, user)
    }

    // ── Resolution persistence regression tests (#61) ──────────────────

    /// Regression for the live bug: `outline_entry` mutates an already-flushed
    /// entry, so the change was previously lost on reopen.
    #[test]
    fn outline_survives_flush_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);

        session.outline_entry(&user).unwrap();
        drop(session);

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                reopened.resolution_of(&user),
                EntryResolution::Outlined { .. }
            ),
            "outlined resolution must survive flush + reopen"
        );
    }

    /// Regression for `summarize_entry` losing its mutation on reopen.
    #[test]
    fn summarize_survives_flush_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);

        session.summarize_entry(&user).unwrap();
        drop(session);

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                reopened.resolution_of(&user),
                EntryResolution::Summarized { .. }
            ),
            "summarized resolution must survive flush + reopen"
        );
    }

    /// Regression for the worst manifestation: after a restart, both the
    /// `Compaction` entry *and* the entries it replaced render into context.
    #[tokio::test]
    async fn compaction_survives_flush_and_reopen_without_duplicating_originals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        // sys, u1, a1, u2, a2, u3 (leaf) — enough turns for a compaction
        // range to accumulate above a tiny threshold.
        let _root = session.append_user_message("system");
        let u1 = session.append_user_message("first request");
        let _a1 = session.append_user_message("first answer");
        let _u2 = session.append_user_message("second request");
        let _a2 = session.append_user_message("second answer");
        let _u3 = session.append_user_message("current request");

        let compaction_id = session
            .compact_older_than(1, &MechanicalCompactionStrategy::new())
            .await
            .unwrap();
        // Explicit flush: `compact_older_than` must persist its resolution
        // changes, but exercise `flush` directly so this test isolates the
        // persistence question from the auto-flush wiring.
        session.flush().unwrap();
        drop(session);

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                &reopened.resolution_of(&u1),
                EntryResolution::Compacted { into } if into == &compaction_id
            ),
            "compacted entry must still point at its Compaction after reopen"
        );
    }

    /// `compact_older_than` must persist its own resolution changes without
    /// requiring an explicit `flush()` call. Catches the missing trailing
    /// flush in the compaction path specifically.
    #[tokio::test]
    async fn compact_older_than_persists_without_explicit_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let _root = session.append_user_message("system");
        let u1 = session.append_user_message("first request");
        let _a1 = session.append_user_message("first answer");
        let _u2 = session.append_user_message("second request");
        let _a2 = session.append_user_message("second answer");
        let _u3 = session.append_user_message("current request");

        session
            .compact_older_than(1, &MechanicalCompactionStrategy::new())
            .await
            .unwrap();
        // Deliberately no `flush()` — the Session value is dropped as-is.
        drop(session);

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                reopened.resolution_of(&u1),
                EntryResolution::Compacted { .. }
            ),
            "compact_older_than must flush its resolution changes itself"
        );
    }

    /// A reopened compacted session must not render both the Compaction entry
    /// and the original entries it replaced.
    #[tokio::test]
    async fn reopened_compaction_path_excludes_compacted_originals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let _root = session.append_user_message("system");
        let _u1 = session.append_user_message("first request");
        let _a1 = session.append_user_message("first answer");
        let _u2 = session.append_user_message("second request");
        let _a2 = session.append_user_message("second answer");
        let _u3 = session.append_user_message("current request");

        session
            .compact_older_than(1, &MechanicalCompactionStrategy::new())
            .await
            .unwrap();
        drop(session);

        let reopened = open_session(&path).unwrap();
        let messages = reopened.path_messages();
        let texts: Vec<String> = messages
            .iter()
            .filter_map(|m| match m {
                ChatMessage::User { content } => content.first().map(|block| match block {
                    crate::message::ContentBlock::Text { text } => text.clone(),
                }),
                _ => None,
            })
            .collect();
        assert!(
            !texts.iter().any(|t| t == "first request"),
            "compacted original must not render into context after reopen; got {texts:?}"
        );
    }

    /// Multiple `Resolution` lines for one entry: the last write wins.
    #[test]
    fn multiple_resolution_lines_for_one_entry_last_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);

        session.outline_entry(&user).unwrap();
        session.summarize_entry(&user).unwrap();
        drop(session);

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                reopened.resolution_of(&user),
                EntryResolution::Summarized { .. }
            ),
            "last Resolution line must win"
        );
    }

    /// An `Resolution` line referring to an entry id that does not exist in
    /// the file must not fail the load.
    #[test]
    fn resolution_line_for_unknown_entry_is_skipped_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, _user) = seed_two_entries(&mut session);
        drop(session);

        // Append a Resolution line for an entry that was never written.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let line = serde_json::json!({
            "type": "Resolution",
            "entry": "deadbeef",
            "resolution": {"Outlined": {"outline": "ghost"}},
            "cursor": null
        });
        writeln!(file, "{line}").unwrap();

        let reopened = open_session(&path).unwrap();
        assert_eq!(reopened.entry_count(), 2, "unknown entry must be skipped");
    }

    /// A torn final line (a crash mid-write) must not prevent opening.
    #[test]
    fn torn_final_entry_line_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        seed_two_entries(&mut session);
        drop(session);

        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"Entry\",\"id\":\"abc\",\"parent_id\":");
        std::fs::write(&path, text).unwrap();

        let reopened = open_session(&path).unwrap();
        assert_eq!(reopened.entry_count(), 2);
    }

    /// A torn final `Resolution` line must not prevent opening.
    #[test]
    fn torn_final_resolution_line_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);
        session.outline_entry(&user).unwrap();
        drop(session);

        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"Resolution\",\"entry\":");
        std::fs::write(&path, text).unwrap();

        let reopened = open_session(&path).unwrap();
        assert_eq!(reopened.entry_count(), 2);
    }

    /// Header versions newer than this build understands must be rejected
    /// rather than silently misread.
    #[test]
    fn future_header_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        seed_two_entries(&mut session);
        drop(session);

        let future = SESSION_FORMAT_VERSION + 1;
        let text = std::fs::read_to_string(&path).unwrap();
        let bumped: Vec<String> = text
            .lines()
            .map(|line| {
                if line.contains("\"type\":\"Header\"") {
                    line.replace(
                        &format!("\"version\":{SESSION_FORMAT_VERSION}"),
                        &format!("\"version\":{future}"),
                    )
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&path, format!("{}\n", bumped.join("\n"))).unwrap();

        let err = open_session(&path).unwrap_err();
        assert!(
            err.to_string().contains("version"),
            "error should mention version, got: {err}"
        );
    }

    /// v1 files with no `Resolution` lines must still open cleanly, and a
    /// hand-migrated v1 file (resolution embedded in the Entry line) must have
    /// that resolution honoured.
    #[test]
    fn v1_file_with_embedded_resolution_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);
        drop(session);

        // Case 1: a pure v1 file (Entry lines only, all resolutions Full).
        let pure_v1 = open_session(&path).unwrap();
        assert_eq!(pure_v1.entry_count(), 2);
        assert!(matches!(
            pure_v1.resolution_of(&user),
            EntryResolution::Full
        ));

        // Case 2: a hand-migrated v1 file — the entry's embedded resolution
        // is Outlined and there are no Resolution lines.
        let text = std::fs::read_to_string(&path).unwrap();
        let migrated_lines: Vec<String> = text
            .lines()
            .map(|line| {
                if line.contains(&user.to_string()) {
                    let value: serde_json::Value = serde_json::from_str(line).unwrap();
                    serde_json::json!({
                        "type": "Entry",
                        "id": user.to_string(),
                        "parent_id": value["parent_id"],
                        "timestamp": value["timestamp"],
                        "resolution": {"Outlined": {"outline": "hand migrated"}},
                        "payload": value["payload"],
                    })
                    .to_string()
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&path, format!("{}\n", migrated_lines.join("\n"))).unwrap();

        let migrated = open_session(&path).unwrap();
        assert!(
            matches!(
                &migrated.resolution_of(&user),
                EntryResolution::Outlined { outline } if outline == "hand migrated"
            ),
            "v1 embedded resolution must be honoured"
        );
    }

    /// A v1 file's embedded resolution seeds the overlay, so `resolution_of`
    /// reports it even though no `Resolution` line exists.
    #[test]
    fn v1_embedded_resolution_seeds_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);
        drop(session);

        // Rewrite as a hand-migrated v1 file: the resolution lives inside the
        // Entry line, and there are no Resolution lines at all.
        let text = std::fs::read_to_string(&path).unwrap();
        let migrated: Vec<String> = text
            .lines()
            .map(|line| {
                if line.contains(&user.to_string()) {
                    let value: serde_json::Value = serde_json::from_str(line).unwrap();
                    serde_json::json!({
                        "type": "Entry",
                        "id": user.to_string(),
                        "parent_id": value["parent_id"],
                        "timestamp": value["timestamp"],
                        "resolution": {"Outlined": {"outline": "hand migrated"}},
                        "payload": value["payload"],
                    })
                    .to_string()
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&path, format!("{}\n", migrated.join("\n"))).unwrap();

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                reopened.resolution_of(&user),
                EntryResolution::Outlined { outline } if outline == "hand migrated"
            ),
            "a v1 embedded resolution must seed the overlay"
        );
    }

    /// A v1 file whose entries all carry the payload default produces an
    /// empty overlay — the sparse-map invariant, and what keeps the map
    /// sparse rather than mirroring every entry.
    #[test]
    fn v1_all_default_produces_empty_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);
        drop(session);

        let reopened = open_session(&path).unwrap();
        assert!(
            reopened.resolution_overlay_is_empty(),
            "all-default v1 file should not populate the overlay"
        );
        assert_eq!(reopened.resolution_of(&user), EntryResolution::Full);
    }

    /// PR 3: a newly-written `Entry` line carries no `resolution` field —
    /// resolution lives only in `Resolution` lines. Opening such a file and
    /// writing a fresh entry must keep the file free of the legacy field.
    #[test]
    fn new_entry_lines_omit_the_resolution_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);
        drop(session);

        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines().filter(|l| l.contains("\"Entry\"")) {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(
                value.get("resolution").is_none(),
                "Entry lines must not carry an embedded resolution: {line}"
            );
        }

        // Reopen and append: the new line also omits it.
        let mut reopened = open_session(&path).unwrap();
        reopened.append_user_message("after");
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines().filter(|l| l.contains("\"Entry\"")) {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(
                value.get("resolution").is_none(),
                "Entry lines must not carry an embedded resolution: {line}"
            );
        }
        let _ = user;
    }

    /// PR 3: `Entry` no longer carries `resolution`, so a v1 file's embedded
    /// resolution is read by `serde` into a throwaway field and used only to
    /// seed the overlay. Once the field is gone for good this test documents
    /// that the migration path still works.
    #[test]
    fn v1_embedded_resolution_seeds_overlay_after_field_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user) = seed_two_entries(&mut session);
        drop(session);

        let text = std::fs::read_to_string(&path).unwrap();
        let migrated: Vec<String> = text
            .lines()
            .map(|line| {
                if line.contains(&user.to_string()) {
                    let value: serde_json::Value = serde_json::from_str(line).unwrap();
                    serde_json::json!({
                        "type": "Entry",
                        "id": user.to_string(),
                        "parent_id": value["parent_id"],
                        "timestamp": value["timestamp"],
                        "resolution": {"Outlined": {"outline": "hand migrated"}},
                        "payload": value["payload"],
                    })
                    .to_string()
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&path, format!("{}\n", migrated.join("\n"))).unwrap();

        let reopened = open_session(&path).unwrap();
        assert!(
            matches!(
                reopened.resolution_of(&user),
                EntryResolution::Outlined { outline } if outline == "hand migrated"
            ),
            "a v1 embedded resolution must seed the overlay even with the field removed"
        );
        // The value is reachable only through the overlay now.
        assert!(!reopened.resolution_overlay_is_empty());
    }

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

    /// Generate a unique temp dir path for a test, ensuring no hash
    /// collisions with parallel test runs.
    fn unique_test_dir(label: &str) -> PathBuf {
        let uuid = uuid::Uuid::new_v4();
        std::env::temp_dir().join(format!("rho_test_{label}_{uuid}"))
    }

    #[test]
    fn list_sessions_returns_empty_for_nonexistent_directory() {
        let cwd = Path::new("/tmp/nonexistent_rho_test_12345");
        let result = list_sessions(cwd);
        assert!(result.is_empty());
    }

    #[test]
    fn list_sessions_returns_empty_for_empty_directory() {
        let home = tempfile::tempdir().unwrap();
        let base = unique_test_dir("list_empty");
        let session_dir = session_dir_for(home.path(), &base);
        std::fs::create_dir_all(&session_dir).unwrap();

        let result = list_sessions_in(home.path(), &base);
        assert!(result.is_empty());
    }

    #[test]
    fn list_sessions_returns_sessions_sorted_by_mtime() {
        let home = tempfile::tempdir().unwrap();
        let base = unique_test_dir("list_sorted");
        let session_dir = session_dir_for(home.path(), &base);
        std::fs::create_dir_all(&session_dir).unwrap();

        let _older = create_test_session(&session_dir, "aaa11111", 1000, 5);
        let _newer = create_test_session(&session_dir, "bbb22222", 2000, 10);

        let older_path = session_dir.join("1000_aaa11111.jsonl");
        let newer_path = session_dir.join("2000_bbb22222.jsonl");
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

        let result = list_sessions_in(home.path(), &base);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "bbb22222");
        assert_eq!(result[1].id, "aaa11111");
        assert_eq!(result[0].entry_count, 10);
        assert_eq!(result[1].entry_count, 5);
        assert_eq!(result[0].cwd, PathBuf::from("/tmp/test"));
    }

    #[test]
    fn find_latest_returns_none_when_no_sessions() {
        let cwd = Path::new("/tmp/nonexistent_rho_test_99999");
        assert!(find_latest_session(cwd).is_none());
    }

    #[test]
    fn find_latest_returns_most_recent_session() {
        let home = tempfile::tempdir().unwrap();
        let base = unique_test_dir("find_latest");
        let session_dir = session_dir_for(home.path(), &base);
        std::fs::create_dir_all(&session_dir).unwrap();

        let _old = create_test_session(&session_dir, "ccc33333", 5000, 3);
        let _new = create_test_session(&session_dir, "ddd44444", 6000, 7);

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

        let result = find_latest_session_in(home.path(), &base);
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("ddd44444"));
    }

    #[test]
    fn list_sessions_skips_non_jsonl_files() {
        let home = tempfile::tempdir().unwrap();
        let base = unique_test_dir("skip_files");
        let session_dir = session_dir_for(home.path(), &base);
        std::fs::create_dir_all(&session_dir).unwrap();

        let _ = create_test_session(&session_dir, "eee55555", 7000, 2);
        std::fs::write(session_dir.join("readme.txt"), "not a session").unwrap();
        std::fs::File::create(session_dir.join("empty.jsonl")).unwrap();

        let result = list_sessions_in(home.path(), &base);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "eee55555");
    }
}
