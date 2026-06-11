//! File operation tools: `ReadFile`, `WriteFile`, `BatchRead`, and `ListDir`.

use crate::hashline::compute_line_hash;
use crate::error::ToolError;
use async_trait::async_trait;
use rho_core::{
    Result, SandboxRoot, ToolName, ToolRisk,
    tool::{CancellationToken, Tool, ToolOutcome, ToolResult},
};
use tracing::{debug, info, warn};

pub struct ReadFile {
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> ToolName {
        ToolName::from("read_file")
    }

    fn description(&self) -> &str {
        "Read the text contents of a file within the project. \
         Returns the file's content wrapped in <context> tags. \
         When hashline is enabled (default), each line is prefixed with LINE#HASH: \
         (e.g., '  9#KTNS:  console.log(\"world\");') for reliable editing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to the project root)."
                },
                "hashline": {
                    "type": "boolean",
                    "description": "Enable hashline format (LINE#HASH: prefix). Defaults to true."
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
        let path_str = arguments["path"].as_str().ok_or_else(|| ToolError::MissingArgument { name: "path".to_string() })?;
        let hashline = arguments["hashline"].as_bool().unwrap_or(true);

        let candidate = self.root.path().join(path_str);
        let safe_path = self.root.validate(&candidate)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        debug!(path = %path_str, hashline = hashline, "reading file");

        let content = match tokio::fs::read_to_string(&*safe_path).await {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %path_str, error = %e, "failed to read file");
                return Ok(ToolOutcome::Immediate(ToolResult::error(format!("read_file: failed to read `{path_str}`: {e}"))));
            }
        };

        debug!(path = %path_str, lines = content.lines().count(), "file read successfully");

        let formatted_content = if hashline {
            let lines: Vec<&str> = content.lines().collect();
            if lines.is_empty() {
                String::new()
            } else {
                let pad_width = lines.len().to_string().len();
                let hashlined: Vec<String> = lines.iter().enumerate().map(|(i, line)| {
                    let line_num = i + 1;
                    let hash = compute_line_hash(line, line_num);
                    format!("{line_num:>pad_width$}#{hash}:{line}")
                }).collect();
                hashlined.join("\n")
            }
        } else {
            content
        };

        let framed = format!("<context>\n{formatted_content}\n<context:end>");
        Ok(ToolOutcome::Immediate(ToolResult::success(framed)))
    }
}

pub struct BatchRead {
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for BatchRead {
    fn name(&self) -> ToolName {
        ToolName::from("batch_read")
    }

    fn description(&self) -> &str {
        "Read multiple files at once. Takes an array of paths, returns all \
         contents in a single response. Each file is wrapped in <context> tags \
         with hashline format. Max 20 paths."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Array of file paths to read (relative to the project root). Max 20."
                }
            },
            "required": ["paths"]
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
        let paths_arg = arguments["paths"].as_array().ok_or_else(|| ToolError::MissingArgument { name: "paths".to_string() })?;

        if paths_arg.is_empty() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("batch_read: paths array is empty")));
        }
        if paths_arg.len() > 20 {
            return Ok(ToolOutcome::Immediate(ToolResult::error("batch_read: max 20 paths per call")));
        }

        let mut results = Vec::with_capacity(paths_arg.len());

        for (i, val) in paths_arg.iter().enumerate() {
            if cancel.is_cancelled() {
                return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
            }

            let path_str = val.as_str().unwrap_or("<invalid>");
            let candidate = self.root.path().join(path_str);
            let safe_path = match self.root.validate(&candidate) {
                Ok(p) => p,
                Err(e) => {
                    warn!(path = %path_str, error = %e, "batch_read: sandbox validation failed");
                    results.push(format!("[{}] error: {e}", i + 1));
                    continue;
                }
            };

            let content = match tokio::fs::read_to_string(&*safe_path).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(path = %path_str, error = %e, "batch_read: failed to read file");
                    results.push(format!("[{}] error: failed to read `{path_str}`: {e}", i + 1));
                    continue;
                }
            };

            let lines: Vec<&str> = content.lines().collect();
            let formatted = if lines.is_empty() {
                format!("<context file=\"{path_str}\">\n<context:end>")
            } else {
                let pad_width = lines.len().to_string().len();
                let hashlined: Vec<String> = lines.iter().enumerate().map(|(idx, line)| {
                    let line_num = idx + 1;
                    let hash = compute_line_hash(line, line_num);
                    format!("{line_num:>pad_width$}#{hash}:{line}")
                }).collect();
                format!("<context file=\"{}\">\n{}\n<context:end>", path_str, hashlined.join("\n"))
            };
            results.push(formatted);
        }

        let errors = paths_arg.len() - results.len();
        info!(requested = paths_arg.len(), succeeded = results.len(), errors = errors, "batch_read completed");

        let header = format!("batch_read: {}/{} files read", results.len(), paths_arg.len());
        Ok(ToolOutcome::Immediate(ToolResult::success(format!("{header}\n\n{}", results.join("\n\n")))))
    }
}

pub struct WriteFile {
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> ToolName {
        ToolName::from("write_file")
    }

    fn description(&self) -> &str {
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
        let path_str = arguments["path"].as_str().ok_or_else(|| ToolError::MissingArgument { name: "path".to_string() })?;
        let content = arguments["content"].as_str().ok_or_else(|| ToolError::MissingArgument { name: "content".to_string() })?;

        let candidate = self.root.path().join(path_str);
        let safe_path = self.root.validate_for_write(&candidate)?;

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        if let Some(parent) = safe_path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!("write_file: failed to create directories for `{path_str}`: {e}"))));
        }

        debug!(path = %path_str, bytes = content.len(), "writing file");

        if let Err(e) = tokio::fs::write(&*safe_path, content).await {
            warn!(path = %path_str, error = %e, "failed to write file");
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!("write_file: failed to write `{path_str}`: {e}"))));
        }

        info!(path = %path_str, bytes = content.len(), "file written successfully");
        Ok(ToolOutcome::Immediate(ToolResult::success(format!("wrote {} bytes to {path_str}", content.len()))))
    }
}

pub struct ListDir {
    pub root: SandboxRoot,
}

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> ToolName {
        ToolName::from("list_dir")
    }

    fn description(&self) -> &str {
        "List files and directories within the project. \
         Respects .gitignore rules by default. \
         Set recursive to true to walk subdirectories."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to list (relative to the project root). Defaults to the project root."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to list files recursively. Defaults to false."
                }
            },
            "required": []
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
        let path_str = arguments["path"].as_str().unwrap_or(".");
        let recursive = arguments["recursive"].as_bool().unwrap_or(false);

        let safe_path = if path_str == "." {
            self.root.path().to_path_buf()
        } else {
            let candidate = self.root.path().join(path_str);
            self.root.validate(&candidate)?.to_path_buf()
        };

        if cancel.is_cancelled() {
            return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
        }

        if !safe_path.is_dir() {
            return Ok(ToolOutcome::Immediate(ToolResult::error(format!("list_dir: `{path_str}` is not a directory"))));
        }

        let mut builder = ignore::WalkBuilder::new(&safe_path);
        builder.hidden(false).git_ignore(true).git_global(true).git_exclude(true).ignore(true).require_git(false).sort_by_file_name(std::cmp::Ord::cmp);

        if !recursive {
            builder.max_depth(Some(1));
        }

        let walker = builder.build();

        let mut entries = Vec::new();
        let mut error_count = 0u32;

        for entry in walker {
            if cancel.is_cancelled() {
                return Ok(ToolOutcome::Immediate(ToolResult::error("cancelled")));
            }

            match entry {
                Ok(e) => {
                    if e.path() == safe_path {
                        continue;
                    }

                    let relative = e.path().strip_prefix(&safe_path).unwrap_or(e.path());
                    let path_display = relative.to_string_lossy();

                    if e.file_type().is_some_and(|ft| ft.is_dir()) {
                        entries.push(format!("{path_display}/"));
                    } else {
                        entries.push(path_display.into_owned());
                    }
                }
                Err(_) => {
                    error_count += 1;
                }
            }
        }

        debug!(path = %path_str, entries = entries.len(), recursive = recursive, errors = error_count, "directory listing completed");

        let mut output = entries.join("\n");
        if error_count > 0 {
            use std::fmt::Write;
            let _ = write!(output, "\n\n({error_count} entries could not be read)");
        }

        if output.is_empty() {
            output.clear();
            output.push_str("(empty directory)");
        }

        Ok(ToolOutcome::Immediate(ToolResult::success(output)))
    }
}