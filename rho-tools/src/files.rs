//! File operation tools: [`ReadFile`] and [`WriteFile`].
//!
//! Phase 1b: sandbox enforcement via [`SandboxRoot`], and `<context>` framing on
//! `ReadFile` output so the model treats file contents as data, not instructions.

use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};

// ── ReadFile ──────────────────────────────────────────────────────────────────

/// Read a file's text content.
///
/// Output is wrapped in `<context>...</context>` tags so the model treats it
/// as data rather than as instructions (defense-in-depth against prompt
/// injection via file contents).
///
/// Paths are validated against the [`SandboxRoot`] before any I/O.
pub struct ReadFile {
    /// Sandbox root — all reads are validated against this.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> ToolName {
        ToolName::from("read_file")
    }

    fn description(&self) -> &'static str {
        "Read the text contents of a file within the project. \
         Returns the file's content wrapped in <context> tags."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to the project root)."
                }
            },
            "required": ["path"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Read
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let path_str = arguments["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("read_file: missing required argument `path`"))?;

        // Validate path is within the sandbox root.
        let safe_path = self.root.validate(path_str)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        let content = tokio::fs::read_to_string(&*safe_path)
            .await
            .map_err(|e| anyhow::anyhow!("read_file: failed to read `{path_str}`: {e}"))?;

        // Wrap in <context> framing — signals to the model that this is data,
        // not instructions. The system prompt reinforces this contract.
        let framed = format!("<context>\n{content}\n</context>");

        Ok(ToolOutcome::Immediate(ToolResult::success(framed)))
    }
}

// ── WriteFile ─────────────────────────────────────────────────────────────────

/// Write content to a file, creating it if it does not exist.
///
/// Paths are validated against the [`SandboxRoot`] before any I/O. Uses the
/// not-yet-existing-path validation so new files in the sandbox are allowed.
pub struct WriteFile {
    /// Sandbox root — all writes are validated against this.
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> ToolName {
        ToolName::from("write_file")
    }

    fn description(&self) -> &'static str {
        "Write text content to a file within the project. \
         Creates the file and any necessary parent directories if they do not exist; \
         overwrites it if it does."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to the project root)."
                },
                "content": {
                    "type": "string",
                    "description": "The text content to write."
                }
            },
            "required": ["path", "content"]
        })
    }

    fn risk(&self) -> ToolRisk {
        ToolRisk::Write
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome> {
        let path_str = arguments["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("write_file: missing required argument `path`"))?;
        let content = arguments["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("write_file: missing required argument `content`"))?;

        // Use the write-variant validator that handles not-yet-existing paths.
        let safe_path = self.root.validate_for_write(path_str)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        // Create parent directories if needed.
        if let Some(parent) = safe_path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                anyhow::anyhow!("write_file: failed to create directories for `{path_str}`: {e}")
            })?;
        }

        tokio::fs::write(&*safe_path, content)
            .await
            .map_err(|e| anyhow::anyhow!("write_file: failed to write `{path_str}`: {e}"))?;

        Ok(ToolOutcome::Immediate(ToolResult::success(format!(
            "wrote {} bytes to {path_str}",
            content.len()
        ))))
    }
}
