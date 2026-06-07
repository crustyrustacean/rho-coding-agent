//! JSON-RPC 2.0 client for the rho subprocess.
//!
//! Spawns rho as a child process, pipes stdin/stdout, and provides a
//! typed interface for sending requests and receiving messages (responses
//! and notifications).

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

// ── Message types ───────────────────────────────────────────────────────────────

/// A parsed JSON-RPC 2.0 message.
#[derive(Debug)]
pub(crate) enum Message {
    /// A response to a request (has a non-null `id`).
    Response {
        /// The request ID.
        id: Value,
        /// The result (present on success).
        result: Option<Value>,
        /// The error object (present on protocol-level failure).
        error: Option<Value>,
    },
    /// A notification (no `id` field).
    Notification {
        /// The notification method name.
        method: String,
        /// The notification parameters.
        params: Value,
    },
}

impl Message {
    /// Parse a raw JSON value into a JSON-RPC 2.0 message.
    pub(crate) fn parse(value: Value) -> Result<Self> {
        let has_id = value.get("id").is_some_and(|v| !v.is_null());

        if has_id {
            ensure!(
                value.get("result").is_some() || value.get("error").is_some(),
                "response missing both 'result' and 'error'"
            );
            Ok(Self::Response {
                id: value["id"].clone(),
                result: value.get("result").cloned(),
                error: value.get("error").cloned(),
            })
        } else {
            let method = value
                .get("method")
                .and_then(|v| v.as_str())
                .context("notification missing 'method' field")?
                .to_owned();
            let params = value.get("params").cloned().unwrap_or(json!({}));
            Ok(Self::Notification { method, params })
        }
    }
}

// ── Client ────────────────────────────────────────────────────────────────────

/// Manages the rho subprocess and JSON-RPC 2.0 communication.
///
/// A background tokio task reads lines from rho's stdout, parses them as
/// JSON-RPC messages, and delivers them through an mpsc channel. The
/// foreground caller sends requests via [`send`](Self::send) and receives
/// messages via [`recv`](Self::recv).
pub(crate) struct RhoClient {
    /// The child process handle.
    child: tokio::process::Child,
    /// Buffered async writer for the child's stdin.
    stdin: tokio::io::BufWriter<tokio::process::ChildStdin>,
    /// Channel receiver for parsed JSON-RPC messages.
    rx: mpsc::Receiver<Message>,
    /// Monotonic request ID counter.
    next_id: u64,
}

impl RhoClient {
    /// Spawn a rho subprocess with the given command-line arguments.
    ///
    /// `args[0]` is the program to execute; `args[1..]` are its arguments.
    /// The child's stderr is inherited (rho diagnostics pass through).
    pub(crate) fn spawn(args: &[String]) -> Result<Self> {
        ensure!(!args.is_empty(), "args must not be empty");

        let mut child = Command::new(&args[0])
            .args(&args[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to spawn: {}", args[0]))?;

        let stdin = child
            .stdin
            .take()
            .context("failed to capture child stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture child stdout")?;

        let (tx, rx) = mpsc::channel(256);

        // Background task: read JSON-RPC lines from the child's stdout.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(value) => match Message::parse(value) {
                        Ok(msg) => {
                            if tx.send(msg).await.is_err() {
                                break; // receiver dropped
                            }
                        }
                        Err(e) => {
                            eprintln!("[rho-repl] parse error: {e}");
                        }
                    },
                    Err(e) => {
                        eprintln!("[rho-repl] json error: {e}");
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin: tokio::io::BufWriter::new(stdin),
            rx,
            next_id: 0,
        })
    }

    /// Send a JSON-RPC 2.0 request and return the assigned request ID.
    pub(crate) async fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(id)
    }

    /// Receive the next JSON-RPC message from rho.
    ///
    /// Returns an error if the child process exits or the channel closes.
    pub(crate) async fn recv(&mut self) -> Result<Message> {
        self.rx
            .recv()
            .await
            .context("rho process ended unexpectedly")
    }

    /// Kill the child process.
    pub(crate) fn kill(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Drop for RhoClient {
    fn drop(&mut self) {
        self.kill();
    }
}
