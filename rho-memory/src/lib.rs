//! `rho-memory` — Persistent knowledge base for the rho coding agent.
//!
//! A library crate providing structured document storage with full-text search,
//! content deduplication, and soft deletes over `SQLite` (FTS5).
//!
//! # Quick start
//!
//! ```ignore
//! use rho_memory::Memory;
//!
//! let mem = Memory::open_in_memory().await?;
//! let doc = mem.create("My doc", "Some content", &["tag".to_string()]).await?;
//! let results = mem.search("doc", None, 10, 0).await?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod brain;
mod db;
mod error;
mod models;

pub use brain::Memory;
pub use error::Error;
pub use models::{Document, SearchResult};
