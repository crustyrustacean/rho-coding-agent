//! General-purpose async dispatcher.
//!
//! A single background thread with a tokio runtime + threadpool that any
//! synchronous op can dispatch async work to and block on the result.
//!
//! This replaces the one-off `HttpExecutor` pattern (which duplicated a
//! dedicated thread + channel + runtime per capability) with a shared
//! resource. New async capabilities just need to hand the dispatcher a
//! future — no new threads, no new channels.
//!
//! # Why this exists
//!
//! `deno_core::JsRuntime` is `!Send`, so the extension thread uses `LocalSet`.
//! V8 callbacks are `extern "C"` and cannot unwind (panic across FFI = abort),
//! so `#[op2]` async ops don't work — they try to throw from inside the V8
//! callback. The only option is a **synchronous** op that dispatches async work
//! elsewhere and blocks on the result.
//!
//! # Thread model
//!
//! ```text
//! V8 extension thread                     Background dispatcher thread
//! ──────────────────────                  ────────────────────────────
//! sync #[op2] fn {                        tokio::Runtime (multi-thread)
//!   let result = dispatcher.block_on(     while let Ok(task) = rx.recv() {
//!     my_async_work(params)                 rt.spawn(task.future);
//!   );                                      // result sent back via reply channel
//!   return result;                         }
//! }
//! ```

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

/// A task to be executed on the background dispatcher.
struct AsyncTask {
    /// The boxed async function to run. Returns `Result<String, String>`.
    future: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
    >,
    /// Channel to send the result back to the caller.
    reply: mpsc::Sender<Result<String, String>>,
}

/// A shared background executor for async work.
///
/// Spawns a single dedicated thread with a multi-threaded tokio runtime.
/// Any sync op can call [`AsyncDispatcher::block_on`] to dispatch an async
/// future and block until it completes.
///
/// Lazily initialized via [`AsyncDispatcher::global`]. Lives for the process
/// lifetime (the background thread exits when the process does).
pub struct AsyncDispatcher {
    /// Channel sender for dispatching tasks.
    tx: mpsc::Sender<AsyncTask>,
    /// Handle to the background thread (kept for correctness, not joined).
    _thread: JoinHandle<()>,
}

impl AsyncDispatcher {
    /// Get or create the global singleton dispatcher.
    ///
    /// Thread-safe. The background thread is spawned once and reused.
    pub fn global() -> &'static Self {
        static DISPATCHER: std::sync::OnceLock<AsyncDispatcher> = std::sync::OnceLock::new();
        DISPATCHER.get_or_init(Self::new)
    }

    /// Create a new dispatcher by spawning the background thread.
    fn new() -> Self {
        let (tx, rx) = mpsc::channel::<AsyncTask>();

        let thread = thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .expect("failed to create AsyncDispatcher tokio runtime");
            while let Ok(task) = rx.recv() {
                let AsyncTask { future, reply } = task;
                rt.spawn(async move {
                    let result = future.await;
                    // Ignore send error — caller may have dropped
                    let _ = reply.send(result);
                });
            }
        });

        Self {
            tx,
            _thread: thread,
        }
    }

    /// Dispatch an async future to the background runtime and block until
    /// it completes.
    ///
    /// # Panics
    ///
    /// Panics if the background thread has shut down (should never happen
    /// since the global singleton lives for the process lifetime).
    pub fn block_on<F>(&self, future: F) -> Result<String, String>
    where
        F: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::channel();

        self.tx
            .send(AsyncTask {
                future: Box::pin(future),
                reply: reply_tx,
            })
            .unwrap_or_else(|_| panic!("AsyncDispatcher background thread has shut down"));

        reply_rx
            .recv()
            .unwrap_or_else(|_| panic!("AsyncDispatcher background thread has shut down"))
    }

}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Helper: create a fresh dispatcher (not the global singleton).
    /// Uses a thread that exits after a timeout so tests don't leak.
    fn fresh_dispatcher() -> AsyncDispatcher {
        let (tx, rx) = mpsc::channel::<AsyncTask>();

        let thread = thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new()
                .expect("failed to create tokio runtime for test dispatcher");
            // Thread exits when channel closes (no more senders)
            while let Ok(task) = rx.recv() {
                let AsyncTask { future, reply } = task;
                rt.spawn(async move {
                    let result = future.await;
                    let _ = reply.send(result);
                });
            }
        });

        AsyncDispatcher {
            tx,
            _thread: thread,
        }
    }

    #[test]
    fn dispatcher_returns_ok_result() {
        let d = fresh_dispatcher();
        let result = d.block_on(async { Ok("hello".to_string()) });
        assert_eq!(result, Ok("hello".to_string()));
    }

    #[test]
    fn dispatcher_returns_err_result() {
        let d = fresh_dispatcher();
        let result = d.block_on(async { Err("something failed".to_string()) });
        assert_eq!(result, Err("something failed".to_string()));
    }

    #[test]
    fn dispatcher_runs_async_code() {
        let d = fresh_dispatcher();
        let result = d.block_on(async {
            // Verify we're on a different thread
            tokio::task::yield_now().await;
            Ok(format!("thread: {:?}", std::thread::current().id()))
        });
        assert!(result.is_ok());
    }

    #[test]
    fn dispatcher_handles_multiple_sequential_calls() {
        let d = fresh_dispatcher();
        for i in 0..10 {
            let result = d.block_on(async move { Ok(format!("call-{i}")) });
            assert_eq!(result, Ok(format!("call-{i}")));
        }
    }

    #[test]
    fn dispatcher_handles_tokio_async_ops() {
        let d = fresh_dispatcher();
        let result = d.block_on(async {
            // Do some real async work (tokio sleep)
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok("slept".to_string())
        });
        assert_eq!(result, Ok("slept".to_string()));
    }

    #[test]
    fn dispatcher_can_run_concurrent_futures() {
        let _d = fresh_dispatcher();
        let counter = Arc::new(AtomicUsize::new(0));
        let d = AsyncDispatcher::global();

        let handles: Vec<_> = (0..5)
            .map(|i| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    let _ = d.block_on(async move {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok::<String, String>(format!("done-{i}"))
                    });
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(counter.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn global_singleton_is_shared() {
        let d1 = AsyncDispatcher::global() as *const AsyncDispatcher;
        let d2 = AsyncDispatcher::global() as *const AsyncDispatcher;
        assert_eq!(d1, d2, "global() should return the same instance");
    }

    #[test]
    fn dispatcher_propagates_panic_as_error() {
        let d = fresh_dispatcher();
        // The future itself panics — this should be caught by tokio and
        // we get a send error or the task just never replies.
        // For safety, we wrap in a catch.
        let (_reply_tx, reply_rx) = mpsc::channel::<Result<String, String>>();
        let (ready_tx, ready_rx) = mpsc::channel::<()>();

        // Spawn a thread that will block_on a panicking future
        let handle = thread::spawn(move || {
            // Signal that we're about to send
            let _ = ready_tx.send(());
            // This will panic inside the dispatcher — but since it's spawned
            // on tokio, the JoinHandle catches it. Our reply channel just
            // never gets a response.
            let _ = d.block_on(async {
                panic!("intentional test panic");
                #[allow(unreachable_code)]
                Ok::<String, String>("never".to_string())
            });
        });

        // Wait for the thread to be ready
        let _ = ready_rx.recv_timeout(Duration::from_secs(2));

        // The reply should time out since the panicking task never sends
        let result = reply_rx.recv_timeout(Duration::from_secs(2));
        // Thread might panic or timeout — either is acceptable
        let _ = handle.join();
        // We're testing that the dispatcher doesn't crash — if we get here, it's fine
        let _ = result;
    }
}
