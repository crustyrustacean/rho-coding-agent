/// SQLite database operations backing the knowledge base.

use crate::error::Error;
use crate::models::{Document, Stats, UpdateRequest};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

/// The raw database handle. Prefer [`Memory`](crate::Memory) as the public API.
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Open an in-memory database (for tests).
    pub async fn in_memory() -> Result<Self, Error> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        let db = Self { pool };
        db.run_migrations().await?;
        Ok(db)
    }

    /// Open (or create) a database at the given file path.
    ///
    /// Creates parent directories automatically.
    pub async fn open_file(path: &std::path::Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await?;
        let db = Self { pool };
        db.run_migrations().await?;
        Ok(db)
    }

    async fn run_migrations(&self) -> Result<(), Error> {
        sqlx::raw_sql(include_str!("../migrations/001_initial.sql"))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Create a new document, deduplicating on content hash.
    pub async fn create_document(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
        metadata: Option<&serde_json::Value>,
    ) -> Result<Document, Error> {
        let content_hash = Self::compute_hash(content);

        // Dedup: return existing document if content is identical.
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM documents WHERE content_hash = ?1 AND is_deleted = 0")
                .bind(&content_hash)
                .fetch_optional(&self.pool)
                .await?;

        if let Some((id_str,)) = existing {
            let id = Uuid::parse_str(&id_str).map_err(|e| Error::Input(e.to_string()))?;
            return self.get_document(&id).await?.ok_or_else(|| {
                Error::Database(sqlx::Error::RowNotFound)
            });
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(tags)?;
        let metadata_json = metadata.map(|m| serde_json::to_string(m)).transpose()?;

        sqlx::query(
            "INSERT INTO documents (id, title, content, content_hash, tags, metadata, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&id)
        .bind(title)
        .bind(content)
        .bind(&content_hash)
        .bind(&tags_json)
        .bind(&metadata_json)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        self.get_document_by_id(&id)
            .await?
            .ok_or_else(|| Error::Database(sqlx::Error::RowNotFound))
    }

    /// Get a document by UUID.
    pub async fn get_document(&self, id: &Uuid) -> Result<Option<Document>, Error> {
        self.get_document_by_id(&id.to_string()).await
    }

    /// Update an existing document. Omitted fields in `update` are left unchanged.
    pub async fn update_document(
        &self,
        id: &Uuid,
        update: &UpdateRequest,
    ) -> Result<Option<Document>, Error> {
        let existing = match self.get_document(id).await? {
            Some(doc) => doc,
            None => return Ok(None),
        };

        let new_title = update.title.as_deref().unwrap_or(&existing.title);
        let new_content = update.content.as_deref().unwrap_or(&existing.content);
        let new_tags = update.tags.as_ref().unwrap_or(&existing.tags);
        let new_metadata = update.metadata.as_ref().or(existing.metadata.as_ref());
        let new_content_hash = if update.content.is_some() {
            Self::compute_hash(new_content)
        } else {
            existing.content_hash.clone()
        };

        let now = Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(new_tags)?;
        let metadata_json = new_metadata.map(|m| serde_json::to_string(m)).transpose()?;

        sqlx::query(
            "UPDATE documents
             SET title = ?1, content = ?2, content_hash = ?3, tags = ?4, metadata = ?5, updated_at = ?6
             WHERE id = ?7",
        )
        .bind(new_title)
        .bind(new_content)
        .bind(&new_content_hash)
        .bind(&tags_json)
        .bind(&metadata_json)
        .bind(&now)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;

        self.get_document(id).await
    }

    /// Soft-delete a document.
    pub async fn delete_document(&self, id: &Uuid) -> Result<bool, Error> {
        let result = sqlx::query("UPDATE documents SET is_deleted = 1, updated_at = ?1 WHERE id = ?2")
            .bind(Utc::now().to_rfc3339())
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Full-text search with optional tag filtering and pagination.
    ///
    /// Returns matching documents and the total hit count.
    pub async fn search_documents(
        &self,
        query: &str,
        tags: Option<&[String]>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Document>, i64), Error> {
        let mut where_conditions = vec!["d.is_deleted = 0".to_string()];
        let mut bind_params: Vec<String> = Vec::new();
        let mut param_count = 0;

        if !query.is_empty() {
            param_count += 1;
            where_conditions.push(format!(
                "d.id IN (SELECT id FROM documents_fts WHERE documents_fts MATCH ?{})",
                param_count
            ));
            bind_params.push(Self::escape_fts_query(query));
        }

        if let Some(tag_list) = tags {
            for tag in tag_list {
                param_count += 1;
                where_conditions.push(format!("d.tags LIKE ?{}", param_count));
                bind_params.push(format!("%\"{}\"%", tag));
            }
        }

        let where_clause = where_conditions.join(" AND ");

        // Count
        let count_sql = format!(
            "SELECT COUNT(*) as cnt FROM documents d WHERE {}",
            where_clause
        );
        let mut count_query = sqlx::query_scalar::<_, i64>(&count_sql);
        for param in &bind_params {
            count_query = count_query.bind(param);
        }
        let total_count = count_query.fetch_one(&self.pool).await.unwrap_or(0);

        // Paginated results
        let sql = format!(
            "SELECT d.id, d.title, d.content, d.content_hash, d.tags, d.metadata,
                    d.created_at, d.updated_at
             FROM documents d
             WHERE {}
             ORDER BY d.updated_at DESC
             LIMIT ?{} OFFSET ?{}",
            where_clause,
            param_count + 1,
            param_count + 2
        );

        let mut query = sqlx::query(&sql);
        for param in &bind_params {
            query = query.bind(param);
        }
        query = query.bind(limit as i64);
        query = query.bind(offset as i64);

        let rows = query.fetch_all(&self.pool).await?;

        let documents = rows
            .into_iter()
            .map(|row| {
                let id: String = row.get("id");
                let title: String = row.get("title");
                let content: String = row.get("content");
                let content_hash: String = row.get("content_hash");
                let tags_str: String = row.get("tags");
                let metadata_str: Option<String> = row.get("metadata");
                let created_str: String = row.get("created_at");
                let updated_str: String = row.get("updated_at");

                let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                let metadata: Option<serde_json::Value> =
                    metadata_str.and_then(|s| serde_json::from_str(&s).ok());
                let created_at = DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc);
                let updated_at = DateTime::parse_from_rfc3339(&updated_str)?.with_timezone(&Utc);

                Ok(Document {
                    id: Uuid::parse_str(&id).map_err(|e| Error::Input(e.to_string()))?,
                    title,
                    content,
                    content_hash,
                    tags,
                    metadata,
                    created_at,
                    updated_at,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok((documents, total_count))
    }

    /// List all non-deleted documents with pagination.
    pub async fn list_documents(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Document>, i64), Error> {
        self.search_documents("", None, limit, offset).await
    }

    /// Aggregate statistics.
    pub async fn get_stats(&self) -> Result<Stats, Error> {
        let total_documents: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM documents WHERE is_deleted = 0")
                .fetch_one(&self.pool)
                .await?;

        let total_links: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM document_links")
                .fetch_one(&self.pool)
                .await?;

        let db_size: i64 = sqlx::query_scalar(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        )
        .fetch_one(&self.pool)
        .await?;

        let last_updated: Option<DateTime<Utc>> =
            match sqlx::query_scalar::<_, Option<String>>(
                "SELECT MAX(updated_at) FROM documents WHERE is_deleted = 0",
            )
            .fetch_optional(&self.pool)
            .await?
            {
                Some(Some(updated_str)) => {
                    Some(DateTime::parse_from_rfc3339(&updated_str)?.with_timezone(&Utc))
                }
                _ => Some(Utc::now()),
            };

        Ok(Stats {
            total_documents,
            total_links,
            database_size_bytes: db_size,
            last_updated: last_updated.unwrap_or_else(Utc::now),
        })
    }

    // ── Private helpers ───────────────────────────────────────────────────

    async fn get_document_by_id(&self, id: &str) -> Result<Option<Document>, Error> {
        let row = sqlx::query(
            "SELECT id, title, content, content_hash, tags, metadata, created_at, updated_at
             FROM documents WHERE id = ?1 AND is_deleted = 0",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };

        let id: String = row.get("id");
        let title: String = row.get("title");
        let content: String = row.get("content");
        let content_hash: String = row.get("content_hash");
        let tags_str: String = row.get("tags");
        let metadata_str: Option<String> = row.get("metadata");
        let created_str: String = row.get("created_at");
        let updated_str: String = row.get("updated_at");

        let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
        let metadata: Option<serde_json::Value> =
            metadata_str.and_then(|s| serde_json::from_str(&s).ok());
        let created_at = DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc);
        let updated_at = DateTime::parse_from_rfc3339(&updated_str)?.with_timezone(&Utc);

        Ok(Some(Document {
            id: Uuid::parse_str(&id).map_err(|e| Error::Input(e.to_string()))?,
            title,
            content,
            content_hash,
            tags,
            metadata,
            created_at,
            updated_at,
        }))
    }

    fn compute_hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Escape a user query string for use in an FTS5 MATCH clause.
    ///
    /// Wrapping in double-quotes forces FTS5 to treat it as a literal phrase,
    /// with internal double-quotes escaped per the `""` convention.
    fn escape_fts_query(query: &str) -> String {
        let escaped = query.replace('"', "\"\"");
        format!("\"{escaped}\"")
    }
}

// Inline hex encoding to avoid pulling in the hex crate.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}
