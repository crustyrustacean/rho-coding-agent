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
//! `Cursor::in_memory()` creates a session that skips all disk operations.
//! Used by tests and ephemeral sessions.

use crate::error::Result;
use crate::session::entry::{Entry, EntryResolution};
use crate::session::error::SessionError;
use crate::session::{Cursor, SessionHeader};
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
    /// A resolution change applied to an existing entry, by one cursor.
    ///
    /// Resolution changes happen after an entry is flushed (the entry is
    /// already on disk), so they cannot be folded back into the `Entry` line.
    /// Each change is appended as its own line; replay applies them in file
    /// order with last-write-wins **per cursor**.
    ///
    /// `cursor` identifies whose overlay the change belongs to. Two cursors
    /// over one log can resolve the same entry differently, so this field is
    /// what keeps their changes from overwriting each other. `None` marks a
    /// line written before cursors existed (format v2); those are adopted by
    /// whichever cursor reopens the file.
    Resolution {
        /// The entry whose resolution changed.
        entry: crate::newtypes::EntryId,
        /// The new resolution for that entry.
        resolution: crate::session::entry::EntryResolution,
        /// Cursor this change applies to. `None` for pre-cursor (v2) files.
        #[serde(default)]
        cursor: Option<String>,
    },
    /// A cursor's position in the log: which entries it is looking at.
    ///
    /// Written on every append, so the last `Cursor` line for a given id is
    /// that cursor's current leaf. Replay builds the full roster from these,
    /// which is how a reopened session knows about *every* cursor rather than
    /// inferring one from file order.
    ///
    /// Files written before cursors existed have no such lines; the reader
    /// then falls back to the pre-cursor behaviour of taking the last entry
    /// in the file as the single leaf.
    Cursor {
        /// The cursor's id.
        id: String,
        /// The entry this cursor's leaf points at.
        leaf: crate::newtypes::EntryId,
        /// Optional human label for the cursor (e.g. a branch name).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

/// A queued resolution change waiting to be written to disk.
///
/// Keyed on `(cursor, entry)` rather than entry alone: two cursors over one
/// log can hold different resolutions for the *same* entry, and both must
/// survive a round-trip. Keying on entry alone silently dropped one cursor's
/// change.
///
/// `cursor` is `None` only for a queued legacy line; in practice the queue
/// always stamps the writing cursor's id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingResolution {
    /// Queued changes, in insertion order. Replay is last-write-wins per
    /// `(cursor, entry)`, so only the final value for each key matters.
    changes: Vec<(
        Option<crate::newtypes::CursorId>,
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )>,
}

impl PendingResolution {
    /// Number of queued changes.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Drop every queued change.
    pub fn clear(&mut self) {
        self.changes.clear();
    }

    /// Queue a change, replacing any existing change for the same
    /// `(cursor, entry)` pair.
    ///
    /// Replacing rather than appending keeps the queue bounded when the
    /// eviction planner rewrites one entry several times before a flush. Order
    /// is preserved so replay's last-write-wins rule reproduces the in-memory
    /// sequence exactly.
    pub fn push(
        &mut self,
        cursor: Option<crate::newtypes::CursorId>,
        entry: crate::newtypes::EntryId,
        resolution: crate::session::entry::EntryResolution,
    ) {
        if let Some(slot) = self
            .changes
            .iter_mut()
            .find(|(c, e, _)| *c == cursor && *e == entry)
        {
            slot.2 = resolution;
        } else {
            self.changes.push((cursor, entry, resolution));
        }
    }

    /// The queued changes in insertion order.
    pub fn iter(
        &self,
    ) -> impl Iterator<
        Item = &(
            Option<crate::newtypes::CursorId>,
            crate::newtypes::EntryId,
            crate::session::entry::EntryResolution,
        ),
    > {
        self.changes.iter()
    }
}

/// A cursor's persisted position in the log.
///
/// Returned by [`Cursor::cursors`](crate::session::Cursor::cursors) so a
/// reopened session can enumerate every cursor it knows about, each with the
/// leaf it was last at.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CursorState {
    /// The cursor's id.
    pub id: String,
    /// The entry this cursor's leaf pointed at when last written.
    pub leaf: crate::newtypes::EntryId,
    /// Optional human label for the cursor.
    pub name: Option<String>,
}

/// Queued cursor-position updates awaiting a flush.
///
/// Like [`PendingResolution`], this is independent of `flushed_count`: a leaf
/// move applies to an entry that is already on disk. Dedupe is by cursor id —
/// only that cursor's *latest* position matters, and appending on every
/// append would otherwise grow the file without bound.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingCursors {
    /// Latest position per cursor, in first-seen order.
    entries: Vec<(String, crate::newtypes::EntryId, Option<String>)>,
}

impl PendingCursors {
    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every queued update.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Queue a cursor's current position, replacing any earlier one.
    pub fn push(&mut self, id: String, leaf: crate::newtypes::EntryId, name: Option<String>) {
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|(existing, _, _)| existing == &id)
        {
            slot.1 = leaf;
            slot.2 = name;
        } else {
            self.entries.push((id, leaf, name));
        }
    }

    /// The queued updates in first-seen cursor order.
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = &(String, crate::newtypes::EntryId, Option<String>)> {
        self.entries.iter()
    }
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
#[derive(Debug, Clone)]
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
    pub pending_resolution: PendingResolution,
    /// Cursor positions not yet written to disk.
    ///
    /// Independent of `flushed_count` for the same reason as
    /// `pending_resolution`: a cursor's leaf points at an entry that is
    /// already on disk, and the update must survive a crash.
    pub pending_cursors: PendingCursors,
}

impl PersistState {
    /// Create an in-memory persist state (no disk I/O).
    pub fn in_memory() -> Self {
        Self {
            save_path: None,
            flushed_count: 0,
            pending_resolution: PendingResolution::default(),
            pending_cursors: PendingCursors::default(),
        }
    }

    /// Create a file-backed persist state.
    pub fn with_path(path: PathBuf, flushed_count: usize) -> Self {
        Self {
            save_path: Some(path),
            flushed_count,
            pending_resolution: PendingResolution::default(),
            pending_cursors: PendingCursors::default(),
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
pub fn open_session(path: &Path) -> Result<Cursor> {
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

    // Reopening continues the *same* cursor, so it adopts the id found in the
    // file rather than minting a fresh one. Lines stamped with a different
    // cursor belong to a sibling and are skipped; lines with `cursor: null`
    // (written before cursors existed) belong to us and are adopted under a
    // fresh id, which is stable for the rest of this session's life.
    let persisted_cursor = first_cursor_id_in(&raw_lines);

    let ParsedBody {
        entries,
        append_order,
        last_entry_id,
        entry_count,
        resolution_changes,
        legacy_resolutions,
        cursor_states,
    } = parse_body_lines(path, &raw_lines)?;

    // Fold the `Cursor` lines into a roster, last line per cursor id winning.
    let mut roster: Vec<CursorState> = Vec::new();
    for state in cursor_states {
        if let Some(slot) = roster.iter_mut().find(|c| c.id == state.id) {
            *slot = state;
        } else {
            roster.push(state);
        }
    }

    // Reopening continues a specific cursor. Prefer one the file actually
    // recorded; fall back to the primary for a pre-cursor file.
    let self_cursor = persisted_cursor
        .or_else(|| roster.first().map(|c| c.id.clone().into()))
        .unwrap_or_default();

    let resolution_overlay = build_resolution_overlay(
        &entries,
        &resolution_changes,
        &legacy_resolutions,
        path,
        &self_cursor,
    );

    // Determine the leaf.
    //
    // A `Cursor` line is authoritative: it is the position that cursor was
    // actually at. Falling back to "last entry in the file" is the pre-cursor
    // behaviour and is only correct when no `Cursor` lines exist.
    let leaf = roster
        .iter()
        .find(|c| c.id == self_cursor.to_string())
        .map(|c| c.leaf.clone())
        .or(last_entry_id);

    // Build the session header.
    let header = SessionHeader {
        id: crate::newtypes::SessionId::from(session_id),
        version,
        created_at: std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(created_at_secs),
        cwd,
        parent_session: parent_session.map(PathBuf::from),
    };

    // We need to construct a Cursor, but Session has private fields.
    // We'll use a builder approach: create a minimal session and then
    // replace its internals.
    let session = Cursor::new_internal_with_overlay(
        header,
        &entries,
        append_order,
        leaf,
        PersistState::with_path(path.to_path_buf(), entry_count),
        resolution_overlay,
        crate::session::builder::CursorRoster {
            self_id: self_cursor,
            states: roster,
        },
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
pub fn flush_session(session: &mut Cursor) -> Result<()> {
    let persist = session.persist_state();
    let save_path = match &persist.save_path {
        Some(p) => p.clone(),
        None => return Ok(()), // in-memory mode
    };

    let total_entries = session.entry_count();
    let pending_resolution = session.persist_state().pending_resolution.clone();
    let pending_cursors = session.persist_state().pending_cursors.clone();

    if persist.flushed_count >= total_entries
        && pending_resolution.is_empty()
        && pending_cursors.is_empty()
    {
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
    write_cursor_lines(&mut writer, &pending_cursors, &save_path)?;

    writer.flush().map_err(|e| {
        SessionError::Persistence(format!(
            "failed to flush session file {}: {e}",
            save_path.display()
        ))
    })?;

    // Update flushed count and clear the queue only after a successful write.
    session.set_flushed_count(total_entries);
    session.clear_pending_resolution();
    session.clear_pending_cursors();

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
        JsonlLine::Entry(_) | JsonlLine::Resolution { .. } | JsonlLine::Cursor { .. } => {
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
fn write_header_line<W: Write>(writer: &mut W, session: &Cursor, save_path: &Path) -> Result<()> {
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
    changes: &PendingResolution,
    save_path: &Path,
) -> Result<()> {
    for (cursor, entry_id, resolution) in changes.iter() {
        let line = JsonlLine::Resolution {
            entry: entry_id.clone(),
            resolution: resolution.clone(),
            cursor: cursor.as_ref().map(std::string::ToString::to_string),
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

/// Serialise and write queued cursor positions, one line per cursor.
///
/// Like resolution changes, these are written *after* the entries so a
/// `Cursor` line never precedes the `Entry` it points at. Last write wins per
/// cursor id on replay.
fn write_cursor_lines<W: Write>(
    writer: &mut W,
    cursors: &PendingCursors,
    save_path: &Path,
) -> Result<()> {
    for (id, leaf, name) in cursors.iter() {
        let line = JsonlLine::Cursor {
            id: id.clone(),
            leaf: leaf.clone(),
            name: name.clone(),
        };
        let json = serde_json::to_string(&line).map_err(|e| {
            SessionError::Persistence(format!("failed to serialize cursor {id}: {e}"))
        })?;
        writeln!(writer, "{json}").map_err(|e| {
            SessionError::Persistence(format!(
                "failed to write cursor {id} to {}: {e}",
                save_path.display()
            ))
        })?;
    }
    Ok(())
}

/// Accumulated state from parsing a session file's body lines.
struct ParsedBody {
    /// Every entry, keyed by id.
    entries: HashMap<crate::newtypes::EntryId, Entry>,
    /// Entry ids in JSONL file order — the authoritative append order (#29).
    append_order: Vec<crate::newtypes::EntryId>,
    /// The last entry id seen in the file (the leaf).
    last_entry_id: Option<crate::newtypes::EntryId>,
    /// Number of `Entry` lines parsed.
    entry_count: usize,
    /// `Resolution` lines in file order, each tagged with the cursor it
    /// belongs to (`None` = written before cursors existed). The caller
    /// applies last-write-wins for its own cursor.
    resolution_changes: Vec<(
        Option<crate::newtypes::CursorId>,
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )>,
    /// v1 inline resolutions harvested from legacy `Entry` lines.
    legacy_resolutions: Vec<(
        crate::newtypes::EntryId,
        crate::session::entry::EntryResolution,
    )>,
    /// `Cursor` lines in file order. The last line for a cursor id wins, so
    /// the caller can fold them into a roster.
    cursor_states: Vec<CursorState>,
}

/// Parse the body lines of a session file (everything after the header).
///
/// Populates [`ParsedBody`]: entries, their file order, the leaf, and the
/// resolution replay inputs. `Resolution` lines are collected in file order so
/// the caller can apply them last-write-wins.
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
fn parse_body_lines(path: &Path, raw_lines: &[String]) -> Result<ParsedBody> {
    let mut body = ParsedBody {
        entries: HashMap::new(),
        append_order: Vec::new(),
        last_entry_id: None,
        entry_count: 0,
        resolution_changes: Vec::new(),
        legacy_resolutions: Vec::new(),
        cursor_states: Vec::new(),
    };
    let ParsedBody {
        entries,
        append_order,
        last_entry_id,
        entry_count,
        resolution_changes,
        legacy_resolutions,
        cursor_states,
    } = &mut body;

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
                // File order is the append order: the JSONL log is append-only,
                // so a later line is always a later append. Sorting by
                // timestamp instead would be non-deterministic on ties (#29).
                let id = last_entry_id.clone().expect("just set");
                if entries.insert(id.clone(), entry).is_none() {
                    append_order.push(id);
                }
                *entry_count += 1;
            }
            JsonlLine::Resolution {
                entry,
                resolution,
                cursor,
            } => {
                resolution_changes.push((cursor.map(Into::into), entry, resolution));
            }
            JsonlLine::Cursor { id, leaf, name } => {
                cursor_states.push(CursorState { id, leaf, name });
            }
        }
    }
    Ok(body)
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
    resolution_changes: &[(
        Option<crate::newtypes::CursorId>,
        crate::newtypes::EntryId,
        EntryResolution,
    )],
    legacy_resolutions: &[(crate::newtypes::EntryId, EntryResolution)],
    path: &Path,
    self_cursor: &crate::newtypes::CursorId,
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
    for (cursor, entry_id, resolution) in resolution_changes {
        // A line stamped with a sibling cursor's id must not leak into this
        // cursor's overlay. `None` (pre-cursor v2 files) belongs to us.
        if cursor.as_ref().is_some_and(|c| c != self_cursor) {
            continue;
        }
        apply(entry_id, resolution);
    }

    overlay
}

/// Recover the cursor id a session file was last written by.
///
/// Reopening *continues* the cursor that wrote the file, so it adopts this id
/// rather than minting a fresh one. Without this, a session's own stamped
/// `Resolution` lines would no longer match on reload and its overrides would
/// be silently dropped.
///
/// Returns `None` for a file with no stamped lines (v1, or a v2 file whose only
/// changes predate cursors), in which case the caller mints a fresh id and
/// adopts the `cursor: null` lines under it.
fn first_cursor_id_in(raw_lines: &[String]) -> Option<crate::newtypes::CursorId> {
    raw_lines
        .iter()
        .filter_map(|line| serde_json::from_str::<JsonlLine>(line).ok())
        .find_map(|line| match line {
            JsonlLine::Resolution { cursor, .. } => cursor,
            JsonlLine::Header { .. } | JsonlLine::Entry(_) | JsonlLine::Cursor { .. } => None,
        })
        .map(Into::into)
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

    // ── Cursor roster persistence (#65 step 5) ─────────────────────────

    /// A `Cursor` line round-trips its id and leaf.
    #[test]
    fn cursor_line_round_trips() {
        let line = JsonlLine::Cursor {
            id: crate::newtypes::CursorId::new().to_string(),
            leaf: EntryId::new(),
            name: Some("exploration".to_owned()),
        };
        let json = serde_json::to_string(&line).unwrap();
        let back: JsonlLine = serde_json::from_str(&json).unwrap();
        assert_eq!(line, back);
    }

    /// Reopening restores **both** cursors, each at its own leaf.
    ///
    /// Before cursor persistence, `open_session` took the leaf as "the last
    /// entry in the file". With two cursors that is only correct for whichever
    /// appended last — the other cursor's position was lost.
    #[test]
    fn reopen_restores_every_cursor_at_its_own_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cursors.jsonl");
        let mut primary = file_backed_session(path.clone());
        let root = primary.append_user_message("shared root");
        primary.append_user_message("primary turn");

        // A second cursor, branched back to the root and given its own turn.
        let mut forked = primary.fork();
        forked.branch_to(&root).unwrap();
        forked.append_user_message("forked turn");
        primary.flush().unwrap();
        forked.flush().unwrap();

        let reopened = Cursor::open(&path).unwrap();
        let cursors = reopened.cursors();
        assert_eq!(
            cursors.len(),
            2,
            "both cursors must survive the round-trip, got {cursors:?}"
        );

        let primary_id = primary.cursor_id().to_string();
        let forked_id = forked.cursor_id().to_string();
        let found_primary = cursors
            .iter()
            .find(|c| c.id == primary_id)
            .expect("primary cursor must be present");
        let found_forked = cursors
            .iter()
            .find(|c| c.id == forked_id)
            .expect("forked cursor must be present");

        assert_ne!(
            found_primary.leaf, found_forked.leaf,
            "the two cursors must be restored at different positions"
        );
    }

    /// The sibling cursor is preserved separately rather than merged into
    /// whichever cursor happened to write last.
    #[test]
    fn reopen_keeps_siblings_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leaf.jsonl");
        let mut primary = file_backed_session(path.clone());
        let root = primary.append_user_message("root");
        primary.append_user_message("primary turn");

        let mut forked = primary.fork();
        forked.branch_to(&root).unwrap();
        forked.append_user_message("forked turn");
        primary.flush().unwrap();
        forked.flush().unwrap();

        let reopened = Cursor::open(&path).unwrap();
        assert!(
            reopened.leaf().is_some(),
            "the reopened cursor must have a leaf"
        );
        assert_eq!(
            reopened.cursors().len(),
            2,
            "the sibling cursor must be preserved separately, not merged"
        );
    }

    /// A file with no `Cursor` lines still opens — the pre-cursor behaviour of
    /// taking the last entry as the leaf.
    #[test]
    fn legacy_file_without_cursor_lines_opens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.jsonl");
        let mut session = file_backed_session(path.clone());
        session.append_user_message("one");
        session.append_user_message("two");
        session.flush().unwrap();

        // Strip any Cursor lines to emulate a pre-cursor file.
        let contents = std::fs::read_to_string(&path).unwrap();
        let stripped = contents
            .lines()
            .filter(|line| !line.contains(r#""type":"Cursor""#))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{stripped}\n")).unwrap();

        let reopened = Cursor::open(&path).unwrap();
        assert!(reopened.leaf().is_some(), "must still open");
    }

    /// Appending through a cursor writes its `Cursor` line, so the on-disk
    /// roster is always current without a separate checkpoint step.
    #[test]
    fn appending_stamps_the_cursor_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stamp.jsonl");
        let mut session = file_backed_session(path.clone());
        session.append_user_message("hello");
        let cursor_id = session.cursor_id().to_string();
        session.flush().unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains(r#""type":"Cursor""#),
            "an append must stamp a Cursor line, got {contents}"
        );
        assert!(
            contents.contains(&cursor_id),
            "the Cursor line must carry this cursor's id"
        );
    }

    /// `restore_cursor` rebuilds a sibling at its persisted position.
    #[test]
    fn restore_cursor_rebuilds_sibling_at_its_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restore.jsonl");
        let mut primary = file_backed_session(path.clone());
        let root = primary.append_user_message("root");
        primary.append_user_message("primary turn");

        let mut forked = primary.fork();
        forked.branch_to(&root).unwrap();
        let forked_turn = forked.append_user_message("forked turn");
        let forked_id = forked.cursor_id().to_string();
        primary.flush().unwrap();
        forked.flush().unwrap();

        let reopened = Cursor::open(&path).unwrap();
        let restored = reopened.restore_cursor(&forked_id).unwrap();

        assert_eq!(restored.cursor_id().to_string(), forked_id);
        assert_eq!(
            restored.leaf(),
            Some(forked_turn),
            "the restored cursor must sit at the leaf it was persisted at"
        );
    }

    /// Restoring an unknown cursor is an error, not a silent new cursor.
    #[test]
    fn restore_cursor_rejects_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown.jsonl");
        let mut session = file_backed_session(path.clone());
        session.append_user_message("hello");
        session.flush().unwrap();

        let reopened = Cursor::open(&path).unwrap();
        assert!(reopened.restore_cursor("does-not-exist").is_err());
    }

    /// A restored cursor's resolution overlay is independent — a pin made on
    /// one cursor does not leak into a restored sibling.
    #[test]
    fn restore_cursor_has_independent_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlay.jsonl");
        let mut primary = file_backed_session(path.clone());
        let entry = primary.append_user_message("root");
        primary.flush().unwrap();

        let mut forked = primary.fork();
        forked.pin_entry(&entry).unwrap();
        let forked_id = forked.cursor_id().to_string();
        forked.flush().unwrap();

        let reopened = Cursor::open(&path).unwrap();
        let restored = reopened.restore_cursor(&forked_id).unwrap();
        assert_eq!(restored.resolution_of(&entry), EntryResolution::Full);
    }

    /// Build a file-backed [`Session`] rooted at `path`, using the internal
    /// constructor so tests can point persistence at a temp file.
    ///
    /// The session starts with a root system-message entry and no unwritten
    /// entries. Callers append via the normal public API to exercise the
    /// real flush path.
    fn file_backed_session(path: PathBuf) -> Cursor {
        let header = SessionHeader {
            id: crate::newtypes::SessionId::from("test0001"),
            version: SESSION_FORMAT_VERSION,
            created_at: std::time::SystemTime::UNIX_EPOCH,
            cwd: std::env::temp_dir(),
            parent_session: None,
        };
        Cursor::new_internal_with_overlay(
            header,
            &HashMap::new(),
            Vec::new(),
            None,
            PersistState::with_path(path, 0),
            HashMap::new(),
            crate::session::builder::CursorRoster::default(),
        )
    }

    /// Append a root system message plus a user turn, returning their IDs.
    ///
    /// Exercises the real append/flush path so the file exists on disk before
    /// a resolution change is applied.
    fn seed_two_entries(session: &mut Cursor) -> (EntryId, EntryId) {
        let root = session.append_user_message("system");
        let user = session.append_user_message("hello");
        (root, user)
    }

    // ── Per-cursor resolution persistence (#65 step 3) ──────────────────

    /// The load-bearing test: two cursors, one shared entry, different
    /// resolutions, both survive a flush and reopen.
    ///
    /// Before the queue gained a cursor dimension, `pending_resolution` deduped
    /// by entry id alone, so the second cursor's change replaced the first —
    /// one cursor's intent vanished with no error.
    #[test]
    fn two_cursors_persist_different_resolutions_for_same_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cursors.jsonl");
        let mut primary = file_backed_session(path.clone());
        let (_root, user_id) = seed_two_entries(&mut primary);

        // Two cursors, two different opinions about the same entry.
        primary.pin_entry(&user_id).unwrap();
        let mut forked = primary.fork();
        forked
            .set_resolution(
                &user_id,
                EntryResolution::Summarized {
                    summary: "only this cursor summarises it".to_owned(),
                },
            )
            .unwrap();
        primary.flush().unwrap();

        // Both changes are on disk, each stamped with its own cursor.
        let contents = std::fs::read_to_string(&path).unwrap();
        let resolution_lines: Vec<&str> = contents
            .lines()
            .filter(|line| line.contains(r#""type":"Resolution""#))
            .collect();
        assert_eq!(
            resolution_lines.len(),
            2,
            "both cursors' changes must reach disk, got {resolution_lines:?}"
        );
        assert!(
            resolution_lines
                .iter()
                .all(|line| !line.contains(r#""cursor":null"#)),
            "every v3 line must be cursor-stamped, got {resolution_lines:?}"
        );

        // Reopening yields the primary cursor, which must see its own pin.
        let reopened = Cursor::open(&path).unwrap();
        assert!(
            matches!(reopened.resolution_of(&user_id), EntryResolution::Pinned),
            "the primary cursor's pin must survive reopen"
        );
    }

    /// A `Resolution` line round-trips its cursor stamp.
    #[test]
    fn resolution_line_round_trips_with_cursor() {
        let line = JsonlLine::Resolution {
            entry: EntryId::new(),
            resolution: EntryResolution::Pinned,
            cursor: Some(crate::newtypes::CursorId::new().to_string()),
        };
        let json = serde_json::to_string(&line).unwrap();
        let back: JsonlLine = serde_json::from_str(&json).unwrap();
        assert_eq!(line, back);
        assert!(
            json.contains(r#""cursor":"#),
            "the cursor field must be present on the wire, got {json}"
        );
    }

    /// Files written before cursors existed carry `cursor: null`. Replay must
    /// apply those to the reopened session's own cursor, or every existing
    /// session silently loses its resolution overrides.
    #[test]
    fn legacy_resolution_line_without_cursor_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy_cursor.jsonl");
        let mut session = file_backed_session(path.clone());
        let (_root, user_id) = seed_two_entries(&mut session);
        session.pin_entry(&user_id).unwrap();
        session.flush().unwrap();

        // Rewrite the stamped cursor as an explicit null — the v2 shape.
        // `cursor` serialises as the last field, so the trailing `,"cursor":"…"`
        // is replaced with `,"cursor":null`.
        let contents = std::fs::read_to_string(&path).unwrap();
        let legacy = contents
            .lines()
            .map(|line| {
                if line.contains(r#""type":"Resolution""#) {
                    let cut = line
                        .find(r#","cursor":"#)
                        .unwrap_or_else(|| panic!("expected a cursor field: {line}"));
                    format!("{},\"cursor\":null}}", &line[..cut])
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{legacy}\n")).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains(r#""cursor":null"#),
            "fixture should now look like a v2 file"
        );

        let reopened = Cursor::open(&path).unwrap();
        assert!(
            matches!(reopened.resolution_of(&user_id), EntryResolution::Pinned),
            "a cursor-less Resolution line must still be honoured on reopen"
        );
    }

    /// Dedupe is keyed on (cursor, entry), not entry alone.
    #[test]
    fn dedupe_is_per_cursor_not_global() {
        use crate::newtypes::CursorId;

        let entry = EntryId::new();
        let cursor_a = CursorId::new();
        let cursor_b = CursorId::new();
        let mut queue = super::PendingResolution::default();

        queue.push(
            Some(cursor_a.clone()),
            entry.clone(),
            EntryResolution::Pinned,
        );
        queue.push(
            Some(cursor_b),
            entry.clone(),
            EntryResolution::Summarized {
                summary: "b".to_owned(),
            },
        );
        assert_eq!(
            queue.len(),
            2,
            "two cursors must retain separate changes for one entry"
        );

        // The same cursor re-writing the same entry replaces its own change.
        queue.push(Some(cursor_a), entry, EntryResolution::Attached);
        assert_eq!(
            queue.len(),
            2,
            "a same-cursor rewrite must dedupe, not append"
        );
    }

    /// `CursorId` follows the `EntryId` convention.
    #[test]
    fn cursor_id_is_unique_and_hex() {
        let a = crate::newtypes::CursorId::new();
        let b = crate::newtypes::CursorId::new();
        assert_ne!(a, b, "cursor ids must be unique");
        assert_eq!(a.to_string().len(), 16, "expected a 16-char hex prefix");
        assert!(a.to_string().chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// `fork()` yields a distinct cursor over the same log.
    #[test]
    fn fork_yields_independent_cursor_id() {
        let mut session = Cursor::in_memory("m", Some("sys"), vec![], "/tmp");
        session.append_user_message("first turn");
        let forked = session.fork();
        assert_ne!(
            session.cursor_id(),
            forked.cursor_id(),
            "a fork must get its own cursor id"
        );
    }

    /// Regression for #29: `append_order` was reconstructed by sorting a
    /// `HashMap`'s values by timestamp. When two entries share a timestamp the
    /// stable sort preserves whatever order the `HashMap` happened to iterate
    /// in, which is randomised per process — so a reload could produce a
    /// different append order than the file was written in.
    ///
    /// The fix uses JSONL file order as the append order. This test writes
    /// entries with *identical* timestamps in a known order and asserts the
    /// reload preserves it.
    #[test]
    fn reopen_preserves_file_order_when_timestamps_tie() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tied.jsonl");

        // Build a file by hand: three entries, byte-identical timestamps.
        let timestamp =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let header = serde_json::json!({
            "type": "Header",
            "id": "tie00001",
            "version": SESSION_FORMAT_VERSION,
            "created_at_secs": 1_700_000_000u64,
            "cwd": "/tmp/test",
            "parent_session": null
        });
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{header}").unwrap();

        // Deliberately not in id order: file order is b, a, c.
        let ids = ["bbbbbbbb", "aaaaaaaa", "cccccccc"];
        for (i, id) in ids.iter().enumerate() {
            let entry = Entry {
                id: EntryId::from(*id),
                parent_id: if i == 0 {
                    None
                } else {
                    Some(EntryId::from(ids[i - 1]))
                },
                timestamp,
                payload: crate::session::EntryPayload::Message(
                    crate::message::ChatMessage::user_text(format!("m{i}")),
                ),
            };
            let line = JsonlLine::Entry(entry);
            writeln!(file, "{}", serde_json::to_string(&line).unwrap()).unwrap();
        }
        drop(file);

        let session = open_session(&path).unwrap();
        let order: Vec<String> = session
            .entries_in_order()
            .iter()
            .map(|e| e.id.to_string())
            .collect();
        assert_eq!(
            order,
            vec!["bbbbbbbb", "aaaaaaaa", "cccccccc"],
            "reload must preserve JSONL file order, not a timestamp-sorted order"
        );
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

    /// A cursor's name must survive a reopen — it is part of the roster, and
    /// the roster is what `listBranches` reads after a restart.
    #[test]
    fn cursor_name_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("named.jsonl");
        let mut primary = file_backed_session(path.clone());
        primary.append_user_message("hello");
        let id = primary.cursor_id().to_string();
        primary.name_cursor(&id, "baseline").unwrap();
        primary.flush().unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("baseline"),
            "the Cursor line must carry the name; file was:
{contents}"
        );

        let reopened = Cursor::open(&path).unwrap();
        let name = reopened
            .cursors()
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.name);
        assert_eq!(
            name.as_deref(),
            Some("baseline"),
            "a reopened session must restore the cursor's name"
        );
    }

    /// Renaming after a position update still persists: the name is carried on
    /// the queued Cursor line, not only in the in-memory roster.
    #[test]
    fn cursor_rename_persists_despite_later_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rename.jsonl");
        let mut session = file_backed_session(path.clone());
        let id = session.cursor_id().to_string();
        session.append_user_message("first");
        session.name_cursor(&id, "renamed").unwrap();
        // An append re-queues the position; the name must survive it.
        session.append_user_message("second");
        session.flush().unwrap();

        let reopened = Cursor::open(&path).unwrap();
        let name = reopened
            .cursors()
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.name);
        assert_eq!(name.as_deref(), Some("renamed"));
    }
}
