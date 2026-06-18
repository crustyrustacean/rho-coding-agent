/// The public knowledge-base API.
///
/// Wraps [`Database`] with a friendlier interface. Opens a SQLite file at the
/// given path (or in-memory) and provides async CRUD + search operations.

use crate::db::Database;
use crate::error::Error;
use crate::models::{CreateRequest, Document, SearchResult, Stats, UpdateRequest};

/// A persistent knowledge base backed by SQLite + FTS5.
///
/// # Example
///
/// ```ignore
/// use rho_memory::Memory;
///
/// let mem = Memory::open(std::path::Path::new("/path/to/brain.db")).await?;
/// let doc = mem.create(
///     "Steering Messages Plan",
///     "Design doc for mid-turn message injection...",
///     &["rho-coding-agent".into(), "architecture".into()],
/// ).await?;
/// # Ok::<(), rho_memory::Error>(())
/// ```
pub struct Memory {
    db: Database,
}

impl Memory {
    /// Open (or create) a file-backed knowledge base.
    pub async fn open(path: &std::path::Path) -> Result<Self, Error> {
        let db = Database::open_file(path).await?;
        Ok(Self { db })
    }

    /// Open an ephemeral in-memory knowledge base (useful for tests).
    pub async fn open_in_memory() -> Result<Self, Error> {
        let db = Database::in_memory().await?;
        Ok(Self { db })
    }

    /// Create a new document.
    ///
    /// If an existing non-deleted document has identical content (SHA-256 match),
    /// the existing document is returned instead of creating a duplicate.
    pub async fn create(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<Document, Error> {
        self.db
            .create_document(title, content, tags, None)
            .await
    }

    /// Create a new document from a [`CreateRequest`], including optional metadata.
    pub async fn create_with(&self, req: &CreateRequest) -> Result<Document, Error> {
        self.db
            .create_document(&req.title, &req.content, &req.tags, req.metadata.as_ref())
            .await
    }

    /// Get a document by its UUID.
    pub async fn get(&self, id: &uuid::Uuid) -> Result<Option<Document>, Error> {
        self.db.get_document(id).await
    }

    /// Update an existing document.
    ///
    /// Returns `None` if the document doesn't exist. Only fields present in
    /// `update` are changed; the rest are preserved.
    pub async fn update(
        &self,
        id: &uuid::Uuid,
        update: &UpdateRequest,
    ) -> Result<Option<Document>, Error> {
        self.db.update_document(id, update).await
    }

    /// Soft-delete a document. Returns `true` if a document was deleted.
    pub async fn delete(&self, id: &uuid::Uuid) -> Result<bool, Error> {
        self.db.delete_document(id).await
    }

    /// Full-text search across all non-deleted documents.
    ///
    /// * `query` — the FTS5 search string (wrapped in quotes internally).
    /// * `tags` — optional tag filter; only documents containing *all* listed
    ///   tags are returned.
    /// * `limit` / `offset` — pagination.
    ///
    /// Returns matching documents and the total hit count.
    pub async fn search(
        &self,
        query: &str,
        tags: Option<&[String]>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Document>, i64), Error> {
        self.db.search_documents(query, tags, limit, offset).await
    }

    /// Convenience: search and return [`SearchResult`]s with excerpts.
    pub async fn search_with_excerpts(
        &self,
        query: &str,
        tags: Option<&[String]>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<SearchResult>, i64), Error> {
        let (docs, total) = self.db.search_documents(query, tags, limit, offset).await?;
        let results = docs
            .into_iter()
            .map(|doc| {
                let excerpt = extract_excerpt(&doc.content, query);
                SearchResult { document: doc, excerpt }
            })
            .collect();
        Ok((results, total))
    }

    /// List all non-deleted documents with pagination (ordered by updated_at desc).
    pub async fn list(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Document>, i64), Error> {
        self.db.list_documents(limit, offset).await
    }

    /// Aggregate statistics.
    pub async fn stats(&self) -> Result<Stats, Error> {
        self.db.get_stats().await
    }
}

/// Extract a short excerpt from `content` around the first occurrence of
/// a word from `query`.
fn extract_excerpt(content: &str, query: &str) -> String {
    let content_lower = content.to_lowercase();
    let query_lower = query.to_lowercase();

    // Find the first query word that appears in the content.
    let match_pos = query_lower
        .split_whitespace()
        .filter_map(|word| {
            let pos = content_lower.find(word)?;
            Some(pos)
        })
        .min();

    let (start, end) = match match_pos {
        Some(pos) => {
            // Show ~80 chars around the match
            let context = 80;
            let start = pos.saturating_sub(context);
            let end = (pos + context + query.len()).min(content.len());
            (start, end)
        }
        None => (0, content.len().min(200)),
    };

    let mut excerpt = String::from(&content[start..end]);
    if start > 0 {
        excerpt.insert_str(0, "...");
    }
    if end < content.len() {
        excerpt.push_str("...");
    }
    excerpt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::CreateRequest;

    async fn fresh() -> Memory {
        Memory::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn create_and_get_round_trip() {
        let mem = fresh().await;
        let doc = mem
            .create("Title", "Content here", &["a".into(), "b".into()])
            .await
            .unwrap();

        assert!(!doc.id.is_nil());
        assert_eq!(doc.title, "Title");
        assert_eq!(doc.content, "Content here");
        assert_eq!(doc.tags, vec!["a", "b"]);

        let fetched = mem.get(&doc.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, doc.id);
        assert_eq!(fetched.content_hash, doc.content_hash);
    }

    #[tokio::test]
    async fn dedup_returns_existing_on_same_content() {
        let mem = fresh().await;
        let d1 = mem.create("T1", "same content", &[]).await.unwrap();
        let d2 = mem.create("T2", "same content", &[]).await.unwrap();
        // Should return the same document (same ID)
        assert_eq!(d1.id, d2.id);
    }

    #[tokio::test]
    async fn update_changes_only_provided_fields() {
        let mem = fresh().await;
        let doc = mem
            .create("Old", "content", &["tag1".into()])
            .await
            .unwrap();

        let updated = mem
            .update(
                &doc.id,
                &UpdateRequest {
                    title: Some("New".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(updated.title, "New");
        assert_eq!(updated.content, "content"); // unchanged
        assert_eq!(updated.tags, vec!["tag1"]); // unchanged
    }

    #[tokio::test]
    async fn update_nonexistent_returns_none() {
        let mem = fresh().await;
        let result = mem
            .update(
                &uuid::Uuid::new_v4(),
                &UpdateRequest {
                    title: Some("x".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn soft_delete_hides_from_search() {
        let mem = fresh().await;
        let doc = mem
            .create("To delete", "content", &["x".into()])
            .await
            .unwrap();

        assert!(mem.delete(&doc.id).await.unwrap());
        assert!(mem.get(&doc.id).await.unwrap().is_none());

        let (results, _) = mem.search("delete", None, 10, 0).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_finds_by_content() {
        let mem = fresh().await;
        mem.create("Doc A", "rust programming language", &["lang".into()])
            .await
            .unwrap();
        mem.create("Doc B", "python programming language", &["lang".into()])
            .await
            .unwrap();

        let (results, total) = mem.search("rust", None, 10, 0).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(results[0].title, "Doc A");
    }

    #[tokio::test]
    async fn search_filters_by_tags() {
        let mem = fresh().await;
        mem.create("Doc A", "content", &["rust".into(), "agent".into()])
            .await
            .unwrap();
        mem.create("Doc B", "content", &["python".into(), "agent".into()])
            .await
            .unwrap();

        let (results, total) = mem
            .search("", Some(&["rust".into()]), 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(results[0].title, "Doc A");
    }

    #[tokio::test]
    async fn list_with_pagination() {
        let mem = fresh().await;
        for i in 0..5 {
            mem.create(&format!("Doc {i}"), &format!("content {i}"), &[])
                .await
                .unwrap();
        }

        let (page1, total) = mem.list(2, 0).await.unwrap();
        assert_eq!(total, 5);
        assert_eq!(page1.len(), 2);

        let (page2, _) = mem.list(2, 2).await.unwrap();
        assert_eq!(page2.len(), 2);

        // Pages should not overlap
        let page1_ids: Vec<_> = page1.iter().map(|d| d.id).collect();
        let page2_ids: Vec<_> = page2.iter().map(|d| d.id).collect();
        assert!(page1_ids.iter().all(|id| !page2_ids.contains(id)));
    }

    #[tokio::test]
    async fn create_with_metadata() {
        let mem = fresh().await;
        let doc = mem
            .create_with(&CreateRequest {
                title: "Meta Doc".into(),
                content: "body".into(),
                tags: vec!["test".into()],
                metadata: Some(serde_json::json!({"version": 2})),
            })
            .await
            .unwrap();

        assert_eq!(doc.metadata.as_ref().unwrap()["version"], 2);
    }

    #[tokio::test]
    async fn stats_reflects_document_count() {
        let mem = fresh().await;
        mem.create("A", "a", &[]).await.unwrap();
        mem.create("B", "b", &[]).await.unwrap();

        let stats = mem.stats().await.unwrap();
        assert_eq!(stats.total_documents, 2);
        assert!(stats.database_size_bytes > 0);
    }

    #[tokio::test]
    async fn search_with_excerpts_includes_context() {
        let mem = fresh().await;
        mem.create("Find me", "The steering messages feature allows mid-turn correction", &[])
            .await
            .unwrap();

        let (results, _) = mem
            .search_with_excerpts("steering", None, 10, 0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].excerpt.contains("steering"));
    }
}
