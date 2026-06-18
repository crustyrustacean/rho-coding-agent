/// Error types for rho-memory.

use thiserror::Error;

/// All errors that rho-memory operations can return.
#[derive(Debug, Error)]
pub enum Error {
    /// A document with the given ID was not found.
    #[error("document {0} not found")]
    NotFound(String),

    /// An error from the SQLite database layer.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A JSON serialization/deserialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A date/time parsing error.
    #[error("timestamp parse error: {0}")]
    Timestamp(#[from] chrono::ParseError),

    /// An I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid or malformed input.
    #[error("invalid input: {0}")]
    Input(String),
}
