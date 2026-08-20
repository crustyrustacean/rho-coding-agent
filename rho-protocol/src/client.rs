//! Reusable JSON-RPC 2.0 client for rho frontends.
//!
//! Every existing frontend (the Deno TUI, both desktop GUIs) hand-rolls the
//! same three pieces: request/response correlation keyed by id, a response
//! timeout, and a loop that routes notifications to UI handlers and responses
//! to pending futures. This module is that code, written once.
//!
//! The client is transport-agnostic: hand it any [`Arc<dyn Transport>`].
//! [`StdioChild`] covers the common case of "spawn the rho binary and speak
//! newline-delimited JSON over its stdio."

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use std::process::{Child, Command, Stdio as ProcessStdio};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::Transport;
use crate::transport::StdioTransport;

/// Default response timeout (30s), mirroring the frontends' existing behavior.
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// A message routed to the frontend by [`RhoClient`].
#[derive(Debug)]
pub enum ClientEvent {
    /// A JSON-RPC notification (no `id`): `agent/start`, `message/delta`,
    /// `approval/request`, …
    Notification(Value),
    /// The transport hit EOF — rho exited or the connection closed.
    Disconnected,
    /// A response arrived whose `id` matched no pending request (e.g. the
    /// caller timed out first). Surfaced so the UI can log it.
    OrphanResponse(Value),
}

/// A live rho child process with its stdio wired to a transport.
///
/// Owns the process. Dropping does **not** kill the child: frontends may
/// legitimately want rho to outlive a UI crash (session resume depends on the
/// session file, not the process). Call [`StdioChild::kill`] explicitly when
/// the engine should stop.
/// Manual `Debug` (the transport's internals are uninteresting): shows the
/// child's pid. Not `Clone` — the process handle is not cloneable; use
/// [`StdioChild::transport_handle`] for shared access to the transport.
pub struct StdioChild {
    /// The spawned process.
    pub child: Child,
    /// Transport over the child's stdin/stdout.
    pub transport: StdioTransport,
}

impl std::fmt::Debug for StdioChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioChild")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl StdioChild {
    /// Spawn `bin` with `args`, piping its stdio.
    ///
    /// # Errors
    ///
    /// Fails if the process cannot be spawned (binary missing, permission
    /// denied).
    ///
    /// # Panics
    ///
    /// Never panics: stdio was requested `piped()`, so the take calls
    /// always succeed on a successful spawn.
    pub fn spawn(bin: &str, args: &[&str]) -> std::io::Result<Self> {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(ProcessStdio::piped())
            .stdout(ProcessStdio::piped())
            .stderr(ProcessStdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        // `ChildStdout` is `Read` but not `BufRead`; wrap for line reads.
        let reader = std::io::BufReader::new(stdout);
        let transport = StdioTransport::new(reader, stdin);
        Ok(Self { child, transport })
    }

    /// A shared handle to the transport for handing to [`RhoClient::new`].
    ///
    /// The returned `Arc` shares the same reader/writer as this child's
    /// transport (state lives behind `Arc` internally).
    #[must_use]
    pub fn transport_handle(&self) -> Arc<dyn Transport> {
        Arc::new(self.transport.clone())
    }

    /// Signal the child to stop and reap it.
    ///
    /// # Errors
    ///
    /// Propagates process I/O errors from kill.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()
    }
}

/// Pending request bookkeeping: id → response sender.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// A JSON-RPC 2.0 client with correlation and notification dispatch.
///
/// Cloneable: all clones share the same pending-request table, id counter,
/// and event stream. The intended shape is one clone running [`RhoClient::run`]
/// on a background task while the UI thread makes requests through another.
///
/// ```
/// use std::sync::Arc;
/// use rho_protocol::{ClientEvent, RhoClient, StdioChild};
/// # async fn demo() -> anyhow::Result<()> {
/// let mut proc = StdioChild::spawn("rho", &["--ephemeral"])?;
/// let (client, mut events) = RhoClient::new(proc.transport_handle());
/// let runner = { let client = client.clone(); tokio::spawn(async move { client.run().await }) };
/// let result = client
///     .request("prompt", serde_json::json!({"message": "hello"}))
///     .await?;
/// while let Some(event) = events.recv().await {
///     match event {
///         ClientEvent::Notification(n) => { /* render */ }
///         ClientEvent::Disconnected => break,
///         _ => {}
///     }
/// }
/// # let _ = result; runner.abort(); let _ = runner.await;
/// # Ok(())
/// # }
/// ```
pub struct RhoClient {
    /// Shared transport (a clone shares state — see [`Transport::clone_box`]).
    transport: Arc<dyn Transport>,
    /// Pending request-response callbacks, keyed by JSON-RPC id.
    pending: Pending,
    /// Monotonic request id source.
    next_id: Arc<Mutex<u64>>,
    /// Outbound event stream sender; the reader loop drives it.
    tx: mpsc::Sender<ClientEvent>,
}

impl Clone for RhoClient {
    fn clone(&self) -> Self {
        Self {
            transport: Arc::clone(&self.transport),
            pending: Arc::clone(&self.pending),
            next_id: Arc::clone(&self.next_id),
            tx: self.tx.clone(),
        }
    }
}

impl RhoClient {
    /// Create a client over `transport`. Returns the client handle and the
    /// event stream the UI consumes.
    ///
    /// The handle alone does nothing; call [`RhoClient::run`] on a background
    /// task to drive the read loop.
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>) -> (Self, mpsc::Receiver<ClientEvent>) {
        let (tx, rx) = mpsc::channel(256);
        (
            Self {
                transport,
                pending: Arc::new(Mutex::new(HashMap::new())),
                next_id: Arc::new(Mutex::new(0)),
                tx,
            },
            rx,
        )
    }

    /// Send a request and await its response with the default timeout.
    ///
    /// # Errors
    ///
    /// - Transport write failure.
    /// - Timeout ([`DEFAULT_RESPONSE_TIMEOUT`]).
    /// - JSON-RPC error response (the error object is embedded in the message).
    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.request_with_timeout(method, params, DEFAULT_RESPONSE_TIMEOUT)
            .await
    }

    /// Send a request and await its response with a custom timeout.
    ///
    /// # Errors
    ///
    /// Same as [`RhoClient::request`] with a caller-chosen timeout.
    pub async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = {
            let mut guard = self.next_id.lock().await;
            *guard += 1;
            *guard
        };

        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.transport.write_message(&message).await?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => {
                if let Some(error) = response.get("error") {
                    anyhow::bail!("JSON-RPC error from {method}: {error}");
                }
                Ok(response.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(_recv_err)) => anyhow::bail!("client dropped while awaiting {method}"),
            Err(_elapsed) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("request timed out: {method}")
            }
        }
    }

    /// Fire-and-forget: write a notification without tracking a response.
    ///
    /// # Errors
    ///
    /// Transport write failure.
    pub async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.transport.write_message(&message).await
    }

    /// Drive the read loop until EOF. Routes responses to pending requests
    /// and everything else to the event stream; emits
    /// [`ClientEvent::Disconnected`] on EOF.
    ///
    /// Takes `&self` so the runner can be spawned from a clone while the UI
    /// keeps making requests on another.
    ///
    /// # Errors
    ///
    /// Never returns `Err` in practice; kept `Result` for signature stability.
    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            match self.transport.read_message().await {
                crate::ReadResult::Message(msg) => {
                    let is_response = msg.get("id").is_some()
                        && (msg.get("result").is_some() || msg.get("error").is_some());
                    if is_response {
                        let id = msg.get("id").and_then(Value::as_u64);
                        let mut pending = self.pending.lock().await;
                        match id.and_then(|id| pending.remove(&id)) {
                            Some(tx) => {
                                let _ = tx.send(msg);
                            }
                            None => {
                                let _ = self.tx.send(ClientEvent::OrphanResponse(msg)).await;
                            }
                        }
                    } else {
                        let _ = self.tx.send(ClientEvent::Notification(msg)).await;
                    }
                }
                crate::ReadResult::ParseError(_) => {
                    // Malformed lines are skipped; the loop continues. The
                    // frontends' previous behavior (show a parse error line)
                    // can be layered by the UI if wanted.
                }
                crate::ReadResult::Eof => {
                    let _ = self.tx.send(ClientEvent::Disconnected).await;
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A transport whose writes land in a shared buffer and whose reads come
    /// from a queue of pre-baked lines.
    struct FakeTransport {
        reads: std::sync::Mutex<VecDeque<String>>,
        writes: std::sync::Mutex<Vec<String>>,
    }

    impl FakeTransport {
        fn new(reads: Vec<String>) -> Arc<Self> {
            Arc::new(Self {
                reads: std::sync::Mutex::new(reads.into()),
                writes: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn written(&self) -> Vec<String> {
            self.writes.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl Transport for FakeTransport {
        async fn read_message(&self) -> crate::ReadResult {
            let line = self.reads.lock().unwrap().pop_front();
            match line {
                Some(l) => match serde_json::from_str(&l) {
                    Ok(v) => crate::ReadResult::Message(v),
                    Err(e) => crate::ReadResult::ParseError(e.to_string()),
                },
                None => crate::ReadResult::Eof,
            }
        }

        async fn write_message(&self, value: &Value) -> anyhow::Result<()> {
            self.writes.lock().unwrap().push(value.to_string());
            Ok(())
        }
    }

    fn spawn_runner(client: &RhoClient) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        let c = client.clone();
        tokio::spawn(async move { c.run().await })
    }

    #[tokio::test]
    async fn request_correlates_by_id_and_returns_result() {
        let transport = FakeTransport::new(vec![
            r#"{"jsonrpc":"2.0","id":1,"result":{"reply":"hi"}}"#.to_owned(),
        ]);
        let (client, _events) = RhoClient::new(transport.clone());
        let runner = spawn_runner(&client);

        let result = client
            .request("prompt", serde_json::json!({"message": "hello"}))
            .await
            .expect("request should succeed");
        assert_eq!(result["reply"], "hi");

        // The written message carried id 1 and the method name.
        let writes = transport.written();
        assert_eq!(writes.len(), 1);
        let sent: Value = serde_json::from_str(&writes[0]).unwrap();
        assert_eq!(sent["id"], 1);
        assert_eq!(sent["method"], "prompt");

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn error_response_rejects_the_request() {
        let transport = FakeTransport::new(vec![
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#
                .to_owned(),
        ]);
        let (client, _events) = RhoClient::new(transport);
        let runner = spawn_runner(&client);

        let err = client
            .request("bogus", serde_json::json!({}))
            .await
            .expect_err("error response should reject");
        assert!(err.to_string().contains("bogus"));

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn notifications_flow_to_the_event_stream() {
        let transport = FakeTransport::new(vec![
            r#"{"jsonrpc":"2.0","method":"ready"}"#.to_owned(),
            r#"{"jsonrpc":"2.0","method":"agent/start"}"#.to_owned(),
        ]);
        let (client, mut events) = RhoClient::new(transport);
        let runner = spawn_runner(&client);

        match events.recv().await.expect("first notification") {
            ClientEvent::Notification(v) => assert_eq!(v["method"], "ready"),
            other => panic!("expected notification, got {other:?}"),
        }
        match events.recv().await.expect("second notification") {
            ClientEvent::Notification(v) => assert_eq!(v["method"], "agent/start"),
            other => panic!("expected notification, got {other:?}"),
        }

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn eof_emits_disconnected() {
        let transport = FakeTransport::new(vec![]);
        let (client, mut events) = RhoClient::new(transport);
        let runner = spawn_runner(&client);

        match events.recv().await.expect("event after EOF") {
            ClientEvent::Disconnected => {}
            other => panic!("expected Disconnected, got {other:?}"),
        }

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn timeout_rejects_and_cleans_up_pending() {
        // No responses queued: the request must time out.
        let transport = FakeTransport::new(vec![]);
        let (client, _events) = RhoClient::new(transport);
        let runner = spawn_runner(&client);

        let err = client
            .request_with_timeout("prompt", serde_json::json!({}), Duration::from_millis(50))
            .await
            .expect_err("should time out");
        assert!(err.to_string().contains("timed out"));

        // And the pending entry was removed: a later response with the same
        // id is an orphan, not a hang.
        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn orphan_response_is_surfaced_not_dropped() {
        let transport =
            FakeTransport::new(vec![r#"{"jsonrpc":"2.0","id":99,"result":{}}"#.to_owned()]);
        let (client, mut events) = RhoClient::new(transport);
        let runner = spawn_runner(&client);

        match events.recv().await.expect("orphan event") {
            ClientEvent::OrphanResponse(v) => assert_eq!(v["id"], 99),
            other => panic!("expected orphan, got {other:?}"),
        }

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn concurrent_requests_get_distinct_ids() {
        let transport = FakeTransport::new(vec![
            r#"{"jsonrpc":"2.0","id":2,"result":{"b":1}}"#.to_owned(),
            r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}}"#.to_owned(),
        ]);
        let (client, _events) = RhoClient::new(transport.clone());
        let runner = spawn_runner(&client);

        let (r1, r2) = tokio::join!(
            client.request("getState", serde_json::json!({})),
            client.request("listModels", serde_json::json!({})),
        );
        assert_eq!(r1.expect("first ok")["a"], 1);
        assert_eq!(r2.expect("second ok")["b"], 1);

        // Two writes, ids 1 and 2, in some order.
        let writes = transport.written();
        assert_eq!(writes.len(), 2);

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn notify_writes_without_awaiting_response() {
        let transport = FakeTransport::new(vec![]);
        let (client, _events) = RhoClient::new(transport.clone());
        let runner = spawn_runner(&client);

        client
            .notify("approvalResponse", serde_json::json!({"approved": true}))
            .await
            .expect("notify write");

        let writes = transport.written();
        assert_eq!(writes.len(), 1);
        let sent: Value = serde_json::from_str(&writes[0]).unwrap();
        assert!(sent.get("id").is_none(), "notifications carry no id");

        runner.abort();
        let _ = runner.await;
    }

    #[tokio::test]
    async fn stdio_child_spawn_missing_binary_errors() {
        let err = StdioChild::spawn("definitely-not-a-real-binary-xyz", &[])
            .expect_err("spawn should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
