//! Session tree — adaptive-resolution conversation model.
//!
//! Replaces the flat `Vec<ChatMessage>` with a parent-linked tree of typed
//! entries, each carrying an explicit resolution level. See
//! [`entry`] for the core types.

pub mod entry;

pub use entry::{CompactionSummary, Entry, EntryPayload, EntryResolution};
