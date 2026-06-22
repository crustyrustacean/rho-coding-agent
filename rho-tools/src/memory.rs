//! Project-local knowledge base tool backed by `rho-memory`.

use std::sync::Arc;

use rho_core::{CancellationToken, RhoError, Tool, ToolName, ToolOutcome, ToolResult, ToolRisk};
use rho_memory::{CreateRequest, Memory, UpdateRequest};
use serde_json::json;

use crate::error::ToolError;

/// Tool for storing and recalling knowledge across sessions.
///
/// Supports six operations:
/// - `store` — persist a new document (title, content, tags)
/// - `search` — full-text search with optional tag filtering
/// - `get` — fetch a document by UUID
/// - `update` — partial update of an existing document
/// - `delete` — soft-delete a document
/// - `list` — paginated list of all documents
#[derive(Clone, Debug)]
pub struct MemoryTool {
    /// The project-local knowledge base.
    memory: Arc<Memory>,
}

impl MemoryTool {
    /// Create a new `MemoryTool` wrapping the given knowledge base.
    #[must_use]
    pub fn new(memory: Arc<Memory>) -> Self {
        Self { memory }
    }

    /// Parse a `u64` JSON value as `usize`, defaulting to `default` if absent.
    fn get_usize(arguments: &serde_json::Value, field: &str, default: usize) -> usize {
        arguments
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(default)
    }
}

#[async_trait::async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> ToolName {
        ToolName::from("memory")
    }

    fn description(&self) -> &str {
        "Project-local knowledge base for storing and recalling information across sessions. \
         Supports six operations: `store` (persist a new document), `search` (full-text search), \
         `get` (fetch by id), `update` (partial update), `delete` (soft-delete), `list` (paginated). \
         Use `store` to save design decisions, project conventions, debugging discoveries, and \
         architectural patterns. Use `search` at the start of a task to recall relevant context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["store", "search", "get", "update", "delete", "list"],
                    "description": "The operation to perform"
                },
                "title": {
                    "type": "string",
                    "description": "Document title (required for `store`)"
                },
                "content": {
                    "type": "string",
                    "description": "Document content (required for `store`)"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Organization tags (optional for `store` and `update`)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (required for `search`)"
                },
                "id": {
                    "type": "string",
                    "description": "Document UUID (required for `get`, `update`, `delete`)"
                },
                "new_title": {
                    "type": "string",
                    "description": "New title for `update`"
                },
                "new_content": {
                    "type": "string",
                    "description": "New content for `update`"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return for `search` and `list` (default: 10)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of results to skip for `search` and `list` (default: 0)"
                }
            },
            "required": ["operation"]
        })
    }

    fn risk(&self) -> ToolRisk {
        // Risk is determined per-operation in execute(), but the default is Read
        // since the most common operation (search) is read-only.
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> Result<ToolOutcome, RhoError> {
        let operation = arguments
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RhoError::Tool("missing required argument `operation`".to_string()))?;

        match operation {
            "store" => self.execute_store(&arguments).await,
            "search" => self.execute_search(&arguments).await,
            "get" => self.execute_get(&arguments).await,
            "update" => self.execute_update(&arguments).await,
            "delete" => self.execute_delete(&arguments).await,
            "list" => self.execute_list(&arguments).await,
            other => Err(RhoError::Tool(format!(
                "unknown operation `{other}`; expected one of: store, search, get, update, delete, list"
            ))),
        }
    }
}

// ── Operation implementations ──────────────────────────────────────────────

impl MemoryTool {
    /// Store a new document in the knowledge base.
    async fn execute_store(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, RhoError> {
        let title = arguments
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RhoError::Tool("missing required argument `title` for store".to_string())
            })?;
        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RhoError::Tool("missing required argument `content` for store".to_string())
            })?;

        let tags: Vec<String> = arguments
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let req = CreateRequest {
            title: title.to_string(),
            content: content.to_string(),
            tags,
            metadata: None,
        };

        let doc = self
            .memory
            .create_with(&req)
            .await
            .map_err(|e| ToolError::ApiError {
                status: 0,
                message: format!("memory store failed: {e}"),
            })?;

        let output = format!(
            "<memory id=\"{}\" title=\"{}\" created_at=\"{}\" content_hash=\"{}\">\n  tags: {}\n</memory>",
            doc.id,
            escape_attr(&doc.title),
            doc.created_at.to_rfc3339(),
            doc.content_hash,
            doc.tags
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        );

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }

    /// Search documents by full-text query and optional tag filter.
    async fn execute_search(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, RhoError> {
        let query = arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let tags: Option<Vec<String>> =
            arguments.get("tags").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });

        let limit = Self::get_usize(arguments, "limit", 10);
        let offset = Self::get_usize(arguments, "offset", 0);

        let (docs, total) = self
            .memory
            .search(query, tags.as_deref(), limit, offset)
            .await
            .map_err(|e| ToolError::ApiError {
                status: 0,
                message: format!("memory search failed: {e}"),
            })?;

        let mut parts = Vec::new();
        for doc in &docs {
            parts.push(format!(
                "<memory id=\"{}\" title=\"{}\" updated_at=\"{}\">\n  tags: {}\n  content: {}\n</memory>",
                doc.id,
                escape_attr(&doc.title),
                doc.updated_at.to_rfc3339(),
                doc.tags.iter().map(String::as_str).collect::<Vec<_>>().join(", "),
                truncate_content(&doc.content, 500),
            ));
        }

        let output = format!(
            "<memory_search total=\"{total}\" returned=\"{}\" offset=\"{offset}\">\n{}\n</memory_search>",
            docs.len(),
            parts.join("\n"),
        );

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }

    /// Fetch a single document by UUID.
    async fn execute_get(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, RhoError> {
        let id = parse_uuid(arguments, "id", "get")?;

        let doc = self
            .memory
            .get(&id)
            .await
            .map_err(|e| ToolError::ApiError {
                status: 0,
                message: format!("memory get failed: {e}"),
            })?
            .ok_or_else(|| ToolError::ApiError {
                status: 0,
                message: format!("document {id} not found"),
            })?;

        let output = format!(
            "<memory id=\"{}\" title=\"{}\" created_at=\"{}\" updated_at=\"{}\" content_hash=\"{}\">\n  tags: {}\n  content: {}\n</memory>",
            doc.id,
            escape_attr(&doc.title),
            doc.created_at.to_rfc3339(),
            doc.updated_at.to_rfc3339(),
            doc.content_hash,
            doc.tags
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            doc.content,
        );

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }

    /// Partially update an existing document.
    async fn execute_update(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, RhoError> {
        let id = parse_uuid(arguments, "id", "update")?;

        let mut update = UpdateRequest::default();

        if let Some(title) = arguments.get("new_title").and_then(|v| v.as_str()) {
            update.title = Some(title.to_string());
        }
        if let Some(content) = arguments.get("new_content").and_then(|v| v.as_str()) {
            update.content = Some(content.to_string());
        }
        if let Some(tags) = arguments.get("tags").and_then(|v| v.as_array()) {
            update.tags = Some(
                tags.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
            );
        }

        let doc = self
            .memory
            .update(&id, &update)
            .await
            .map_err(|e| ToolError::ApiError {
                status: 0,
                message: format!("memory update failed: {e}"),
            })?
            .ok_or_else(|| ToolError::ApiError {
                status: 0,
                message: format!("document {id} not found"),
            })?;

        let output = format!(
            "<memory id=\"{}\" title=\"{}\" updated_at=\"{}\">\n  tags: {}\n</memory>",
            doc.id,
            escape_attr(&doc.title),
            doc.updated_at.to_rfc3339(),
            doc.tags
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        );

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }

    /// Soft-delete a document.
    async fn execute_delete(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, RhoError> {
        let id = parse_uuid(arguments, "id", "delete")?;

        let deleted = self
            .memory
            .delete(&id)
            .await
            .map_err(|e| ToolError::ApiError {
                status: 0,
                message: format!("memory delete failed: {e}"),
            })?;

        let output = if deleted {
            format!("<memory_delete id=\"{id}\" deleted=\"true\" />")
        } else {
            format!("<memory_delete id=\"{id}\" deleted=\"false\" />")
        };

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }

    /// List all documents with pagination.
    async fn execute_list(&self, arguments: &serde_json::Value) -> Result<ToolOutcome, RhoError> {
        let limit = Self::get_usize(arguments, "limit", 10);
        let offset = Self::get_usize(arguments, "offset", 0);

        let (docs, total) =
            self.memory
                .list(limit, offset)
                .await
                .map_err(|e| ToolError::ApiError {
                    status: 0,
                    message: format!("memory list failed: {e}"),
                })?;

        let mut parts = Vec::new();
        for doc in &docs {
            parts.push(format!(
                "<memory id=\"{}\" title=\"{}\" updated_at=\"{}\">\n  tags: {}\n  content: {}\n</memory>",
                doc.id,
                escape_attr(&doc.title),
                doc.updated_at.to_rfc3339(),
                doc.tags.iter().map(String::as_str).collect::<Vec<_>>().join(", "),
                truncate_content(&doc.content, 200),
            ));
        }

        let output = format!(
            "<memory_list total=\"{total}\" returned=\"{}\" offset=\"{offset}\">\n{}\n</memory_list>",
            docs.len(),
            parts.join("\n"),
        );

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a UUID from the arguments object.
fn parse_uuid(
    arguments: &serde_json::Value,
    field: &str,
    operation: &str,
) -> Result<uuid::Uuid, RhoError> {
    let id_str = arguments
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            RhoError::Tool(format!(
                "missing required argument `{field}` for {operation}"
            ))
        })?;
    uuid::Uuid::parse_str(id_str)
        .map_err(|e| RhoError::Tool(format!("invalid UUID for `{field}` in {operation}: {e}")))
}

/// Escape XML attribute special characters.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Truncate content to at most `max_chars` characters, appending "..." if truncated.
fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        content.to_string()
    } else {
        let end = content.floor_char_boundary(max_chars);
        format!("{}...", &content[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_memory::Memory;
    use std::sync::Arc;

    async fn fresh_tool() -> MemoryTool {
        let memory = Memory::open_in_memory().await.unwrap();
        MemoryTool::new(Arc::new(memory))
    }

    #[test]
    fn tool_name() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tool = MemoryTool {
            memory: Arc::new(rt.block_on(Memory::open_in_memory()).unwrap()),
        };
        assert_eq!(&*tool.name(), "memory");
    }

    #[test]
    fn tool_risk_is_read() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tool = MemoryTool {
            memory: Arc::new(rt.block_on(Memory::open_in_memory()).unwrap()),
        };
        assert_eq!(tool.risk(), ToolRisk::Read);
    }

    #[test]
    fn tool_description_is_not_empty() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tool = MemoryTool {
            memory: Arc::new(rt.block_on(Memory::open_in_memory()).unwrap()),
        };
        let desc = tool.description();
        assert!(!desc.is_empty());
        assert!(desc.contains("store"));
        assert!(desc.contains("search"));
        assert!(desc.contains("get"));
        assert!(desc.contains("update"));
        assert!(desc.contains("delete"));
        assert!(desc.contains("list"));
    }

    #[test]
    fn tool_parameters_schema_is_valid() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let tool = MemoryTool {
            memory: Arc::new(rt.block_on(Memory::open_in_memory()).unwrap()),
        };
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let props = &schema["properties"];
        assert!(props.get("operation").is_some());
        assert!(props.get("title").is_some());
        assert!(props.get("content").is_some());
        assert!(props.get("tags").is_some());
        assert!(props.get("query").is_some());
        assert!(props.get("id").is_some());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("operation")));
    }

    #[tokio::test]
    async fn store_and_get_round_trip() {
        let tool = fresh_tool().await;
        let store_args = json!({
            "operation": "store",
            "title": "Test Doc",
            "content": "Hello world",
            "tags": ["test", "hello"]
        });
        let result = tool
            .execute(store_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(!tr.is_error);
        assert!(tr.output.contains("Test Doc"));
        assert!(tr.output.contains("test, hello"));

        // Extract id from output
        let id_start = tr.output.find("id=\"").unwrap() + 4;
        let id_end = tr.output[id_start..].find('"').unwrap() + id_start;
        let id_str = &tr.output[id_start..id_end];

        let get_args = json!({
            "operation": "get",
            "id": id_str
        });
        let result = tool
            .execute(get_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(!tr.is_error);
        assert!(tr.output.contains("Hello world"));
    }

    #[tokio::test]
    async fn search_finds_stored_document() {
        let tool = fresh_tool().await;

        let store_args = json!({
            "operation": "store",
            "title": "Rust Patterns",
            "content": "Use Result instead of exceptions for error handling in Rust",
            "tags": ["rust", "patterns"]
        });
        let _ = tool
            .execute(store_args, CancellationToken::new())
            .await
            .unwrap();

        let search_args = json!({
            "operation": "search",
            "query": "error handling"
        });
        let result = tool
            .execute(search_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(!tr.is_error);
        assert!(tr.output.contains("Rust Patterns"));
        assert!(tr.output.contains("total=\"1\""));
    }

    #[tokio::test]
    async fn update_changes_title() {
        let tool = fresh_tool().await;

        let store_args = json!({
            "operation": "store",
            "title": "Old Title",
            "content": "content"
        });
        let result = tool
            .execute(store_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        let id_start = tr.output.find("id=\"").unwrap() + 4;
        let id_end = tr.output[id_start..].find('"').unwrap() + id_start;
        let id_str = &tr.output[id_start..id_end];

        let update_args = json!({
            "operation": "update",
            "id": id_str,
            "new_title": "New Title"
        });
        let result = tool
            .execute(update_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(!tr.is_error);
        assert!(tr.output.contains("New Title"));
    }

    #[tokio::test]
    async fn delete_soft_removes_document() {
        let tool = fresh_tool().await;

        let store_args = json!({
            "operation": "store",
            "title": "To Delete",
            "content": "gone soon"
        });
        let result = tool
            .execute(store_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        let id_start = tr.output.find("id=\"").unwrap() + 4;
        let id_end = tr.output[id_start..].find('"').unwrap() + id_start;
        let id_str = &tr.output[id_start..id_end];

        let delete_args = json!({
            "operation": "delete",
            "id": id_str
        });
        let result = tool
            .execute(delete_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(!tr.is_error);
        assert!(tr.output.contains("deleted=\"true\""));

        // Searching should no longer find it
        let search_args = json!({
            "operation": "search",
            "query": "gone"
        });
        let result = tool
            .execute(search_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(tr.output.contains("total=\"0\""));
    }

    #[tokio::test]
    async fn list_returns_paginated_results() {
        let tool = fresh_tool().await;
        for i in 0..3 {
            let store_args = json!({
                "operation": "store",
                "title": format!("Doc {i}"),
                "content": format!("content {i}")
            });
            let _ = tool
                .execute(store_args, CancellationToken::new())
                .await
                .unwrap();
        }

        let list_args = json!({
            "operation": "list",
            "limit": 2,
            "offset": 0
        });
        let result = tool
            .execute(list_args, CancellationToken::new())
            .await
            .unwrap();
        let ToolOutcome::Immediate(tr) = result else {
            panic!("expected Immediate")
        };
        assert!(!tr.is_error);
        assert!(tr.output.contains("total=\"3\""));
        assert!(tr.output.contains("returned=\"2\""));
    }

    #[tokio::test]
    async fn unknown_operation_returns_error() {
        let tool = fresh_tool().await;
        let args = json!({"operation": "bogus"});
        let result = tool.execute(args, CancellationToken::new()).await;
        assert!(result.is_err());
    }

    #[test]
    fn escape_attr_sanitizes_xml_special_chars() {
        assert_eq!(escape_attr("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
    }

    #[test]
    fn truncate_content_shortens_long_text() {
        let long = "a".repeat(300);
        let truncated = truncate_content(&long, 100);
        assert!(truncated.len() < 300);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn truncate_content_preserves_short_text() {
        let short = "hello";
        assert_eq!(truncate_content(short, 100), "hello");
    }
}
