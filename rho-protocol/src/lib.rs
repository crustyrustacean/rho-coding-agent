//! Wire types, transports, and client for the rho JSON-RPC 2.0 protocol.
//!
//! This crate is the **contract** between the rho engine and every frontend —
//! the engine's RPC loop, the terminal UI, the desktop GUIs, and (future) a
//! web client. It is deliberately a leaf crate with **no workspace
//! dependencies**: only serde, `serde_json`, tokio, and anyhow. Anything that
//! needs to speak the protocol can depend on it without pulling in the agent
//! kernel.
//!
//! # Layout
//!
//! - [`types`] — typed wire structs for every method param, result, and
//!   notification. Pure serde; no domain types.
//! - [`transport`] — the [`transport::Transport`] trait plus
//!   [`transport::StdioTransport`] (newline-delimited JSON over any
//!   `BufRead`/`Write` pair). Future transports (WebSocket, TCP) implement
//!   the same trait.
//! - [`client`] — a reusable JSON-RPC client: request/response correlation
//!   with timeouts, notification dispatch, and a child-process helper for the
//!   spawn-`rho`-and-talk-stdio pattern shared by every frontend.
//!
//! Conversions *from* rho-core domain types (e.g. `From<Box<AgentResult>>`
//! for `types::AgentEndParams`) live in the `rho` crate — only the server
//! produces those, so the protocol crate stays domain-free.

pub mod client;
pub mod transport;
pub mod types;

pub use client::{ClientEvent, RhoClient, StdioChild};
pub use transport::{ReadResult, StdioTransport, Transport};
pub use types::*;
