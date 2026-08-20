//! Transport abstraction for the JSON-RPC 2.0 protocol.
//!
//! The [`Transport`] trait decouples the RPC loop from the underlying I/O
//! mechanism. [`StdioTransport`] (newline-delimited JSON over any
//! `BufRead`/`Write` pair) is the default; the trait allows future transports
//! (WebSocket, Unix socket, TCP) without modifying the RPC dispatch logic.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

// ── Read result ────────────────────────────────────────────────────────────────

/// Outcome of a single transport read operation.
#[derive(Debug)]
pub enum ReadResult {
    /// A valid JSON-RPC message was received.
    Message(Value),
    /// The received bytes could not be parsed as JSON.
    ParseError(String),
    /// The transport disconnected cleanly (EOF, connection closed).
    Eof,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A bidirectional transport for JSON-RPC 2.0.
///
/// Each method receives or returns a single JSON-RPC message. The transport
/// handles framing; callers just pass/receive [`Value`].
#[async_trait]
pub trait Transport: Send + Sync {
    /// Read one JSON-RPC message from the transport.
    ///
    /// Returns [`ReadResult::Message`] for valid JSON, [`ReadResult::ParseError`]
    /// when the bytes cannot be parsed, and [`ReadResult::Eof`] on clean
    /// disconnect.
    async fn read_message(&self) -> ReadResult;

    /// Write one JSON-RPC message asynchronously.
    ///
    /// Returns an error on I/O failure.
    async fn write_message(&self, value: &Value) -> Result<()>;
}

// ── Stdio implementation ─────────────────────────────────────────────────────

/// Newline-delimited JSON transport over synchronous [`BufRead`] + [`Write`].
///
/// Internally wraps both handles in `Arc<Mutex<…>>` so the main loop and the
/// approval gate can share them concurrently. Blocking I/O is offloaded to
/// `tokio::task::spawn_blocking` to avoid starving the async runtime.
pub struct StdioTransport {
    /// Shared, mutex-protected reader.
    reader: Arc<Mutex<Box<dyn BufRead + Send>>>,
    /// Shared, mutex-protected writer.
    writer: Arc<Mutex<Box<dyn Write + Send + Sync>>>,
}

impl Clone for StdioTransport {
    /// Clones share the same underlying reader/writer handles — two clones
    /// are two handles onto one pipe, which is what concurrent readers and
    /// writers over the same child process need.
    fn clone(&self) -> Self {
        Self {
            reader: Arc::clone(&self.reader),
            writer: Arc::clone(&self.writer),
        }
    }
}

impl StdioTransport {
    /// Wrap a [`BufRead`] reader and [`Write`] writer as a transport.
    pub fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: BufRead + Send + 'static,
        W: Write + Send + Sync + 'static,
    {
        Self {
            reader: Arc::new(Mutex::new(Box::new(reader))),
            writer: Arc::new(Mutex::new(Box::new(writer))),
        }
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn read_message(&self) -> ReadResult {
        let reader = Arc::clone(&self.reader);
        tokio::task::spawn_blocking(move || {
            let mut guard = reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut line = String::new();
            let n = guard.read_line(&mut line).unwrap_or(0);
            if n == 0 {
                return ReadResult::Eof;
            }
            match serde_json::from_str(line.trim()) {
                Ok(value) => ReadResult::Message(value),
                Err(e) => ReadResult::ParseError(e.to_string()),
            }
        })
        .await
        .unwrap_or(ReadResult::Eof)
    }

    async fn write_message(&self, value: &Value) -> Result<()> {
        let value = value.clone();
        let writer = Arc::clone(&self.writer);
        tokio::task::spawn_blocking(move || {
            let mut guard = writer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            writeln!(guard, "{value}")?;
            guard.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("write task panicked: {e}"))?
    }
}
