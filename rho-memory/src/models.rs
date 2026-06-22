//! Document model stored in the knowledge base.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single document in the knowledge base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Unique identifier (UUID v4).
    pub id: Uuid,
    /// Human-readable title.
    pub title: String,
    /// Full document content.
    pub content: String,
    /// SHA-256 hash of content, used for deduplication.
    pub content_hash: String,
    /// Organization tags.
    pub tags: Vec<String>,
    /// Arbitrary structured metadata.
    pub metadata: Option<serde_json::Value>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last modification timestamp.
    pub updated_at: DateTime<Utc>,
}

/// A search hit returned by [`Memory::search`](crate::Memory::search).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The matching document.
    pub document: Document,
    /// An excerpt of the content surrounding the match.
    pub excerpt: String,
}

/// Describes how to create a new document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRequest {
    /// Human-readable title.
    pub title: String,
    /// Full document content.
    pub content: String,
    /// Organization tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Arbitrary structured metadata.
    pub metadata: Option<serde_json::Value>,
}

/// Describes which fields to update on an existing document.
///
/// Omitted fields are left unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateRequest {
    /// New title.
    pub title: Option<String>,
    /// New content.
    pub content: Option<String>,
    /// New tags (replaces the full list).
    pub tags: Option<Vec<String>>,
    /// New metadata (replaces the full value).
    pub metadata: Option<serde_json::Value>,
}

/// Aggregate statistics about the knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    /// Total non-deleted documents.
    pub total_documents: i64,
    /// Total document links (reserved for future use).
    pub total_links: i64,
    /// Database file size in bytes.
    pub database_size_bytes: i64,
    /// Last time any document was updated.
    pub last_updated: DateTime<Utc>,
}
